//! App hosting — bundle parsing and subdomain routing.
//!
//! App definitions and bundles live in root KV stores (replicated).
//! AppManager (lattice-node) watches stores and pushes AppEvent notifications.
//! The web layer reacts to events and maintains its own routing table.
//! All app management operations go through gRPC (NodeService / DynamicStoreService).

use std::collections::HashMap;
use std::io::Read;

// ============================================================================
// App bundles (zip-based)
// ============================================================================

/// An app bundle extracted from a zip archive.
#[derive(Clone, Debug)]
pub struct AppBundle {
    pub id: String,
    pub name: String,
    pub version: String,
    pub store_type: String,
    /// Files: path → (data, mime type)
    files: HashMap<String, (Vec<u8>, String)>,
}

/// Maximum uncompressed size of a single file in the zip (16 MB).
const MAX_FILE_SIZE: u64 = 16 * 1024 * 1024;
/// Maximum total uncompressed size of all files in the zip (64 MB).
const MAX_BUNDLE_SIZE: u64 = 64 * 1024 * 1024;

impl AppBundle {
    /// Extract an app bundle from zip bytes, reading its manifest.
    pub fn from_zip(zip_bytes: &[u8]) -> Result<Self, String> {
        // Parse manifest via the shared parser in lattice-rootstore
        let (id, manifest) = lattice_rootstore::manifest::parse_bundle_manifest(zip_bytes)
            .map_err(|e| format!("{e}"))?;

        let app_section = manifest.get("app");
        let name = app_section
            .and_then(|s| s.get("name"))
            .cloned()
            .unwrap_or_default();
        let version = app_section
            .and_then(|s| s.get("version"))
            .cloned()
            .unwrap_or_default();
        let store_type = app_section
            .and_then(|s| s.get("store_type"))
            .cloned()
            .unwrap_or_default();

        // Extract files from the zip
        let cursor = std::io::Cursor::new(zip_bytes);
        let mut archive = zip::ZipArchive::new(cursor).map_err(|e| format!("Invalid zip: {e}"))?;
        let mut files = HashMap::new();
        let mut total_size: u64 = 0;

        for i in 0..archive.len() {
            let file = archive
                .by_index(i)
                .map_err(|e| format!("Zip read error: {e}"))?;
            if file.is_dir() {
                continue;
            }
            let name = file.name().to_string();

            // Read with a hard cap via take() — don't trust declared size.
            let file_limit = MAX_FILE_SIZE.min(MAX_BUNDLE_SIZE - total_size);
            let mut limited = file.take(file_limit + 1);
            let mut data = Vec::new();
            limited
                .read_to_end(&mut data)
                .map_err(|e| format!("Zip extract error: {e}"))?;
            if data.len() as u64 > file_limit {
                return Err(format!(
                    "File '{}' exceeds size limit ({} bytes max)",
                    name, MAX_FILE_SIZE
                ));
            }
            total_size += data.len() as u64;
            let mime = mime_guess::from_path(&name)
                .first_or_octet_stream()
                .to_string();
            files.insert(name, (data, mime));
        }

        Ok(Self {
            id,
            name,
            version,
            store_type,
            files,
        })
    }

    /// Get a file from the bundle.
    pub fn get(&self, path: &str) -> Option<(&[u8], &str)> {
        self.files
            .get(path)
            .map(|(data, mime)| (data.as_slice(), mime.as_str()))
    }
}

// ============================================================================
// Subdomain extraction
// ============================================================================

pub mod registry {
    /// Check that a subdomain is a valid DNS label: lowercase alphanumeric + hyphens,
    /// 1–63 chars, cannot start or end with a hyphen.
    fn is_valid_subdomain(s: &str) -> bool {
        let len = s.len();
        len >= 1
            && len <= 63
            && !s.starts_with('-')
            && !s.ends_with('-')
            && s.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    }

    /// Strip the subdomain from a host string, returning the bare host.
    /// e.g. "myapp.localhost:8080" → "localhost:8080"
    pub fn strip_subdomain(host: &str) -> Option<String> {
        let host_no_port = host.split(':').next().unwrap_or(host);
        let dot = host_no_port.find('.')?;
        let rest = &host[dot + 1..]; // includes port if present
        Some(rest.to_string())
    }

