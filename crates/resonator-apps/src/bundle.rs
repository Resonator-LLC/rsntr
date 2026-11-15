//! App bundles: a zip with `manifest.json` and `app.wasm`, named by the
//! SHA-256 of the zip.

use std::io::{Cursor, Read};

use sha2::{Digest, Sha256};

use crate::{AppError, Manifest};

/// The largest bundle accepted.
pub const MAX_BUNDLE_BYTES: usize = 16 << 20;
/// The largest `manifest.json` accepted.
pub const MAX_MANIFEST_BYTES: u64 = 1 << 20;
/// The largest `app.wasm` accepted, uncompressed. A JavaScript app carries
/// its engine and is about 12 MiB.
pub const MAX_WASM_BYTES: u64 = 48 << 20;

/// The id of an app: `sha256:<hex>` of its bundle.
pub fn app_id(sha256: &[u8; 32]) -> String {
    format!("sha256:{}", hex::encode(sha256))
}

/// An unpacked bundle.
#[derive(Debug, Clone)]
pub struct Bundle {
    pub manifest: Manifest,
    pub wasm: Vec<u8>,
    pub sha256: [u8; 32],
}

impl Bundle {
    /// Unpacks and checks a bundle. Other files in the zip are ignored.
    pub fn from_zip(bytes: &[u8]) -> Result<Self, AppError> {
        if bytes.len() > MAX_BUNDLE_BYTES {
            return Err(AppError::Bundle(format!(
                "larger than {} MiB",
                MAX_BUNDLE_BYTES >> 20
            )));
        }
        let sha256: [u8; 32] = Sha256::digest(bytes).into();
        let mut zip = zip::ZipArchive::new(Cursor::new(bytes))
            .map_err(|e| AppError::Bundle(format!("not a zip: {e}")))?;
        let manifest = read_entry(&mut zip, "manifest.json", MAX_MANIFEST_BYTES)?;
        let manifest = Manifest::parse(&manifest)?;
        let wasm = read_entry(&mut zip, "app.wasm", MAX_WASM_BYTES)?;
        Ok(Self {
            manifest,
            wasm,
            sha256,
        })
    }

    pub fn id(&self) -> String {
        app_id(&self.sha256)
    }
}

/// Reads one entry, refusing more than `limit` bytes whatever the zip
/// header claims.
fn read_entry(
    zip: &mut zip::ZipArchive<Cursor<&[u8]>>,
    name: &str,
    limit: u64,
) -> Result<Vec<u8>, AppError> {
    let entry = zip
        .by_name(name)
        .map_err(|_| AppError::Bundle(format!("no {name}")))?;
    let mut out = Vec::new();
    entry
        .take(limit + 1)
        .read_to_end(&mut out)
        .map_err(|e| AppError::Bundle(format!("{name}: {e}")))?;
    if out.len() as u64 > limit {
        return Err(AppError::Bundle(format!("{name} is too large")));
    }
    Ok(out)
}

#[cfg(test)]
pub(crate) mod tests {
    use std::io::Write;

    use super::*;

    pub(crate) fn zip_of(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = Cursor::new(Vec::new());
        let mut zip = zip::ZipWriter::new(&mut out);
        for (name, data) in files {
            zip.start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(data).unwrap();
        }
        zip.finish().unwrap();
        out.into_inner()
    }

    #[test]
    fn unpacks_and_names_a_bundle() {
        let bytes = zip_of(&[
            ("manifest.json", br#"{"name":"x","version":"1"}"#),
            ("app.wasm", b"\0asm"),
            ("README", b"ignored"),
        ]);
        let bundle = Bundle::from_zip(&bytes).unwrap();
        assert_eq!(bundle.wasm, b"\0asm");
        assert_eq!(bundle.manifest.name, "x");
        let expected: [u8; 32] = Sha256::digest(&bytes).into();
        assert_eq!(bundle.id(), app_id(&expected));
        assert!(bundle.id().starts_with("sha256:"));
    }

    #[test]
    fn refuses_bad_bundles() {
        assert!(Bundle::from_zip(b"not a zip").is_err());
        let no_wasm = zip_of(&[("manifest.json", br#"{"name":"x","version":"1"}"#)]);
        assert!(Bundle::from_zip(&no_wasm).is_err());
        let bad_manifest = zip_of(&[("manifest.json", b"{}"), ("app.wasm", b"")]);
        assert!(Bundle::from_zip(&bad_manifest).is_err());
    }
}
