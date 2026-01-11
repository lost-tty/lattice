use std::io::Result;

fn main() -> Result<()> {
    println!("cargo:rerun-if-changed=proto/");

    // Generate FileDescriptorSet for reflection
    let descriptor_path = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap())
        .join("log_descriptor.bin");

    let mut config = prost_build::Config::new();
    config
        .file_descriptor_set_path(&descriptor_path)
        .compile_protos(
            &["proto/log_store.proto"],
            &["proto/"],
        )?;

    Ok(())
}
