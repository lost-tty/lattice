//! Lattice Web — HTTP API + browser UI
//!
//! Mirrors the CLI's functionality through HTTP. Embeds into the runtime
//! alongside the Node and NetworkService. Exposes:
//!
//! - `GET /`                            → Browser UI (single-page app)
//! - `POST /rpc/{service}/{method}`     → Unary RPC (proto in/out)
//! - `POST /sse/{service}/{method}`     → Server-streaming RPC over SSE
//!
//! ## RPC protocol
//!
//! Both endpoints take the request as a raw protobuf body and dispatch it
//! through the same in-process tonic gRPC stack used by the CLI. Unary
//! responses are raw protobuf bytes; SSE responses base64-encode each
//! message in `data:` lines and signal terminal errors via `event: error`.
//! See `rpc.rs` for the wire-level details.
//!
//! Browser code uses protobufjs to encode/decode the request/response
//! types. Binary `FileDescriptorSet`s are served at `/proto/*.bin`.
//!
//! ## Embedding
//!
//! ```ignore
//! use lattice_web::WebServer;
//!
//! let web = WebServer::new(backend.clone(), app_manager.clone(), 8080);
//! tokio::spawn(web.run());
//! ```

pub mod apps;
mod rpc;
mod ui;
mod web_server;

pub use web_server::{WebRouter, WebServer};

/// Returns the URL the web UI will be reachable at for a given port.
pub fn web_url(port: u16) -> String {
    format!("http://localhost:{}", port)
}

#[cfg(test)]
pub(crate) mod test_utils {
    /// Build a minimal valid app bundle zip in memory.
    pub fn make_test_zip(id: &str, version: &str) -> Vec<u8> {
        let manifest = format!(
            "[app]\nid = \"{id}\"\nname = \"Test\"\nversion = \"{version}\"\nstore_type = \"core:kvstore\"\n"
        );
        let mut buf = Vec::new();
        {
            let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
            let opts = zip::write::SimpleFileOptions::default();
            zip.start_file("manifest.toml", opts).unwrap();
            std::io::Write::write_all(&mut zip, manifest.as_bytes()).unwrap();
            zip.start_file("index.html", opts).unwrap();
            std::io::Write::write_all(&mut zip, b"<h1>hi</h1>").unwrap();
            zip.finish().unwrap();
        }
        buf
    }
}
