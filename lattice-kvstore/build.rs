use std::io::Result;

fn main() -> Result<()> {
    println!("cargo:rerun-if-changed=proto/");

    // Generate FileDescriptorSet for reflection
    let descriptor_path = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap())
        .join("kv_descriptor.bin");

    let mut config = prost_build::Config::new();
    config
        .file_descriptor_set_path(&descriptor_path)
        // Use lattice_proto's storage types instead of generating duplicates
        .extern_path(".lattice.storage", "::lattice_proto::storage")
        .compile_protos(
            &["proto/kv_store.proto"],
            &["proto/", "../lattice-proto/proto/"],
        )?;

    Ok(())
}
