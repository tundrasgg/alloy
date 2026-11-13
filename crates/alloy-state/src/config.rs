//! The configurations of the store implementations (ARFC 0006 section 5.2):
//! version 1 only, Protocol Buffers payloads, unknown fields ignored.

use std::path::Path;

use alloy_seam::{ConfigError, ConfigValue, Detail, SeamConfig};

/// The only version both implementations read and write.
const VERSION: u16 = 1;

/// `message NativeStoreConfig { string dir = 1; }`: the configuration of
/// `native`, for both kinds.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NativeStoreConfig {
    /// The absolute native path of an existing directory, where the store
    /// keeps its file (ARFC 0006 section 4.2). Empty is invalid.
    pub dir: String,
}

/// `message MemStoreConfig { string name = 1; uint64 fail_after = 2; }`: the
/// configuration of `mem`, for both kinds.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MemStoreConfig {
    /// The shared contents to use: 1 to 64 bytes of `[a-z0-9-]`.
    pub name: String,
    /// Above 0, the number of mutating calls that succeed on the shared
    /// contents before every later one fails with `Io`.
    pub fail_after: u64,
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

fn invalid(field: &str, detail: &str) -> ConfigError {
    ConfigError::Invalid {
        field: Detail::new(field),
        detail: Detail::new(detail),
    }
}

fn malformed(detail: &str) -> ConfigError {
    ConfigError::Malformed {
        detail: Detail::new(detail),
    }
}

impl SeamConfig for NativeStoreConfig {
    const VERSION: u16 = VERSION;
    const MIN_VERSION: u16 = VERSION;

    fn to_value(&self, version: u16) -> Result<ConfigValue, ConfigError> {
        check_version(version)?;
        let mut payload = Vec::new();
        put_string(&mut payload, 1, &self.dir);
        ConfigValue::new(version, payload)
    }

    fn from_value(value: &ConfigValue) -> Result<NativeStoreConfig, ConfigError> {
        check_version(value.version())?;
        let mut config = NativeStoreConfig::default();
        let mut reader = Reader(value.payload());
        while let Some((field, wire)) = reader.key()? {
            match (field, wire) {
                (1, LEN) => config.dir = reader.string("dir")?,
                (1, _) => return Err(malformed("field dir is not length-delimited")),
                _ => reader.skip(field, wire, 0)?,
            }
        }
        Ok(config)
    }
}

impl NativeStoreConfig {
    /// The directory, checked as far as can be without touching it.
    ///
    /// # Errors
    ///
    /// [`ConfigError::Invalid`] naming `dir` when it is empty or not
    /// absolute.
    pub(crate) fn checked_dir(&self) -> Result<&Path, ConfigError> {
        if self.dir.is_empty() {
            return Err(invalid("dir", "is missing"));
        }
        let dir = Path::new(&self.dir);
        if !dir.is_absolute() {
            return Err(invalid("dir", "is not absolute"));
        }
        Ok(dir)
    }
}

impl SeamConfig for MemStoreConfig {
    const VERSION: u16 = VERSION;
    const MIN_VERSION: u16 = VERSION;

    fn to_value(&self, version: u16) -> Result<ConfigValue, ConfigError> {
        check_version(version)?;
        let mut payload = Vec::new();
        put_string(&mut payload, 1, &self.name);
        if self.fail_after != 0 {
            payload.push(0x10);
            put_varint(&mut payload, self.fail_after);
        }
        ConfigValue::new(version, payload)
    }

    fn from_value(value: &ConfigValue) -> Result<MemStoreConfig, ConfigError> {
        check_version(value.version())?;
        let mut config = MemStoreConfig::default();
        let mut reader = Reader(value.payload());
        while let Some((field, wire)) = reader.key()? {
            match (field, wire) {
                (1, LEN) => config.name = reader.string("name")?,
                (1, _) => return Err(malformed("field name is not length-delimited")),
                (2, VARINT) => config.fail_after = reader.varint()?,
                (2, _) => return Err(malformed("field fail_after is not a varint")),
                _ => reader.skip(field, wire, 0)?,
            }
        }
        Ok(config)
    }
}

impl MemStoreConfig {
    /// # Errors
    ///
    /// [`ConfigError::Invalid`] naming `name` when it is not 1 to 64 bytes of
    /// `[a-z0-9-]`.
    pub(crate) fn check(&self) -> Result<(), ConfigError> {
        let valid = (1..=64).contains(&self.name.len())
            && self
                .name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
        if !valid {
            return Err(invalid("name", "must be 1 to 64 bytes of [a-z0-9-]"));
        }
        Ok(())
    }
}

