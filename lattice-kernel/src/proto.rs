//! Protobuf types for Lattice
//!
//! Re-exports from `lattice-proto` for type unification.

// Re-export storage and network modules from lattice-proto
pub use lattice_proto::network;
pub use lattice_proto::storage;

#[cfg(test)]
mod tests {
    use super::storage::*;

    #[test]
    fn test_hlc_roundtrip() {
        let hlc = Hlc {
            wall_time: 1234567890,
            counter: 42,
        };

        let mut buf = Vec::new();
        prost::Message::encode(&hlc, &mut buf).unwrap();
        let decoded: Hlc = prost::Message::decode(&buf[..]).unwrap();

        assert_eq!(decoded.wall_time, 1234567890);
        assert_eq!(decoded.counter, 42);
    }

    #[test]
    fn test_signed_entry() {
        let signed = SignedEntry {
            entry_bytes: vec![1, 2, 3, 4],
            signature: vec![0u8; 64],
            author_id: vec![0u8; 32],
        };

        let mut buf = Vec::new();
        prost::Message::encode(&signed, &mut buf).unwrap();
        let decoded: SignedEntry = prost::Message::decode(&buf[..]).unwrap();
        assert_eq!(decoded.entry_bytes, vec![1, 2, 3, 4]);
    }
}
