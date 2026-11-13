//! The configurations of the `os` implementations (ARFC 0005 section 4.2):
//! version 1 only, Protocol Buffers payloads, unknown fields ignored.

use std::path::PathBuf;

use alloy_seam::{ConfigError, ConfigValue, Detail, SeamConfig};

/// The only version the `os` implementations read and write.
const VERSION: u16 = 1;

/// `message Empty {}`: the configuration of `clock`, `net`, `rng`, and
/// `spawn`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Empty;

/// `message OsDiskIoConfig { string root = 1; }`: the configuration of
/// `disk-io`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct OsDiskIoConfig {
    /// The native directory that `RtPath` `/` names. Empty means `/` on Unix
    /// platforms.
    pub(super) root: String,
}

fn check_version(version: u16) -> Result<(), ConfigError> {
    if version == VERSION {
        Ok(())
    } else {
        Err(ConfigError::UnsupportedVersion {
            version,
            min: VERSION,
            max: VERSION,
        })
    }
}

impl SeamConfig for Empty {
    const VERSION: u16 = VERSION;
    const MIN_VERSION: u16 = VERSION;

    fn to_value(&self, version: u16) -> Result<ConfigValue, ConfigError> {
        check_version(version)?;
        ConfigValue::new(version, Vec::new())
    }

    fn from_value(value: &ConfigValue) -> Result<Empty, ConfigError> {
        check_version(value.version())?;
        let mut reader = Reader::new(value.payload());
        while let Some((field, wire)) = reader.key()? {
            reader.skip(field, wire, 0)?;
        }
        Ok(Empty)
    }
}

impl SeamConfig for OsDiskIoConfig {
    const VERSION: u16 = VERSION;
    const MIN_VERSION: u16 = VERSION;

    fn to_value(&self, version: u16) -> Result<ConfigValue, ConfigError> {
        check_version(version)?;
        let mut payload = Vec::new();
        if !self.root.is_empty() {
            payload.push(ROOT_KEY);
            push_varint(&mut payload, self.root.len() as u64);
            payload.extend_from_slice(self.root.as_bytes());
        }
        ConfigValue::new(version, payload)
    }

    fn from_value(value: &ConfigValue) -> Result<OsDiskIoConfig, ConfigError> {
        check_version(value.version())?;
        let mut reader = Reader::new(value.payload());
        let mut root = String::new();
        while let Some((field, wire)) = reader.key()? {
            if field == ROOT_FIELD {
                if wire != LEN {
                    return Err(malformed("field root is not length-delimited"));
                }
                let bytes = reader.bytes()?;
                // A repeated scalar field takes its last value.
                root = String::from_utf8(bytes.to_vec())
                    .map_err(|_| malformed("field root is not UTF-8"))?;
            } else {
                reader.skip(field, wire, 0)?;
            }
        }
        Ok(OsDiskIoConfig { root })
    }
}

/// The field number of `root`.
const ROOT_FIELD: u32 = 1;
/// The key of `root`: field 1, wire type 2.
const ROOT_KEY: u8 = 0x0A;

/// Wire types.
const VARINT: u8 = 0;
const FIXED64: u8 = 1;
const LEN: u8 = 2;
const START_GROUP: u8 = 3;
const END_GROUP: u8 = 4;
const FIXED32: u8 = 5;

/// The deepest nesting of unknown groups a payload may hold.
const MAX_GROUP_DEPTH: u32 = 64;

/// The largest field number.
const MAX_FIELD: u64 = (1 << 29) - 1;

fn malformed(detail: &str) -> ConfigError {
    ConfigError::Malformed {
        detail: Detail::new(detail),
    }
}

