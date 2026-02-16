//! Generated Protobuf definitions for Lattice

pub mod storage {
    include!(concat!(env!("OUT_DIR"), "/lattice.storage.rs"));

    use lattice_model::head::Head;
    use lattice_model::{PubKey, Hash, HLC};

    impl From<Hlc> for HLC {
        fn from(proto: Hlc) -> Self {
            HLC::new(proto.wall_time, proto.counter)
        }
    }

    impl From<HLC> for Hlc {
        fn from(hlc: HLC) -> Self {
            Hlc {
                wall_time: hlc.wall_time,
                counter: hlc.counter,
            }
        }
    }

    impl TryFrom<HeadInfo> for Head {
        type Error = String;

        fn try_from(proto: HeadInfo) -> Result<Self, Self::Error> {
            Ok(Head {
                value: proto.value,
                hlc: proto.hlc.ok_or("Missing HLC")?.into(),
                author: PubKey::try_from(proto.author.as_slice())
                    .map_err(|_| "Invalid author length".to_string())?,
                hash: Hash::try_from(proto.hash.as_slice())
                    .map_err(|_| "Invalid hash length".to_string())?,
                tombstone: proto.tombstone,
            })
        }
    }

    impl From<Head> for HeadInfo {
        fn from(head: Head) -> Self {
            Self {
                value: head.value,
                hlc: Some(head.hlc.into()),
                author: head.author.to_vec(),
                hash: head.hash.to_vec(),
                tombstone: head.tombstone,
            }
        }
    }
}

pub mod network {
    include!(concat!(env!("OUT_DIR"), "/lattice.network.rs"));
}

pub mod kv {
    include!(concat!(env!("OUT_DIR"), "/lattice.kv.rs"));

    impl Operation {
        /// Create a Put operation
        pub fn put(key: impl Into<Vec<u8>>, value: impl Into<Vec<u8>>) -> Self {
            Self { op_type: Some(operation::OpType::Put(PutOp { key: key.into(), value: value.into() })) }
        }
        
        /// Create a Delete operation
        pub fn delete(key: impl Into<Vec<u8>>) -> Self {
            Self { op_type: Some(operation::OpType::Delete(DeleteOp { key: key.into() })) }
        }
        
        /// Get the key affected by this operation
        pub fn key(&self) -> Option<&[u8]> {
            match &self.op_type {
                Some(operation::OpType::Put(p)) => Some(&p.key),
                Some(operation::OpType::Delete(d)) => Some(&d.key),
                None => None,
            }
        }
    }
}

pub mod weaver {
    include!(concat!(env!("OUT_DIR"), "/lattice.weaver.rs"));
}
