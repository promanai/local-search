use std::{fmt, str::FromStr};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use thiserror::Error;

const HEX_LENGTH: usize = 32;

/// Failure to parse a canonical 128-bit `LocalSearch` identifier.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum IdParseError {
    /// The identifier did not start with the prefix required by its type.
    #[error("invalid {kind} prefix; expected `{expected}`")]
    InvalidPrefix {
        /// Human-readable identifier kind.
        kind: &'static str,
        /// Required canonical prefix.
        expected: &'static str,
    },

    /// The hexadecimal payload did not contain exactly 128 bits.
    #[error("invalid {kind} payload length: expected {expected}, got {actual}")]
    InvalidLength {
        /// Human-readable identifier kind.
        kind: &'static str,
        /// Required number of hexadecimal bytes.
        expected: usize,
        /// Observed number of bytes.
        actual: usize,
    },

    /// The hexadecimal payload contained a non-hexadecimal byte.
    #[error("invalid {kind} hexadecimal byte at offset {offset}: {byte:#04x}")]
    InvalidHex {
        /// Human-readable identifier kind.
        kind: &'static str,
        /// Byte offset inside the hexadecimal payload.
        offset: usize,
        /// Invalid byte.
        byte: u8,
    },
}

macro_rules! define_id128 {
    ($name:ident, $kind:literal, $prefix:literal) => {
        #[doc = concat!("Strongly typed canonical ", $kind, " identifier.")]
        #[derive(Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name([u8; 16]);

        impl $name {
            /// Canonical wire prefix for this identifier type.
            pub const PREFIX: &'static str = $prefix;

            /// Creates an identifier from its opaque 128-bit representation.
            #[must_use]
            pub const fn from_bytes(bytes: [u8; 16]) -> Self {
                Self(bytes)
            }

            /// Creates an identifier from an integer using network byte order.
            #[must_use]
            pub const fn from_u128(value: u128) -> Self {
                Self(value.to_be_bytes())
            }

            /// Returns the opaque 128-bit representation.
            #[must_use]
            pub const fn as_bytes(&self) -> &[u8; 16] {
                &self.0
            }

            /// Consumes this identifier and returns its opaque representation.
            #[must_use]
            pub const fn into_bytes(self) -> [u8; 16] {
                self.0
            }

            /// Returns the integer representation using network byte order.
            #[must_use]
            pub const fn as_u128(self) -> u128 {
                u128::from_be_bytes(self.0)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(Self::PREFIX)?;
                for byte in self.0 {
                    write!(formatter, "{byte:02x}")?;
                }
                Ok(())
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(self, formatter)
            }
        }

        impl FromStr for $name {
            type Err = IdParseError;

            fn from_str(input: &str) -> Result<Self, Self::Err> {
                parse_id(input, $kind, Self::PREFIX).map(Self)
            }
        }

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                serializer.collect_str(self)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let input = String::deserialize(deserializer)?;
                input.parse().map_err(de::Error::custom)
            }
        }
    };
}

define_id128!(MachineId, "machine ID", "machine:");
define_id128!(VolumeId, "volume ID", "volume:");
define_id128!(FileId128, "file ID", "file:");
define_id128!(FileLinkId, "file-link ID", "link:");
define_id128!(DocumentId, "document ID", "document:");

/// Filesystem-neutral identity of a physical object on a volume.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct FileKey {
    /// Stable volume identity; never a drive letter.
    pub volume_id: VolumeId,
    /// Opaque filesystem object identifier.
    pub file_id: FileId128,
}

impl FileKey {
    /// Creates a physical-object identity.
    #[must_use]
    pub const fn new(volume_id: VolumeId, file_id: FileId128) -> Self {
        Self { volume_id, file_id }
    }
}

/// Identity carried by one searchable catalog-link projection.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct CatalogIdentity {
    /// Physical object identity.
    pub object_key: FileKey,
    /// Namespace-link identity.
    pub file_link_id: FileLinkId,
    /// Search projection identity.
    pub document_id: DocumentId,
}

impl CatalogIdentity {
    /// Creates a catalog identity from its independent parts.
    #[must_use]
    pub const fn new(
        object_key: FileKey,
        file_link_id: FileLinkId,
        document_id: DocumentId,
    ) -> Self {
        Self {
            object_key,
            file_link_id,
            document_id,
        }
    }
}

fn parse_id(
    input: &str,
    kind: &'static str,
    prefix: &'static str,
) -> Result<[u8; 16], IdParseError> {
    let payload = input
        .strip_prefix(prefix)
        .ok_or(IdParseError::InvalidPrefix {
            kind,
            expected: prefix,
        })?;

    if payload.len() != HEX_LENGTH {
        return Err(IdParseError::InvalidLength {
            kind,
            expected: HEX_LENGTH,
            actual: payload.len(),
        });
    }

    let encoded = payload.as_bytes();
    let mut decoded = [0_u8; 16];
    for (index, output) in decoded.iter_mut().enumerate() {
        let high_offset = index * 2;
        let low_offset = high_offset + 1;
        let high = hex_nibble(encoded[high_offset]).ok_or(IdParseError::InvalidHex {
            kind,
            offset: high_offset,
            byte: encoded[high_offset],
        })?;
        let low = hex_nibble(encoded[low_offset]).ok_or(IdParseError::InvalidHex {
            kind,
            offset: low_offset,
            byte: encoded[low_offset],
        })?;
        *output = (high << 4) | low;
    }

    Ok(decoded)
}

const fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}
