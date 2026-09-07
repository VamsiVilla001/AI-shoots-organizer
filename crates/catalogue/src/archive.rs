use std::io::{Cursor, Read, Write};

use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;
use zip::{write::SimpleFileOptions, CompressionMethod, ZipArchive, ZipWriter};

use crate::{validate_portable_catalogue, CatalogueError, PackageLimits, Result};

const MANIFEST_NAME: &str = "manifest.json";
const CATALOGUE_NAME: &str = "catalog.sqlite";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CatalogueManifest {
    pub schema_version: u32,
    pub library_id: String,
    pub shoot_id: String,
    pub published_revision: u64,
    pub created_at: String,
    pub catalogue_blake3: String,
    pub media_count: u64,
}

#[derive(Debug)]
pub struct DecodedPayload {
    pub manifest: CatalogueManifest,
    pub catalogue: Zeroizing<Vec<u8>>,
}

pub fn build_payload(mut manifest: CatalogueManifest, catalogue: &[u8]) -> Result<Vec<u8>> {
    manifest.catalogue_blake3 = blake3::hash(catalogue).to_hex().to_string();
    let cursor = Cursor::new(Vec::new());
    let mut zip = ZipWriter::new(cursor);
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .unix_permissions(0o600);
    zip.start_file(MANIFEST_NAME, options)?;
    zip.write_all(&serde_json::to_vec(&manifest)?)?;
    zip.start_file(CATALOGUE_NAME, options)?;
    zip.write_all(catalogue)?;
    Ok(zip.finish()?.into_inner())
}

pub fn read_payload(payload: &[u8], limits: PackageLimits) -> Result<DecodedPayload> {
    let mut zip = ZipArchive::new(Cursor::new(payload))?;
    if zip.len() != 2 {
        return Err(CatalogueError::Invalid(
            "payload must contain exactly manifest.json and catalog.sqlite".into(),
        ));
    }
    let mut manifest_bytes = Vec::new();
    {
        let entry = zip.by_name(MANIFEST_NAME)?;
        if entry.size() > 1024 * 1024 {
            return Err(CatalogueError::TooLarge);
        }
        entry.take(1024 * 1024 + 1).read_to_end(&mut manifest_bytes)?;
        if manifest_bytes.len() > 1024 * 1024 {
            return Err(CatalogueError::TooLarge);
        }
    }
    let manifest: CatalogueManifest = serde_json::from_slice(&manifest_bytes)?;
    if manifest.schema_version != 1 {
        return Err(CatalogueError::Invalid(format!(
            "unsupported catalogue schema {}",
            manifest.schema_version
        )));
    }
    let mut catalogue = Vec::new();
    {
        let entry = zip.by_name(CATALOGUE_NAME)?;
        if entry.size() as usize > limits.max_catalogue_bytes {
            return Err(CatalogueError::TooLarge);
        }
        entry
            .take(limits.max_catalogue_bytes as u64 + 1)
            .read_to_end(&mut catalogue)?;
        if catalogue.len() > limits.max_catalogue_bytes {
            return Err(CatalogueError::TooLarge);
        }
    }
    if !catalogue.starts_with(b"SQLite format 3\0") {
        return Err(CatalogueError::Invalid("catalogue is not SQLite".into()));
    }
    if blake3::hash(&catalogue).to_hex().as_str() != manifest.catalogue_blake3 {
        return Err(CatalogueError::Invalid("catalogue checksum mismatch".into()));
    }
    validate_portable_catalogue(&catalogue)?;
    Ok(DecodedPayload {
        manifest,
        catalogue: Zeroizing::new(catalogue),
    })
}

impl From<zip::result::ZipError> for CatalogueError {
    fn from(value: zip::result::ZipError) -> Self {
        Self::Invalid(format!("invalid encrypted payload archive: {value}"))
    }
}
