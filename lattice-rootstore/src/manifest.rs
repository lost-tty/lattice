//! Bundle manifest parsing.
//!
//! Reads `manifest.toml` from a zip archive and returns a generic
//! section → key-value representation. Both the rootstore (proto conversion)
//! and the web server (field extraction) use this as the single source of truth.

use std::collections::BTreeMap;
use std::io::Read;

/// Parsed bundle manifest — section name → (key → value).
pub type Manifest = BTreeMap<String, BTreeMap<String, String>>;

/// Maximum manifest.toml size (64 KB). Anything larger is rejected.
const MAX_MANIFEST_SIZE: u64 = 64 * 1024;

/// Parse `manifest.toml` from a zip archive.
///
/// Returns `(app_id, manifest)` where `manifest` is a generic map of
/// TOML section headers to key-value pairs. Validates that `[app].id`
/// is present and non-empty.
pub fn parse_bundle_manifest(
    zip_bytes: &[u8],
) -> Result<(String, Manifest), Box<dyn std::error::Error + Send + Sync>> {
    let cursor = std::io::Cursor::new(zip_bytes);
    let mut archive = zip::ZipArchive::new(cursor)?;

    let manifest_file = archive
        .by_name("manifest.toml")
        .map_err(|_| "Missing manifest.toml in app bundle")?;

    // Read with a hard cap — don't trust declared size.
    let mut manifest_str = String::new();
    manifest_file
        .take(MAX_MANIFEST_SIZE + 1)
        .read_to_string(&mut manifest_str)?;
    if manifest_str.len() as u64 > MAX_MANIFEST_SIZE {
        return Err("manifest.toml exceeds 64 KB size limit".into());
    }

    let toml_value: toml::Table = toml::from_str(&manifest_str)?;

    let mut manifest = Manifest::new();
    for (section_name, section_value) in &toml_value {
        if let toml::Value::Table(table) = section_value {
            let entries: BTreeMap<String, String> = table
                .iter()
                .map(|(key, value)| {
                    let string_value = match value {
                        toml::Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    (key.clone(), string_value)
                })
                .collect();
            manifest.insert(section_name.clone(), entries);
        }
    }

    let app_id = manifest
        .get("app")
        .and_then(|s| s.get("id"))
        .filter(|id| !id.is_empty())
        .ok_or("manifest.toml: app.id is missing or empty")?
        .clone();

    Ok((app_id, manifest))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn make_zip_with_manifest(manifest: &str) -> Vec<u8> {
        let mut buf = Vec::new();
        {
            let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
            let opts = zip::write::SimpleFileOptions::default();
            zip.start_file("manifest.toml", opts).unwrap();
            zip.write_all(manifest.as_bytes()).unwrap();
            zip.finish().unwrap();
        }
        buf
    }

    #[test]
    fn valid_manifest() {
        let zip = make_zip_with_manifest(
            "[app]\nid = \"myapp\"\nname = \"My App\"\nversion = \"1.0\"\nstore_type = \"core:kvstore\"\n",
        );
        let (app_id, manifest) = parse_bundle_manifest(&zip).unwrap();
        assert_eq!(app_id, "myapp");
        assert_eq!(manifest["app"]["name"], "My App");
        assert_eq!(manifest["app"]["version"], "1.0");
        assert_eq!(manifest["app"]["store_type"], "core:kvstore");
    }

    #[test]
    fn missing_manifest_toml() {
        let mut buf = Vec::new();
        {
            let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
            let opts = zip::write::SimpleFileOptions::default();
            zip.start_file("index.html", opts).unwrap();
            zip.write_all(b"<h1>hi</h1>").unwrap();
            zip.finish().unwrap();
        }
        let err = parse_bundle_manifest(&buf).unwrap_err();
        assert!(err.to_string().contains("Missing manifest.toml"));
    }

    #[test]
    fn missing_app_id() {
        let zip = make_zip_with_manifest("[app]\nname = \"No ID\"\n");
        let err = parse_bundle_manifest(&zip).unwrap_err();
        assert!(err.to_string().contains("app.id"));
    }

    #[test]
    fn empty_app_id() {
        let zip = make_zip_with_manifest("[app]\nid = \"\"\nname = \"Empty\"\n");
        let err = parse_bundle_manifest(&zip).unwrap_err();
        assert!(err.to_string().contains("app.id"));
    }

    #[test]
    fn invalid_toml() {
        let zip = make_zip_with_manifest("this is not valid toml {{{}");
        assert!(parse_bundle_manifest(&zip).is_err());
    }

    #[test]
    fn not_a_zip() {
        assert!(parse_bundle_manifest(b"not a zip").is_err());
    }

    #[test]
    fn multiple_sections() {
        let zip = make_zip_with_manifest(
            "[app]\nid = \"myapp\"\nname = \"My App\"\nversion = \"1.0\"\nstore_type = \"core:kvstore\"\n\n[permissions]\nread = \"true\"\nwrite = \"false\"\n",
        );
        let (_, manifest) = parse_bundle_manifest(&zip).unwrap();
        assert_eq!(manifest["permissions"]["read"], "true");
        assert_eq!(manifest["permissions"]["write"], "false");
    }

    #[test]
    fn oversized_manifest_rejected() {
        // 64 KB + 1 byte should be rejected
        let big = "a".repeat(MAX_MANIFEST_SIZE as usize + 1);
        let zip = make_zip_with_manifest(&format!("[app]\nid = \"x\"\ndata = \"{big}\"\n"));
        assert!(parse_bundle_manifest(&zip).is_err());
    }
}
