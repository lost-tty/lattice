//! Strong types for byte arrays
//!
//! Semantic newtypes for common fixed-size byte arrays, replacing raw `[u8; N]`.

use std::fmt;

/// Macro to define fixed-size byte arrays with strong types.
/// 
/// Args:
/// - $name: The name of the struct (e.g., Hash)
/// - $len: The size of the array (e.g., 32)
/// - $display_len: How many bytes to show in Display/Debug
/// - $suffix: String to append to display (e.g., "...")
/// - $doc: Documentation string
/// - $derives: List of traits to derive
macro_rules! define_bytes {
    ($name:ident, $len:expr, $doc:expr, [$($derives:ident),*]) => {
        #[doc = $doc]
        #[derive(Clone, Copy, serde::Serialize, serde::Deserialize, $($derives),*)]
        #[repr(transparent)] 
        pub struct $name(#[serde(with = "serde_bytes")] pub [u8; $len]);

        impl $name {
            /// Returns the inner bytes as a slice.
            pub fn as_bytes(&self) -> &[u8; $len] {
                &self.0
            }

            /// Parse from a hex string.
            pub fn from_hex(hex_str: &str) -> Result<Self, String> {
                let bytes = hex::decode(hex_str)
                    .map_err(|e| format!("invalid hex: {}", e))?;
                if bytes.len() != $len {
                    return Err(format!(
                        "expected {} hex characters, got {}",
                        $len * 2,
                        hex_str.len()
                    ));
                }
                Ok(Self(bytes.try_into().map_err(|_| "internal error: length mismatch".to_string())?))
            }
        }

        // Standard Conversions
        impl From<[u8; $len]> for $name {
            fn from(bytes: [u8; $len]) -> Self {
                Self(bytes)
            }
        }

        impl From<$name> for [u8; $len] {
            fn from(wrapper: $name) -> [u8; $len] {
                wrapper.0
            }
        }

        impl AsRef<[u8]> for $name {
            fn as_ref(&self) -> &[u8] {
                &self.0
            }
        }

        impl std::ops::Deref for $name {
            type Target = [u8; $len];
            fn deref(&self) -> &Self::Target {
                &self.0
            }
        }

        // Zero-allocation Hex formatting
        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::LowerHex::fmt(self, f)
            }
        }

        impl fmt::LowerHex for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                for byte in &self.0 {
                    write!(f, "{:02x}", byte)?;
                }
                Ok(())
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}(", stringify!($name))?;
                fmt::Display::fmt(self, f)?;
                write!(f, ")")
            }
        }

        // TryFrom for slice parsing (for from_bytes)
        impl TryFrom<&[u8]> for $name {
            type Error = std::array::TryFromSliceError;
            fn try_from(slice: &[u8]) -> Result<Self, Self::Error> {
                Ok(Self(<[u8; $len]>::try_from(slice)?))
            }
        }

        // TryFrom<Vec<u8>> for owned vector parsing
        impl TryFrom<Vec<u8>> for $name {
            type Error = Vec<u8>;
            fn try_from(vec: Vec<u8>) -> Result<Self, Self::Error> {
                if vec.len() != $len {
                    return Err(vec);
                }
                let mut arr = [0u8; $len];
                arr.copy_from_slice(&vec);
                Ok(Self(arr))
            }
        }
    };
}

// --- Type Definitions ---

define_bytes!(
    Hash, 
    32, 
    "32-byte hash (BLAKE3)", 
    [PartialEq, Eq, Hash, Default, PartialOrd, Ord]
);

impl Hash {
    pub const ZERO: Hash = Hash([0u8; 32]);
}

define_bytes!(
    PubKey, 
    32, 
    "32-byte Ed25519 public key", 
    [PartialEq, Eq, Hash, Default, PartialOrd, Ord]
);

define_bytes!(
    Signature, 
    64, 
    "64-byte Ed25519 signature", 
    [PartialEq, Eq]
);

// --- Tests ---

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hash_display() {
        let hash = Hash([0xab; 32]);
        let expected = "abababababababababababababababababababababababababababababababab";
        assert_eq!(format!("{}", hash), expected);
        assert_eq!(format!("{:?}", hash), format!("Hash({})", expected));
    }

    #[test]
    fn test_signature_display() {
        let sig = Signature([0xef; 64]);
        let expected = "ef".repeat(64);
        assert_eq!(format!("{}", sig), expected);
        assert_eq!(format!("{:?}", sig), format!("Signature({})", expected));
    }

    #[test]
    fn test_traits() {
        let bytes = [1u8; 32];
        let hash: Hash = bytes.into();
        assert_eq!(*hash, bytes); // Test Deref
        assert_eq!(hash.as_bytes(), &bytes);
    }
    
    #[test]
    fn test_from_into() {
        let bytes: [u8; 32] = [1; 32];
        let hash: Hash = bytes.into();
        let back: [u8; 32] = hash.into();
        assert_eq!(bytes, back);
    }
}
