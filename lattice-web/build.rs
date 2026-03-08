fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out_dir = std::path::PathBuf::from(std::env::var("OUT_DIR")?);

    // Compile tunnel.proto with prost for Rust-side WsRequest/WsResponse types.
    // Also emit the binary FileDescriptorSet so it can be served to the browser.
    prost_build::Config::new()
        .file_descriptor_set_path(out_dir.join("tunnel_descriptor.bin"))
        .compile_protos(&["proto/tunnel.proto"], &["proto"])?;

    Ok(())
}
