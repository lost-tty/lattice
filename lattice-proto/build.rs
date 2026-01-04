use std::io::Result;

fn main() -> Result<()> {
    println!("cargo:rerun-if-changed=proto/");
    
    // Compilegeneric storage and network protos
    prost_build::compile_protos(
        &["proto/storage.proto", "proto/network.proto"],
        &["proto/"],
    )?;
    
    Ok(())
}
