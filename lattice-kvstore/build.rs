use std::io::Result;

fn main() -> Result<()> {
    println!("cargo:rerun-if-changed=proto/");

    // Generate FileDescriptorSet for reflection
    let descriptor_path =
        std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("kv_descriptor.bin");

    let mut config = prost_build::Config::new();
    config
        .file_descriptor_set_path(&descriptor_path)
        // Use lattice_proto to link against existing messages
        .extern_path(".lattice", "::lattice_proto")
        .compile_protos(
            &["../lattice-proto/proto/kv_store.proto"],
            &["../lattice-proto/proto/"],
        )?;

    Ok(())
}
