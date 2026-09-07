//! Encrypted `.skwad` catalogue packages.
//!
//! The container deliberately separates a small visible routing header from an
//! encrypted ZIP payload. The ZIP contains only `manifest.json` and
//! `catalog.sqlite`; callers must construct that portable database without
//! absolute paths, embeddings, thumbnails, proxies, credentials or job data.

mod archive;
mod crypto;
mod format;
mod paths;
mod portable;

pub use archive::{build_payload, read_payload, CatalogueManifest, DecodedPayload};
pub use crypto::{
    calibrate_argon2id, generate_device_keypair, open_package, prepare_package, publish_package, rewrap_package,
    sign_prepared_package, validate_prepared_package, DeviceKeyPair, OpenCredential, PublishOptions, Recipient,
    SigningKeyPair,
};
pub use format::{
    inspect_header, Argon2Parameters, PackageHeader, PackageLimits, RecipientWrap, FORMAT_VERSION, MAGIC,
};
pub use paths::{normalize_relative_path, resolve_beneath_root};
pub use portable::{
    build_portable_catalogue, catalogue_groups, catalogue_media, catalogue_summary, validate_portable_catalogue,
    CatalogueGroup, CatalogueMedia, CatalogueSummary, PortableCatalogue,
};

#[derive(Debug, thiserror::Error)]
pub enum CatalogueError {
    #[error("invalid SKWAD package: {0}")]
    Invalid(String),
    #[error("SKWAD package exceeds the configured size limit")]
    TooLarge,
    #[error("the package signature is invalid")]
    InvalidSignature,
    #[error("no usable key wrap was found for this device")]
    NoMatchingRecipient,
    #[error("the passphrase or recipient key is incorrect")]
    DecryptionFailed,
    #[error("unsupported SKWAD format version {0}")]
    UnsupportedVersion(u16),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, CatalogueError>;
