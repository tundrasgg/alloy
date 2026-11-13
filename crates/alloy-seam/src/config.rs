//! Typed, versioned configuration values (ARFC 0001 clause 3.13 and section 4).

use std::fmt;

use crate::{Detail, ImplName};

/// A typed, versioned configuration value in serialized form (ARFC 0001
/// section 4): a version of 1 or more and the Protocol Buffers payload of the
/// implementation's configuration message for that version.
///
/// A value of this type always has a non-zero version and serializes to at
/// most [`ConfigValue::MAX_SERIALIZED`] bytes. It does not decode its payload:
/// [`SeamConfig::from_value`] does. Its `Debug` output never holds the payload
/// (ARFC 0001 clause 3.18).
#[derive(Clone, PartialEq, Eq)]
pub struct ConfigValue {
    version: u16,
    payload: Vec<u8>,
}

/// Writes `ConfigValue { version: <version>, len: <payload length> }`, never
/// the payload (ARFC 0001 clause 3.18).
impl fmt::Debug for ConfigValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConfigValue")
            .field("version", &self.version)
            .field("len", &self.payload.len())
            .finish()
    }
}

impl ConfigValue {
    /// The largest serialized size, in bytes: the 2-byte version and a payload
    /// of at most 65,534 bytes.
    pub const MAX_SERIALIZED: usize = 65_536;

    /// The bytes of the serialized version field.
    const VERSION_LEN: usize = 2;

    /// A value of `version` with `payload`.
    ///
    /// # Errors
    ///
    /// [`ConfigError::TooLarge`] when `2 + payload.len()` exceeds
    /// [`ConfigValue::MAX_SERIALIZED`], and otherwise
    /// [`ConfigError::InvalidVersion`] when `version` is 0. The size check
    /// comes first.
    pub fn new(version: u16, payload: Vec<u8>) -> Result<ConfigValue, ConfigError> {
        let len = Self::VERSION_LEN.saturating_add(payload.len());
        if len > Self::MAX_SERIALIZED {
            return Err(ConfigError::TooLarge { len });
        }
        if version == 0 {
            return Err(ConfigError::InvalidVersion);
        }
        Ok(ConfigValue { version, payload })
    }

    /// A value of `version` with an empty payload: every field takes its
    /// default.
    ///
    /// # Errors
    ///
    /// [`ConfigError::InvalidVersion`] when `version` is 0.
    pub fn defaults(version: u16) -> Result<ConfigValue, ConfigError> {
        ConfigValue::new(version, Vec::new())
    }

    /// The format version, 1 or more.
    pub fn version(&self) -> u16 {
        self.version
    }

    /// The Protocol Buffers payload, possibly empty.
    pub fn payload(&self) -> &[u8] {
        &self.payload
    }

    /// The ARFC 0001 section 4 encoding: the version as unsigned 16-bit little
    /// endian, then the payload. At most [`ConfigValue::MAX_SERIALIZED`] bytes.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(Self::VERSION_LEN + self.payload.len());
        bytes.extend_from_slice(&self.version.to_le_bytes());
        bytes.extend_from_slice(&self.payload);
        bytes
    }

    /// Parses the ARFC 0001 section 4 encoding. Does not decode the payload.
    /// Total: never panics.
    ///
    /// # Errors
    ///
    /// Checked in this order, the first that applies: [`ConfigError::Truncated`]
    /// for fewer than 2 bytes, [`ConfigError::TooLarge`] for more than 65,536,
    /// and [`ConfigError::InvalidVersion`] for version 0.
    pub fn from_bytes(bytes: &[u8]) -> Result<ConfigValue, ConfigError> {
        let Some((&[low, high], payload)) = bytes.split_first_chunk::<2>() else {
            return Err(ConfigError::Truncated);
        };
        if bytes.len() > Self::MAX_SERIALIZED {
            return Err(ConfigError::TooLarge { len: bytes.len() });
        }
        ConfigValue::new(u16::from_le_bytes([low, high]), payload.to_vec())
    }
}

/// An implementation's typed configuration, carried across the seam as a
/// [`ConfigValue`] (ARFC 0001 clauses 3.7 and 3.13).
///
/// The implementation's ARFC defines the payload of each version as a Protocol
/// Buffers message. Implementations must satisfy clause 3.13: they decode and
/// write every version from `MIN_VERSION` to `VERSION`, return
/// [`ConfigError::UnsupportedVersion`] outside that range, decode an empty
/// payload to their defaults, never drop a value silently, and round-trip:
/// `from_value(&c.to_value(v)?) == c` whenever `to_value(v)` succeeds.
pub trait SeamConfig: Sized {
    /// The newest version `to_value` writes and `from_value` reads.
    const VERSION: u16;
    /// The oldest version `to_value` writes and `from_value` reads.
    const MIN_VERSION: u16;

    /// Writes the configuration in format `version` (ARFC 0001 clause 3.13).
    ///
    /// # Errors
    ///
    /// [`ConfigError::UnsupportedVersion`], with `min` set to `MIN_VERSION` and
    /// `max` to `VERSION`, for a version outside that range.
    /// [`ConfigError::NotExpressible`] naming the field when format `version`
    /// cannot carry a value the configuration holds. [`ConfigError::TooLarge`]
    /// when the result would exceed [`ConfigValue::MAX_SERIALIZED`].
    fn to_value(&self, version: u16) -> Result<ConfigValue, ConfigError>;

