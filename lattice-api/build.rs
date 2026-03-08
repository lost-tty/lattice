fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out_dir = std::path::PathBuf::from(std::env::var("OUT_DIR")?);
    let config = tonic_prost_build::configure()
        .build_server(true)
        .build_client(true)
        .file_descriptor_set_path(out_dir.join("lattice_api_descriptor.bin"));

    #[cfg(feature = "ffi")]
    let config = {
        // Add uniffi::Record to message types exposed in FFI
        let ffi_records = [
            "NodeStatus",
            "RootStoreRecord",
            "StoreRef",
            "StoreDetails",
            "StoreMeta",
            "PeerInfo",
            "WitnessLogEntry",
            "SignedIntention",
            "Condition",
            "CausalDeps",
            "HLC",
            "AuthorState",
            "JoinResponse",
            // Event message types (for BackendEvent unification)
            "MeshReadyEvent",
            "StoreReadyEvent",
            "JoinFailedEvent",
            "SyncResultEvent",
            "SExpr",
            "SExprList",
            "GetIntentionRequest",
        ];

        let mut config = config;
        for msg in ffi_records {
            config = config.type_attribute(msg, "#[derive(uniffi::Record)]");
        }

        // Add uniffi::Enum to enum types
        let ffi_enums = [
            "ErrorCode",
            ".lattice.daemon.v1.NodeEvent.node_event",
            ".lattice.daemon.v1.Condition.kind",
            ".lattice.daemon.v1.SExpr.value",
        ];

        for e in ffi_enums {
            config = config.type_attribute(e, "#[derive(uniffi::Enum)]");
        }

        config
    };

    config.compile_protos(&["proto/services.proto"], &["proto"])?;
    Ok(())
}
