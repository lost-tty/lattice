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