const VARINT: u8 = 0;
const FIXED64: u8 = 1;
const LEN: u8 = 2;
const START_GROUP: u8 = 3;
const END_GROUP: u8 = 4;
const FIXED32: u8 = 5;

fn put_varint(out: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        out.push((value & 0x7F) as u8 | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

fn put_string(out: &mut Vec<u8>, field: u8, value: &str) {
    if !value.is_empty() {
        out.push(field << 3 | LEN);
        put_varint(out, value.len() as u64);
        out.extend_from_slice(value.as_bytes());
    }
}

/// A total decoder of the Protocol Buffers wire format.
struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, len: usize) -> Result<&'a [u8], ConfigError> {
        if len > self.0.len() {
            return Err(malformed("payload ends inside a field"));
        }
        let (head, tail) = self.0.split_at(len);
        self.0 = tail;
        Ok(head)
    }

    fn varint(&mut self) -> Result<u64, ConfigError> {
        let mut value = 0u64;
        for index in 0..10 {
            let byte = self.take(1)?[0];
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

    fn key(&mut self) -> Result<Option<(u32, u8)>, ConfigError> {
        if self.0.is_empty() {
            return Ok(None);
        }
        let key = self.varint()?;
        let field = key >> 3;
        if field == 0 || field >= 1 << 29 {
            return Err(malformed("field number outside 1 to 536870911"));
        }
        // `field` is below 2^29, so it fits.
        Ok(Some((field as u32, (key & 7) as u8)))
    }

    fn bytes(&mut self) -> Result<&'a [u8], ConfigError> {
        let len =
            usize::try_from(self.varint()?).map_err(|_| malformed("length beyond the payload"))?;
        self.take(len)
    }

    fn string(&mut self, field: &str) -> Result<String, ConfigError> {
        String::from_utf8(self.bytes()?.to_vec())
            .map_err(|_| malformed(&format!("field {field} is not UTF-8")))
    }

    fn skip(&mut self, field: u32, wire: u8, depth: u32) -> Result<(), ConfigError> {
        match wire {
            VARINT => self.varint().map(drop),
            FIXED64 => self.take(8).map(drop),
            LEN => self.bytes().map(drop),
            FIXED32 => self.take(4).map(drop),
            START_GROUP if depth < 64 => loop {
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
            },
            START_GROUP => Err(malformed("groups nested more than 64 deep")),
            END_GROUP => Err(malformed("group end without its start")),
            _ => Err(malformed("unknown wire type")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configs_round_trip_and_ignore_unknown_fields() {
        let native = NativeStoreConfig {
            dir: "/var/lib/alloy/state/global".to_owned(),
        };
        assert_eq!(
            NativeStoreConfig::from_value(&native.to_value(1).unwrap()),
            Ok(native)
        );
        let mem = MemStoreConfig {
            name: "node-1".to_owned(),
            fail_after: 300,
        };
        let value = mem.to_value(1).unwrap();
        assert_eq!(MemStoreConfig::from_value(&value), Ok(mem.clone()));
        let mut payload = value.payload().to_vec();
        payload.extend_from_slice(&[0x18, 0x05, 0x22, 0x01, 0xFF, 0x2B, 0x08, 0x01, 0x2C]);
        let value = ConfigValue::new(1, payload).unwrap();
        assert_eq!(MemStoreConfig::from_value(&value), Ok(mem));
    }

    #[test]
    fn missing_or_invalid_fields_name_the_field() {
        let field = |error: ConfigError| match error {
            ConfigError::Invalid { field, .. } => field.as_str().to_owned(),
            other => format!("{other:?}"),
        };
        let empty = ConfigValue::defaults(1).unwrap();
        let native = NativeStoreConfig::from_value(&empty).unwrap();
        assert_eq!(field(native.checked_dir().unwrap_err()), "dir");
        let relative = NativeStoreConfig {
            dir: "state".to_owned(),
        };
        assert_eq!(field(relative.checked_dir().unwrap_err()), "dir");
        let mem = MemStoreConfig::from_value(&empty).unwrap();
        assert_eq!(field(mem.check().unwrap_err()), "name");
        for bad in ["Upper", "a b", "x_y", &"a".repeat(65)] {
            let config = MemStoreConfig {
                name: bad.to_owned(),
                fail_after: 0,
            };
            assert_eq!(field(config.check().unwrap_err()), "name");
        }
        assert!(matches!(
            MemStoreConfig::from_value(&ConfigValue::defaults(2).unwrap()),
            Err(ConfigError::UnsupportedVersion { version: 2, .. })
        ));
    }
}
