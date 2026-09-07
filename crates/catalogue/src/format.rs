use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{CatalogueError, Result};

pub const MAGIC: &[u8; 8] = b"SKWAD\0\x02\0";
pub const FORMAT_VERSION: u16 = 2;
pub(crate) const PREFIX_LEN: usize = MAGIC.len() + 4;

#[derive(Debug, Clone, Copy)]
pub struct PackageLimits {
    pub max_header_bytes: usize,
    pub max_ciphertext_bytes: usize,
    pub max_catalogue_bytes: usize,
}

impl Default for PackageLimits {
    fn default() -> Self {
        Self {
            max_header_bytes: 1024 * 1024,
            max_ciphertext_bytes: 512 * 1024 * 1024,
            max_catalogue_bytes: 1024 * 1024 * 1024,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Argon2Parameters {
    pub memory_kib: u32,
    pub iterations: u32,
    pub parallelism: u32,
}

impl Default for Argon2Parameters {
    fn default() -> Self {
        Self {
            memory_kib: 65_536,
            iterations: 3,
            parallelism: 1,
        }
    }
}

impl Argon2Parameters {
    pub fn validate(&self) -> Result<()> {
        if self.memory_kib < 65_536 || self.memory_kib > 1_048_576 {
            return Err(CatalogueError::Invalid(
                "Argon2 memory cost must be between 64 MiB and 1 GiB".into(),
            ));
        }
        if !(1..=20).contains(&self.iterations) || !(1..=16).contains(&self.parallelism) {
            return Err(CatalogueError::Invalid("unsafe Argon2 parameters".into()));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum RecipientWrap {
    Hpke {
        key_id: String,
        encapsulated_key: String,
        ciphertext: String,
    },
    Passphrase {
        key_id: String,
        salt: String,
        nonce: String,
        ciphertext: String,
        argon2id: Argon2Parameters,
    },
}

impl RecipientWrap {
    pub fn key_id(&self) -> &str {
        match self {
            Self::Hpke { key_id, .. } | Self::Passphrase { key_id, .. } => key_id,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AuthenticatedHeader {
    pub format_version: u16,
    pub package_id: Uuid,
    pub revision_id: Uuid,
    pub payload_cipher: String,
    pub key_wrap: String,
    pub payload_nonce: String,
    pub ciphertext_length: u64,
    pub signing_key_id: String,
    pub recipients: Vec<RecipientWrap>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PackageHeader {
    #[serde(flatten)]
    pub authenticated: AuthenticatedHeader,
    pub signature: String,
}

impl PackageHeader {
    pub(crate) fn aad(&self) -> Result<Vec<u8>> {
        Ok(serde_json::to_vec(&self.authenticated)?)
    }

    /// Immutable fields bound to the payload AEAD. Recipient wraps are bound
    /// by the backend signature instead, which permits an authorised backend
    /// to add a device wrap without re-encrypting a large catalogue.
    pub(crate) fn payload_aad(&self) -> Result<Vec<u8>> {
        let mut core = self.authenticated.clone();
        core.recipients.clear();
        Ok(serde_json::to_vec(&core)?)
    }
}

pub fn inspect_header(bytes: &[u8], limits: PackageLimits) -> Result<PackageHeader> {
    let (header, _) = split_container(bytes, limits)?;
    Ok(header)
}

pub(crate) fn split_container(bytes: &[u8], limits: PackageLimits) -> Result<(PackageHeader, &[u8])> {
    if bytes.len() < PREFIX_LEN || &bytes[..MAGIC.len()] != MAGIC {
        return Err(CatalogueError::Invalid("bad file magic".into()));
    }
    let len_bytes: [u8; 4] = bytes[MAGIC.len()..PREFIX_LEN]
        .try_into()
        .map_err(|_| CatalogueError::Invalid("truncated header length".into()))?;
    let header_len = u32::from_le_bytes(len_bytes) as usize;
    if header_len == 0 || header_len > limits.max_header_bytes {
        return Err(CatalogueError::TooLarge);
    }
    let header_end = PREFIX_LEN.checked_add(header_len).ok_or(CatalogueError::TooLarge)?;
    let header_bytes = bytes
        .get(PREFIX_LEN..header_end)
        .ok_or_else(|| CatalogueError::Invalid("truncated package header".into()))?;
    let ciphertext = bytes
        .get(header_end..)
        .ok_or_else(|| CatalogueError::Invalid("truncated ciphertext".into()))?;
    if ciphertext.len() > limits.max_ciphertext_bytes {
        return Err(CatalogueError::TooLarge);
    }
    let header: PackageHeader = serde_json::from_slice(header_bytes)?;
    if header.authenticated.format_version != FORMAT_VERSION {
        return Err(CatalogueError::UnsupportedVersion(header.authenticated.format_version));
    }
    if header.authenticated.ciphertext_length != ciphertext.len() as u64 {
        return Err(CatalogueError::Invalid("ciphertext length mismatch".into()));
    }
    if header.authenticated.recipients.is_empty() {
        return Err(CatalogueError::Invalid("package has no key recipients".into()));
    }
    Ok((header, ciphertext))
}

pub(crate) fn encode_container(header: &PackageHeader, ciphertext: &[u8]) -> Result<Vec<u8>> {
    let header_bytes = serde_json::to_vec(header)?;
    let header_len: u32 = header_bytes.len().try_into().map_err(|_| CatalogueError::TooLarge)?;
    let mut out = Vec::with_capacity(PREFIX_LEN + header_bytes.len() + ciphertext.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&header_len.to_le_bytes());
    out.extend_from_slice(&header_bytes);
    out.extend_from_slice(ciphertext);
    Ok(out)
}