    /// Decodes a value of any version from `MIN_VERSION` to `VERSION`. An
    /// empty payload gives the defaults.
    ///
    /// # Errors
    ///
    /// [`ConfigError::UnsupportedVersion`], with `min` set to `MIN_VERSION` and
    /// `max` to `VERSION`, for a version outside that range.
    /// [`ConfigError::Malformed`] when the payload does not decode and
    /// [`ConfigError::Invalid`] when a field's value is out of its range.
    fn from_value(value: &ConfigValue) -> Result<Self, ConfigError>;
}

/// Why a configuration value was rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    /// The serialized value would exceed [`ConfigValue::MAX_SERIALIZED`].
    #[error("configuration is {len} bytes serialized, the limit is 65536")]
    TooLarge {
        /// The serialized size, in bytes.
        len: usize,
    },
    /// The serialized value is shorter than its version field.
    #[error("configuration is shorter than its 2-byte version")]
    Truncated,
    /// The version is 0.
    #[error("configuration version 0 is not valid")]
    InvalidVersion,
    /// The version is outside the range an implementation reads or writes.
    #[error("configuration version {version} is outside {min}..={max}")]
    UnsupportedVersion {
        /// The version asked for or found.
        version: u16,
        /// The implementation's `MIN_VERSION`.
        min: u16,
        /// The implementation's `VERSION`.
        max: u16,
    },
    /// The payload is not a valid encoding of the version's message.
    #[error("configuration payload does not decode: {detail}")]
    Malformed {
        /// What failed to decode.
        detail: Detail,
    },
    /// A field decoded but its value is not acceptable.
    #[error("configuration field {field} is invalid: {detail}")]
    Invalid {
        /// The field's name.
        field: Detail,
        /// Why the value is not acceptable.
        detail: Detail,
    },
    /// `to_value` was asked for a version that cannot hold the value of
    /// `field`.
    #[error("configuration field {field} cannot be written in version {version}")]
    NotExpressible {
        /// The version asked for.
        version: u16,
        /// The field whose value that version cannot carry.
        field: Detail,
    },
}

/// An implementation name and its configuration: what control plane state
/// stores to choose one instance of a seam (ARFC 0001 section 2).
///
/// Its `Debug` output holds the configuration's version and length only.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Binding {
    /// The implementation to build.
    pub name: ImplName,
    /// The configuration to build it from.
    pub config: ConfigValue,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_checks_size_before_version() {
        let max_payload = ConfigValue::MAX_SERIALIZED - 2;
        let value = ConfigValue::new(3, vec![7; max_payload]).unwrap();
        assert_eq!(value.version(), 3);
        assert_eq!(value.payload().len(), max_payload);
        assert_eq!(
            ConfigValue::new(1, vec![0; max_payload + 1]),
            Err(ConfigError::TooLarge { len: 65_537 })
        );
        assert_eq!(
            ConfigValue::new(0, vec![0; max_payload + 1]),
            Err(ConfigError::TooLarge { len: 65_537 })
        );
        assert_eq!(
            ConfigValue::new(0, vec![1]),
            Err(ConfigError::InvalidVersion)
        );
    }

    #[test]
    fn defaults_has_an_empty_payload() {
        let value = ConfigValue::defaults(1).unwrap();
        assert_eq!((value.version(), value.payload()), (1, &[][..]));
        assert_eq!(ConfigValue::defaults(0), Err(ConfigError::InvalidVersion));
    }

    #[test]
    fn encodes_version_little_endian_then_payload() {
        let value = ConfigValue::new(0x0102, vec![0xAA, 0xBB]).unwrap();
        assert_eq!(value.to_bytes(), [0x02, 0x01, 0xAA, 0xBB]);
        assert_eq!(ConfigValue::from_bytes(&value.to_bytes()), Ok(value));
        let empty = ConfigValue::defaults(u16::MAX).unwrap();
        assert_eq!(empty.to_bytes(), [0xFF, 0xFF]);
        assert_eq!(ConfigValue::from_bytes(&[0xFF, 0xFF]), Ok(empty));
    }

    #[test]
    fn from_bytes_checks_in_the_specified_order() {
        assert_eq!(ConfigValue::from_bytes(&[]), Err(ConfigError::Truncated));
        assert_eq!(ConfigValue::from_bytes(&[1]), Err(ConfigError::Truncated));
        assert_eq!(
            ConfigValue::from_bytes(&vec![0; 65_537]),
            Err(ConfigError::TooLarge { len: 65_537 })
        );
        assert_eq!(
            ConfigValue::from_bytes(&[0, 0, 5]),
            Err(ConfigError::InvalidVersion)
        );
        let largest = ConfigValue::from_bytes(&vec![1; 65_536]).unwrap();
        assert_eq!(largest.version(), 0x0101);
        assert_eq!(largest.payload().len(), 65_534);
    }

    #[test]
    fn debug_never_shows_the_payload() {
        let value = ConfigValue::new(2, b"secret".to_vec()).unwrap();
        assert_eq!(format!("{value:?}"), "ConfigValue { version: 2, len: 6 }");
        let binding = Binding {
            name: ImplName::new("mem").unwrap(),
            config: value,
        };
        let text = format!("{binding:?}");
        assert!(!text.contains("secret") && !text.contains("115"), "{text}");
        assert!(
            text.contains("ConfigValue { version: 2, len: 6 }"),
            "{text}"
        );
    }
}
