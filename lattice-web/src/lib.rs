//! Lattice Web - WebSocket/gRPC tunnel and browser UI
//!
//! Provides a web interface that mirrors the CLI's functionality. Embeds into
//! the runtime alongside the Node and NetworkService. Exposes:
//!
//! - `GET /`          → Browser UI (single-page app)
//! - `GET /ws`        → WebSocket endpoint (gRPC tunnel)
//!
//! ## WebSocket Protocol
//!
//! The WebSocket carries binary protobuf-framed RPC messages using
//! `WsRequest`/`WsResponse` envelopes (defined in `proto/tunnel.proto`).
//! The browser uses protobufjs to encode/decode these envelopes and all
//! API types. Binary `FileDescriptorSet`s are served at `/proto/*.bin`
//! and converted to protobufjs `Root` objects client-side via
//! `Root.fromDescriptor()`.
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
mod tunnel;
mod ui;
mod web_server;

/// WebSocket tunnel envelope types (generated from proto/tunnel.proto).
mod ws_proto {
    include!(concat!(env!("OUT_DIR"), "/lattice.web.rs"));
}

/// Tunnel proto `FileDescriptorSet` (binary), served to the browser at `/proto/tunnel.bin`.
const TUNNEL_DESCRIPTOR: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/tunnel_descriptor.bin"));

pub use web_server::WebServer;

/// Returns the URL the web UI will be reachable at for a given port.
pub fn web_url(port: u16) -> String {
    format!("http://[::1]:{}", port)
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