fn push_varint(out: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        out.push((value & 0x7F) as u8 | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

/// A total decoder of the Protocol Buffers wire format: every method returns
/// `Malformed` rather than reading past the end.
struct Reader<'a> {
    bytes: &'a [u8],
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Reader<'a> {
        Reader { bytes }
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8], ConfigError> {
        if len > self.bytes.len() {
            return Err(malformed("payload ends inside a field"));
        }
        let (head, tail) = self.bytes.split_at(len);
        self.bytes = tail;
        Ok(head)
    }

    fn varint(&mut self) -> Result<u64, ConfigError> {
        let mut value = 0u64;
        for index in 0..10 {
            let [byte] = self.take(1)? else {
                return Err(malformed("payload ends inside a varint"));
            };
            let byte = *byte;
            if index == 9 && byte > 1 {
                return Err(malformed("varint longer than 64 bits"));
            }
            value |= u64::from(byte & 0x7F) << (7 * index);
            if byte < 0x80 {
                return Ok(value);
            }
        }
        Err(malformed("varint longer than 10 bytes"))
    }

    /// The next field's number and wire type, or `None` at the end.
    fn key(&mut self) -> Result<Option<(u32, u8)>, ConfigError> {
        if self.bytes.is_empty() {
            return Ok(None);
        }
        let key = self.varint()?;
        let field = key >> 3;
        if field == 0 || field > MAX_FIELD {
            return Err(malformed("field number outside 1 to 536870911"));
        }
        let wire = (key & 7) as u8;
        // `field` is at most 2^29 - 1, so it fits.
        Ok(Some((field as u32, wire)))
    }

    fn bytes(&mut self) -> Result<&'a [u8], ConfigError> {
        let len = self.varint()?;
        let len = usize::try_from(len).map_err(|_| malformed("length beyond the payload"))?;
        self.take(len)
    }

    /// Skips the value of an unknown field, including a whole group.
    fn skip(&mut self, field: u32, wire: u8, depth: u32) -> Result<(), ConfigError> {
        match wire {
            VARINT => self.varint().map(drop),
            FIXED64 => self.take(8).map(drop),
            LEN => self.bytes().map(drop),
            FIXED32 => self.take(4).map(drop),
            START_GROUP => {
                if depth >= MAX_GROUP_DEPTH {
                    return Err(malformed("groups nested more than 64 deep"));
                }
                loop {
                    let Some((inner, inner_wire)) = self.key()? else {
                        return Err(malformed("group without its end"));
                    };
                    if inner_wire == END_GROUP {
                        return if inner == field {
                            Ok(())
                        } else {
                            Err(malformed("group ends with another field number"))
                        };
                    }
                    self.skip(inner, inner_wire, depth + 1)?;
                }
            }
            END_GROUP => Err(malformed("group end without its start")),
            _ => Err(malformed("unknown wire type")),
        }
    }
}

