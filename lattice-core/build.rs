use std::io::Result;

fn main() -> Result<()> {
    println!("cargo:rerun-if-changed=../proto/");
    println!("cargo:rerun-if-changed=src/store/kv_schema.proto");
    
    prost_build::compile_protos(
        &["../proto/storage.proto", "../proto/network.proto", "src/store/kv_schema.proto"],
        &["../proto/", "src/store/"],
    )?;
    
    Ok(())
}
