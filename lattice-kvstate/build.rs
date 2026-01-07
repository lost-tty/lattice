use std::io::Result;

fn main() -> Result<()> {
    println!("cargo:rerun-if-changed=src/kv_schema.proto");
    println!("cargo:rerun-if-changed=../lattice-proto/proto/storage.proto");

    let mut config = prost_build::Config::new();
    // Map the Proto HLC to the existing Rust Hlc struct in lattice-proto
    config.extern_path(".lattice.storage.HLC", "::lattice_proto::storage::Hlc");

    config.compile_protos(
        &["src/kv_schema.proto"],
        &["src/", "../lattice-proto/proto"],
    )?;
    
    Ok(())
}
