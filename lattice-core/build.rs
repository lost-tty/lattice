use std::io::Result;

fn main() -> Result<()> {
    println!("cargo:rerun-if-changed=../proto/");
    
    prost_build::compile_protos(
        &["../proto/storage.proto", "../proto/network.proto", "src/store/impls/kv/schema.proto"],
        &["../proto/", "src/store/impls/kv/"],
    )?;
    
    Ok(())
}