    pub fn extract_subdomain(host: &str) -> Option<&str> {
        let host_no_port = if host.starts_with('[') {
            return None;
        } else {
            host.split(':').next().unwrap_or(host)
        };
        // The subdomain is the first label. The base host is everything after.
        // We need at least 2 parts (subdomain + base), so bare "localhost"
        // or single-label hosts return None.
        let dot = host_no_port.find('.')?;
        let sub = &host_no_port[..dot];
        if sub == "www" || !is_valid_subdomain(sub) {
            None
        } else {
            Some(sub)
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_subdomain() {
        use registry::extract_subdomain;
        assert_eq!(extract_subdomain("localhost:8080"), None);
        assert_eq!(extract_subdomain("localhost"), None);
        assert_eq!(extract_subdomain("[::1]:8080"), None);
        assert_eq!(
            extract_subdomain("inventory.localhost:8080"),
            Some("inventory")
        );
        assert_eq!(extract_subdomain("inventory.localhost"), Some("inventory"));
        assert_eq!(extract_subdomain("www.localhost:8080"), None);
        assert_eq!(extract_subdomain("myapp.example.com"), Some("myapp"));
        assert_eq!(extract_subdomain("www.example.com"), None);
        // Deep hostnames
        assert_eq!(
            extract_subdomain("myapp.node1.mesh.mycompany.internal"),
            Some("myapp")
        );
        assert_eq!(
            extract_subdomain("myapp.node1.mesh.mycompany.internal:8080"),
            Some("myapp")
        );
        assert_eq!(
            extract_subdomain("inventory.lattice.example.co.uk:443"),
            Some("inventory")
        );
        // Invalid subdomains
        assert_eq!(extract_subdomain("-bad.localhost"), None);
        assert_eq!(extract_subdomain("bad-.localhost"), None);
        assert_eq!(extract_subdomain("BAD.localhost"), None);
        assert_eq!(extract_subdomain("has space.localhost"), None);
        assert_eq!(extract_subdomain("../up.localhost"), None);
        assert_eq!(extract_subdomain(".localhost"), None);
    }

    #[test]
    fn test_strip_subdomain() {
        use registry::strip_subdomain;
        assert_eq!(
            strip_subdomain("myapp.localhost:8080"),
            Some("localhost:8080".into())
        );
        assert_eq!(strip_subdomain("myapp.localhost"), Some("localhost".into()));
        assert_eq!(
            strip_subdomain("myapp.example.com"),
            Some("example.com".into())
        );
        assert_eq!(
            strip_subdomain("myapp.node1.mesh.company.internal:443"),
            Some("node1.mesh.company.internal:443".into())
        );
        assert_eq!(strip_subdomain("localhost"), None);
        assert_eq!(strip_subdomain("localhost:8080"), None);
    }

    use crate::test_utils::make_test_zip;

    #[test]
    fn from_zip_valid() {
        let zip = make_test_zip("myapp", "1.0.0");
        let bundle = AppBundle::from_zip(&zip).unwrap();
        assert_eq!(bundle.id, "myapp");
        assert_eq!(bundle.version, "1.0.0");
        assert_eq!(bundle.store_type, "core:kvstore");
        assert!(bundle.get("index.html").is_some());
    }

    #[test]
    fn from_zip_invalid() {
        assert!(AppBundle::from_zip(b"not a zip").is_err());
    }

    #[test]
    fn from_zip_oversized_file_rejected() {
        // Create a zip with a file that exceeds MAX_FILE_SIZE when decompressed.
        // Use a highly compressible payload (all zeroes) that compresses tiny
        // but expands large — simulates a zip bomb.
        let mut buf = Vec::new();
        {
            let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
            let opts = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);

            // Write a valid manifest first
            zip.start_file("manifest.toml", opts).unwrap();
            std::io::Write::write_all(
                &mut zip,
                b"[app]\nid = \"bomb\"\nname = \"Bomb\"\nversion = \"1.0\"\nstore_type = \"core:kvstore\"\n",
            )
            .unwrap();

            // Write a file that exceeds the 16 MB per-file limit
            zip.start_file("huge.bin", opts).unwrap();
            let chunk = vec![0u8; 1024 * 1024]; // 1 MB of zeroes (compresses well)
            for _ in 0..17 {
                std::io::Write::write_all(&mut zip, &chunk).unwrap();
            }
            zip.finish().unwrap();
        }
        let err = AppBundle::from_zip(&buf).unwrap_err();
        assert!(err.contains("exceeds size limit"), "got: {err}");
    }

    #[test]
    fn from_zip_total_size_exceeded() {
        // Multiple files that individually fit but together exceed MAX_BUNDLE_SIZE.
        let mut buf = Vec::new();
        {
            let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
            let opts = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);

            zip.start_file("manifest.toml", opts).unwrap();
            std::io::Write::write_all(
                &mut zip,
                b"[app]\nid = \"big\"\nname = \"Big\"\nversion = \"1.0\"\nstore_type = \"core:kvstore\"\n",
            )
            .unwrap();

            // Each file is under 16 MB but 5 x 15 MB = 75 MB > 64 MB total limit
            let chunk = vec![0u8; 1024 * 1024]; // 1 MB
            for i in 0..5 {
                zip.start_file(format!("file{i}.bin"), opts).unwrap();
                for _ in 0..15 {
                    std::io::Write::write_all(&mut zip, &chunk).unwrap();
                }
            }
            zip.finish().unwrap();
        }
        let err = AppBundle::from_zip(&buf).unwrap_err();
        assert!(err.contains("exceeds size limit"), "got: {err}");
    }

    #[test]
    fn from_zip_missing_manifest() {
        let mut buf = Vec::new();
        {
            let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
            let opts = zip::write::SimpleFileOptions::default();
            zip.start_file("index.html", opts).unwrap();
            std::io::Write::write_all(&mut zip, b"<h1>hi</h1>").unwrap();
            zip.finish().unwrap();
        }
        assert!(AppBundle::from_zip(&buf).is_err());
    }
}
