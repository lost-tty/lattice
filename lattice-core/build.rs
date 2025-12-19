use std::io::Result;

fn main() -> Result<()> {
    prost_build::compile_protos(
        &["../proto/lattice.proto"],
        &["../proto/"],
    )?;
    Ok(())
}
