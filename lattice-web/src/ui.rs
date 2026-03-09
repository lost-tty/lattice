//! Embedded web UI - single-page application
//!
//! Static files under `static/` are embedded into the binary at compile time
//! via `rust-embed`. In debug builds, files are read from disk so you can
//! iterate on the frontend without recompiling.

#[derive(rust_embed::Embed)]
#[folder = "static/"]
pub struct StaticFiles;
