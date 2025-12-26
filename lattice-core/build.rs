use std::io::Result;

fn main() -> Result<()> {
    println!("cargo:rerun-if-changed=../proto/");
    
    prost_build::compile_protos(
        &["../proto/lattice.proto"],
        &["../proto/"],
    )?;
    Ok(())
}
