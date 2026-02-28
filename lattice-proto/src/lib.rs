//! Generated Protobuf definitions for Lattice

pub mod storage {
    include!(concat!(env!("OUT_DIR"), "/lattice.storage.rs"));

    use lattice_model::HLC;

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
}

pub mod network {
    include!(concat!(env!("OUT_DIR"), "/lattice.network.rs"));
}

pub mod kv {
    include!(concat!(env!("OUT_DIR"), "/lattice.kv.rs"));

    impl Operation {
        /// Create a Put operation
        pub fn put(key: impl Into<Vec<u8>>, value: impl Into<Vec<u8>>) -> Self {
            Self {
                op_type: Some(operation::OpType::Put(PutOp {
                    key: key.into(),
                    value: value.into(),
                })),
            }
        }

        /// Create a Delete operation
        pub fn delete(key: impl Into<Vec<u8>>) -> Self {
            Self {
                op_type: Some(operation::OpType::Delete(DeleteOp { key: key.into() })),
            }
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

pub mod convert;
