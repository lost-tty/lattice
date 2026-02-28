use std::io::Result;

fn main() -> Result<()> {
    println!("cargo:rerun-if-changed=proto/");

    // Compilegeneric storage and network protos
    // Generate FileDescriptorSet for inspection
    let descriptor_path =
        std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("lattice_descriptor.bin");

    let mut config = prost_build::Config::new();
    config
        .file_descriptor_set_path(&descriptor_path)
        .compile_protos(
            &[
                "proto/storage.proto",
                "proto/network.proto",
                "proto/kv_store.proto",
                "proto/weaver.proto",
            ],
            &["proto/"],
        )?;

    Ok(())
}