/// The native directory a `disk-io` instance maps `/` to (ARFC 0005 section
/// 4.2 and clause 3.1.10).
///
/// # Errors
///
/// [`ConfigError::Invalid`] naming `root` when it is empty on a platform other
/// than Unix, not absolute, or not an existing directory.
pub(super) fn resolve_root(root: &str) -> Result<PathBuf, ConfigError> {
    let invalid = |detail: &str| ConfigError::Invalid {
        field: Detail::new("root"),
        detail: Detail::new(detail),
    };
    let path = if root.is_empty() {
        if cfg!(unix) {
            PathBuf::from("/")
        } else {
            return Err(invalid("is empty, which means / on Unix platforms only"));
        }
    } else {
        PathBuf::from(root)
    };
    if !path.is_absolute() {
        return Err(invalid("is not an absolute path"));
    }
    match std::fs::metadata(&path) {
        Ok(meta) if meta.is_dir() => Ok(path),
        Ok(_) => Err(invalid("is not a directory")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(invalid("does not exist")),
        Err(e) => Err(invalid(&format!("cannot be read: {e}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value(payload: &[u8]) -> ConfigValue {
        ConfigValue::new(1, payload.to_vec()).unwrap()
    }

    #[test]
    fn empty_ignores_unknown_fields() {
        let unknown = [
            0x08, 0x96, 0x01, // field 1 varint 150
            0x11, 1, 2, 3, 4, 5, 6, 7, 8, // field 2 fixed64
            0x1A, 0x02, b'h', b'i', // field 3 bytes
            0x25, 1, 2, 3, 4, // field 4 fixed32
            0x2B, 0x08, 0x01, 0x2C, // field 5 group holding field 1
        ];
        assert_eq!(Empty::from_value(&value(&unknown)), Ok(Empty));
        assert_eq!(Empty::from_value(&value(&[])), Ok(Empty));
        assert_eq!(Empty.to_value(1), Ok(value(&[])));
    }

    #[test]
    fn malformed_payloads_are_rejected() {
        for bad in [
            &[0x08][..],         // varint missing
            &[0x0A, 0x05, b'a'], // bytes past the end
            &[0x00],             // field 0
            &[0x0E, 0x00],       // wire type 6
            &[0x2B, 0x08, 0x01], // group without its end
            &[0x2C],             // end without start
            &[0x2B, 0x34],       // group ends with field 6
            &[
                0x08, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x02,
            ],
        ] {
            assert!(
                matches!(
                    Empty::from_value(&value(bad)),
                    Err(ConfigError::Malformed { .. })
                ),
                "{bad:?}"
            );
        }
        let deep: Vec<u8> = std::iter::repeat_n(0x0B, 65).collect();
        assert!(matches!(
            Empty::from_value(&value(&deep)),
            Err(ConfigError::Malformed { .. })
        ));
    }

    #[test]
    fn disk_config_round_trips_and_takes_the_last_root() {
        let config = OsDiskIoConfig {
            root: "/var/lib/alloy".to_owned(),
        };
        let encoded = config.to_value(1).unwrap();
        assert_eq!(&encoded.payload()[..2], &[0x0A, 14]);
        assert_eq!(OsDiskIoConfig::from_value(&encoded), Ok(config));
        let twice = value(&[0x0A, 1, b'a', 0x10, 0x01, 0x0A, 1, b'b']);
        assert_eq!(
            OsDiskIoConfig::from_value(&twice).map(|c| c.root),
            Ok("b".to_owned())
        );
        let defaults = OsDiskIoConfig::from_value(&value(&[])).unwrap();
        assert_eq!(defaults.root, "");
        assert_eq!(defaults.to_value(1), Ok(value(&[])));
        assert!(matches!(
            OsDiskIoConfig::from_value(&value(&[0x08, 0x01])),
            Err(ConfigError::Malformed { .. })
        ));
        assert!(matches!(
            OsDiskIoConfig::from_value(&value(&[0x0A, 0x01, 0xFF])),
            Err(ConfigError::Malformed { .. })
        ));
        assert_eq!(
            OsDiskIoConfig::from_value(&ConfigValue::defaults(3).unwrap()),
            Err(ConfigError::UnsupportedVersion {
                version: 3,
                min: 1,
                max: 1
            })
        );
        assert_eq!(
            config_version_error(),
            Err(ConfigError::UnsupportedVersion {
                version: 2,
                min: 1,
                max: 1
            })
        );
    }

    fn config_version_error() -> Result<ConfigValue, ConfigError> {
        Empty.to_value(2)
    }

    #[test]
    fn roots_must_be_existing_absolute_directories() {
        let invalid = |r: Result<PathBuf, ConfigError>| match r {
            Err(ConfigError::Invalid { field, .. }) => field.as_str() == "root",
            _ => false,
        };
        assert!(invalid(resolve_root("relative/dir")));
        let here = std::env::current_dir().unwrap();
        assert_eq!(resolve_root(here.to_str().unwrap()), Ok(here.clone()));
        let missing = here.join("no-such-directory-of-alloy-rt");
        assert!(invalid(resolve_root(missing.to_str().unwrap())));
        let file = here.join("Cargo.toml");
        assert!(invalid(resolve_root(file.to_str().unwrap())));
        if cfg!(unix) {
            assert_eq!(resolve_root(""), Ok(PathBuf::from("/")));
        } else {
            assert!(invalid(resolve_root("")));
        }
    }
}
