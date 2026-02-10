fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = tonic_prost_build::configure()
        .build_server(true)
        .build_client(true);

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
            "HistoryEntry",
            "AuthorState",
            "CleanupResult",
            "JoinResponse",
            // Event message types (for BackendEvent unification)
            "MeshReadyEvent",
            "StoreReadyEvent",
            "JoinFailedEvent",
            "SyncResultEvent",
        ];
        
        let mut config = config;
        for msg in ffi_records {
            config = config.type_attribute(msg, "#[derive(uniffi::Record)]");
        }
        
        // Add uniffi::Enum to enum types
        let ffi_enums = [
            "ErrorCode",
            "node_event.NodeEvent",  // The inner oneof enum for node events
        ];
        
        for e in ffi_enums {
            config = config.type_attribute(e, "#[derive(uniffi::Enum)]");
        }
        
        config
    };

    config.compile_protos(&["proto/services.proto"], &["proto"])?;
    Ok(())
}
