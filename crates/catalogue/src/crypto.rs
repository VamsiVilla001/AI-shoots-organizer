use argon2::{Algorithm, Argon2, Params, Version};
use base64::{engine::general_purpose::STANDARD_NO_PAD as B64, Engine};
use chacha20poly1305::{
    aead::{Aead, KeyInit, Payload},
    ChaCha20Poly1305, Nonce, XChaCha20Poly1305, XNonce,
};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use hpke::{
    aead::ChaCha20Poly1305 as HpkeChaCha20Poly1305, kdf::HkdfSha256, kem::X25519HkdfSha256, Deserializable, Kem as _,
    OpModeR, OpModeS, Serializable,
};
use rand::{rngs::StdRng, RngCore, SeedableRng};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use zeroize::{Zeroize, Zeroizing};

use crate::{
    archive::{read_payload, DecodedPayload},
    format::{
        encode_container, split_container, Argon2Parameters, AuthenticatedHeader, PackageHeader, PackageLimits,
        RecipientWrap, FORMAT_VERSION,
    },
    CatalogueError, Result,
};

const PAYLOAD_CIPHER: &str = "XChaCha20-Poly1305";
const KEY_WRAP: &str = "HPKE-X25519-HKDF-SHA256-ChaCha20Poly1305+Argon2id";
const HPKE_INFO: &[u8] = b"SKWAD package data key v2";
const WRAP_AAD_PREFIX: &[u8] = b"SKWAD wrapped data key v2\0";
const SIGNING_DOMAIN: &[u8] = b"SKWAD signed package v2\0";

type Kem = X25519HkdfSha256;
type Kdf = HkdfSha256;
type HpkeAead = HpkeChaCha20Poly1305;

#[derive(Clone, Zeroize)]
#[zeroize(drop)]
pub struct DeviceKeyPair {
    pub key_id: String,
    private_key: Vec<u8>,
    public_key: Vec<u8>,
}

impl DeviceKeyPair {
    pub fn from_bytes(key_id: impl Into<String>, private_key: Vec<u8>, public_key: Vec<u8>) -> Result<Self> {
        let _ = <Kem as hpke::Kem>::PrivateKey::from_bytes(&private_key)
            .map_err(|_| CatalogueError::Invalid("invalid X25519 private key".into()))?;
        let _ = <Kem as hpke::Kem>::PublicKey::from_bytes(&public_key)
            .map_err(|_| CatalogueError::Invalid("invalid X25519 public key".into()))?;
        Ok(Self {
            key_id: key_id.into(),
            private_key,
            public_key,
        })
    }

    pub fn private_key_bytes(&self) -> &[u8] {
        &self.private_key
    }

    pub fn public_key_bytes(&self) -> &[u8] {
        &self.public_key
    }
}

pub fn generate_device_keypair(key_id: impl Into<String>) -> DeviceKeyPair {
    let mut rng = StdRng::from_os_rng();
    let (private_key, public_key) = Kem::gen_keypair(&mut rng);
    DeviceKeyPair {
        key_id: key_id.into(),
        private_key: private_key.to_bytes().to_vec(),
        public_key: public_key.to_bytes().to_vec(),
    }
}

#[derive(Debug, Clone)]
pub struct Recipient {
    pub key_id: String,
    pub public_key: Vec<u8>,
}

#[derive(Clone, Zeroize)]
#[zeroize(drop)]
pub struct SigningKeyPair {
    pub key_id: String,
    secret: [u8; 32],
}

impl SigningKeyPair {
    pub fn generate(key_id: impl Into<String>) -> Self {
        let mut secret = [0_u8; 32];
        StdRng::from_os_rng().fill_bytes(&mut secret);
        Self {
            key_id: key_id.into(),
            secret,
        }
    }

    pub fn from_bytes(key_id: impl Into<String>, secret: [u8; 32]) -> Self {
        Self {
            key_id: key_id.into(),
            secret,
        }
    }

    pub fn secret_bytes(&self) -> &[u8; 32] {
        &self.secret
    }

    pub fn verifying_key_bytes(&self) -> [u8; 32] {
        SigningKey::from_bytes(&self.secret).verifying_key().to_bytes()
    }
}

#[derive(Debug)]
pub struct PublishOptions<'a> {
    pub package_id: Uuid,
    pub revision_id: Uuid,
    pub recipients: &'a [Recipient],
    pub passphrase: Option<&'a str>,
    pub passphrase_key_id: &'a str,
    pub argon2: Argon2Parameters,
}

pub enum OpenCredential<'a> {
    Device(&'a DeviceKeyPair),
    Passphrase(&'a str),
}

/// Measures a minimum-memory Argon2id pass and selects an iteration count near
/// the requested wall time. Parameters remain bounded by header validation.
pub fn calibrate_argon2id(target_millis: u64) -> Argon2Parameters {
    let base = Argon2Parameters {
        memory_kib: 65_536,
        iterations: 1,
        parallelism: 1,
    };
    let started = std::time::Instant::now();
    let _ = derive_passphrase_key("SKWAD calibration only", &[0x5a; 16], &base);
    let elapsed = started.elapsed().as_millis().max(1) as u64;
    let iterations = target_millis.max(1).div_ceil(elapsed).clamp(1, 10) as u32;
    Argon2Parameters { iterations, ..base }
}

pub fn publish_package(payload: &[u8], options: PublishOptions<'_>, signer: &SigningKeyPair) -> Result<Vec<u8>> {
    let unsigned = prepare_package(payload, options, &signer.key_id)?;
    sign_prepared_package(&unsigned, signer)
}

/// Encrypts and wraps a package without possessing the backend signing key.
/// The resulting container has an empty signature and must be sent to the
/// SKWAD backend's signing endpoint before it can be imported.
pub fn prepare_package(payload: &[u8], options: PublishOptions<'_>, signing_key_id: &str) -> Result<Vec<u8>> {
    options.argon2.validate()?;
    if options.recipients.is_empty() && options.passphrase.is_none() {
        return Err(CatalogueError::Invalid("at least one recipient is required".into()));
    }

    let mut data_key = Zeroizing::new([0_u8; 32]);
    StdRng::from_os_rng().fill_bytes(data_key.as_mut());
    let mut recipients = Vec::with_capacity(options.recipients.len() + usize::from(options.passphrase.is_some()));
    for recipient in options.recipients {
        recipients.push(wrap_for_recipient(&data_key[..], recipient)?);
    }
    if let Some(passphrase) = options.passphrase {
        if passphrase.chars().count() < 12 {
            return Err(CatalogueError::Invalid(
                "offline passphrase must contain at least 12 characters".into(),
            ));
        }
        recipients.push(wrap_for_passphrase(
            &data_key[..],
            passphrase,
            options.passphrase_key_id,
            options.argon2,
        )?);
    }

    let mut payload_nonce = [0_u8; 24];
    StdRng::from_os_rng().fill_bytes(&mut payload_nonce);
    let ciphertext_length = payload.len().checked_add(16).ok_or(CatalogueError::TooLarge)? as u64;
    let authenticated = AuthenticatedHeader {
        format_version: FORMAT_VERSION,
        package_id: options.package_id,
        revision_id: options.revision_id,
        payload_cipher: PAYLOAD_CIPHER.into(),
        key_wrap: KEY_WRAP.into(),
        payload_nonce: B64.encode(payload_nonce),
        ciphertext_length,
        signing_key_id: signing_key_id.to_owned(),
        recipients,
    };
    let header = PackageHeader {
        authenticated,
        signature: String::new(),
    };
    let aad = header.payload_aad()?;
    let cipher = XChaCha20Poly1305::new_from_slice(&data_key[..])
        .map_err(|_| CatalogueError::Invalid("invalid package key".into()))?;
    let ciphertext = cipher
        .encrypt(
            XNonce::from_slice(&payload_nonce),
            Payload {
                msg: payload,
                aad: &aad,
            },
        )
        .map_err(|_| CatalogueError::DecryptionFailed)?;

    encode_container(&header, &ciphertext)
}

/// Validates an unsigned encrypted container and attaches a backend signature.
/// This function is intended for the backend service, not the desktop client.
pub fn sign_prepared_package(bytes: &[u8], signer: &SigningKeyPair) -> Result<Vec<u8>> {
    let limits = PackageLimits::default();
    let (mut header, ciphertext) = split_container(bytes, limits)?;
    validate_algorithms(&header)?;
    if !header.signature.is_empty() || header.authenticated.signing_key_id != signer.key_id {
        return Err(CatalogueError::Invalid("invalid unsigned package signing key".into()));
    }
    let aad = header.aad()?;
    let mut signed = Vec::with_capacity(SIGNING_DOMAIN.len() + aad.len() + ciphertext.len());
    signed.extend_from_slice(SIGNING_DOMAIN);
    signed.extend_from_slice(&aad);
    signed.extend_from_slice(ciphertext);
    let signature = SigningKey::from_bytes(&signer.secret).sign(&signed);
    signed.zeroize();
    header.signature = B64.encode(signature.to_bytes());
    encode_container(&header, ciphertext)
}

/// Backend-side validation before signing. This proves the encrypted package
/// contains a valid allow-listed portable catalogue and that the backend has a
/// recipient wrap for disaster recovery/future authorised rewrapping.
pub fn validate_prepared_package(bytes: &[u8], backend_device: &DeviceKeyPair) -> Result<DecodedPayload> {
    let (header, ciphertext) = split_container(bytes, PackageLimits::default())?;
    validate_algorithms(&header)?;
    if !header.signature.is_empty() {
        return Err(CatalogueError::Invalid("package is already signed".into()));
    }
    let data_key = Zeroizing::new(unwrap_for_device(&header, backend_device)?);
    let nonce = decode_fixed::<24>(&header.authenticated.payload_nonce, "payload nonce")?;
    let aad = header.payload_aad()?;
    let cipher = XChaCha20Poly1305::new_from_slice(&data_key[..]).map_err(|_| CatalogueError::DecryptionFailed)?;
    let plaintext = Zeroizing::new(
        cipher
            .decrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: ciphertext,
                    aad: &aad,
                },
            )
            .map_err(|_| CatalogueError::DecryptionFailed)?,
    );
    read_payload(&plaintext, PackageLimits::default())
}

/// Adds a new device key wrap without decrypting or re-encrypting the payload.
/// The backend recipient must already be present and the backend re-signs the
/// changed routing header.
pub fn rewrap_package(
    bytes: &[u8],
    backend_device: &DeviceKeyPair,
    new_recipient: Recipient,
    signer: &SigningKeyPair,
    verifying_key: &[u8; 32],
) -> Result<Vec<u8>> {
    let (mut header, ciphertext) = split_container(bytes, PackageLimits::default())?;
    validate_algorithms(&header)?;
    let old_aad = header.aad()?;
    verify_signature(&header, &old_aad, ciphertext, verifying_key)?;
    if header
        .authenticated
        .recipients
        .iter()
        .any(|wrap| wrap.key_id() == new_recipient.key_id)
    {
        return Err(CatalogueError::Invalid("recipient already has a key wrap".into()));
    }
    let data_key = Zeroizing::new(unwrap_for_device(&header, backend_device)?);
    header
        .authenticated
        .recipients
        .push(wrap_for_recipient(&data_key, &new_recipient)?);
    header.signature.clear();
    let unsigned = encode_container(&header, ciphertext)?;
    sign_prepared_package(&unsigned, signer)
}

pub fn open_package(
    bytes: &[u8],
    credential: OpenCredential<'_>,
    verifying_key: &[u8; 32],
    limits: PackageLimits,
) -> Result<DecodedPayload> {
    let (header, ciphertext) = split_container(bytes, limits)?;
    validate_algorithms(&header)?;
    let signature_aad = header.aad()?;
    verify_signature(&header, &signature_aad, ciphertext, verifying_key)?;
    let payload_aad = header.payload_aad()?;

    let mut data_key = Zeroizing::new(match credential {
        OpenCredential::Device(device) => unwrap_for_device(&header, device)?,
        OpenCredential::Passphrase(passphrase) => unwrap_for_passphrase(&header, passphrase)?,
    });
    let nonce = decode_fixed::<24>(&header.authenticated.payload_nonce, "payload nonce")?;
    let cipher = XChaCha20Poly1305::new_from_slice(&data_key[..])
        .map_err(|_| CatalogueError::Invalid("invalid package key".into()))?;
    let plaintext = Zeroizing::new(
        cipher
            .decrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: ciphertext,
                    aad: &payload_aad,
                },
            )
            .map_err(|_| CatalogueError::DecryptionFailed)?,
    );
    data_key.zeroize();
    read_payload(&plaintext, limits)
}

fn validate_algorithms(header: &PackageHeader) -> Result<()> {
    if header.authenticated.payload_cipher != PAYLOAD_CIPHER || header.authenticated.key_wrap != KEY_WRAP {
        return Err(CatalogueError::Invalid(
            "unsupported cryptographic algorithm suite".into(),
        ));
    }
    Ok(())
}

fn verify_signature(header: &PackageHeader, aad: &[u8], ciphertext: &[u8], verifying_key: &[u8; 32]) -> Result<()> {
    let verifying_key = VerifyingKey::from_bytes(verifying_key).map_err(|_| CatalogueError::InvalidSignature)?;
    let signature_bytes = decode_fixed::<64>(&header.signature, "signature")?;
    let signature = Signature::from_bytes(&signature_bytes);
    let mut signed = Vec::with_capacity(SIGNING_DOMAIN.len() + aad.len() + ciphertext.len());
    signed.extend_from_slice(SIGNING_DOMAIN);
    signed.extend_from_slice(aad);
    signed.extend_from_slice(ciphertext);
    let result = verifying_key
        .verify(&signed, &signature)
        .map_err(|_| CatalogueError::InvalidSignature);
    signed.zeroize();
    result
}

fn wrap_aad(key_id: &str) -> Vec<u8> {
    let mut aad = Vec::with_capacity(WRAP_AAD_PREFIX.len() + key_id.len());
    aad.extend_from_slice(WRAP_AAD_PREFIX);
    aad.extend_from_slice(key_id.as_bytes());
    aad
}

fn wrap_for_recipient(data_key: &[u8], recipient: &Recipient) -> Result<RecipientWrap> {
    let public = <Kem as hpke::Kem>::PublicKey::from_bytes(&recipient.public_key)
        .map_err(|_| CatalogueError::Invalid("invalid recipient public key".into()))?;
    let (encapped, mut context) =
        hpke::setup_sender::<HpkeAead, Kdf, Kem, _>(&OpModeS::Base, &public, HPKE_INFO, &mut StdRng::from_os_rng())
            .map_err(|_| CatalogueError::Invalid("HPKE setup failed".into()))?;
    let ciphertext = context
        .seal(data_key, &wrap_aad(&recipient.key_id))
        .map_err(|_| CatalogueError::Invalid("HPKE key wrap failed".into()))?;
    Ok(RecipientWrap::Hpke {
        key_id: recipient.key_id.clone(),
        encapsulated_key: B64.encode(encapped.to_bytes()),
        ciphertext: B64.encode(ciphertext),
    })
}

fn unwrap_for_device(header: &PackageHeader, device: &DeviceKeyPair) -> Result<Vec<u8>> {
    let private = <Kem as hpke::Kem>::PrivateKey::from_bytes(&device.private_key)
        .map_err(|_| CatalogueError::DecryptionFailed)?;
    for wrap in &header.authenticated.recipients {
        let RecipientWrap::Hpke {
            key_id,
            encapsulated_key,
            ciphertext,
        } = wrap
        else {
            continue;
        };
        if key_id != &device.key_id {
            continue;
        }
        let encapped = <Kem as hpke::Kem>::EncappedKey::from_bytes(
            &B64.decode(encapsulated_key)
                .map_err(|_| CatalogueError::DecryptionFailed)?,
        )
        .map_err(|_| CatalogueError::DecryptionFailed)?;
        let mut context = hpke::setup_receiver::<HpkeAead, Kdf, Kem>(&OpModeR::Base, &private, &encapped, HPKE_INFO)
            .map_err(|_| CatalogueError::DecryptionFailed)?;
        let wrapped = B64.decode(ciphertext).map_err(|_| CatalogueError::DecryptionFailed)?;
        let key = context
            .open(&wrapped, &wrap_aad(key_id))
            .map_err(|_| CatalogueError::DecryptionFailed)?;
        if key.len() == 32 {
            return Ok(key);
        }
    }
    Err(CatalogueError::NoMatchingRecipient)
}

fn wrap_for_passphrase(
    data_key: &[u8],
    passphrase: &str,
    key_id: &str,
    params: Argon2Parameters,
) -> Result<RecipientWrap> {
    let mut salt = [0_u8; 16];
    let mut nonce = [0_u8; 12];
    let mut rng = StdRng::from_os_rng();
    rng.fill_bytes(&mut salt);
    rng.fill_bytes(&mut nonce);
    let wrapping_key = derive_passphrase_key(passphrase, &salt, &params)?;
    let cipher = ChaCha20Poly1305::new_from_slice(&wrapping_key[..])
        .map_err(|_| CatalogueError::Invalid("invalid wrapping key".into()))?;
    let ciphertext = cipher
        .encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: data_key,
                aad: &wrap_aad(key_id),
            },
        )
        .map_err(|_| CatalogueError::DecryptionFailed)?;
    Ok(RecipientWrap::Passphrase {
        key_id: key_id.into(),
        salt: B64.encode(salt),
        nonce: B64.encode(nonce),
        ciphertext: B64.encode(ciphertext),
        argon2id: params,
    })
}

fn unwrap_for_passphrase(header: &PackageHeader, passphrase: &str) -> Result<Vec<u8>> {
    for wrap in &header.authenticated.recipients {
        let RecipientWrap::Passphrase {
            key_id,
            salt,
            nonce,
            ciphertext,
            argon2id,
        } = wrap
        else {
            continue;
        };
        argon2id.validate()?;
        let salt = decode_fixed::<16>(salt, "Argon2 salt")?;
        let nonce = decode_fixed::<12>(nonce, "wrap nonce")?;
        let key = derive_passphrase_key(passphrase, &salt, argon2id)?;
        let cipher = ChaCha20Poly1305::new_from_slice(&key[..]).map_err(|_| CatalogueError::DecryptionFailed)?;
        let wrapped = B64.decode(ciphertext).map_err(|_| CatalogueError::DecryptionFailed)?;
        let result = cipher.decrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: &wrapped,
                aad: &wrap_aad(key_id),
            },
        );
        if let Ok(data_key) = result {
            if data_key.len() == 32 {
                return Ok(data_key);
            }
        }
    }
    Err(CatalogueError::DecryptionFailed)
}

fn derive_passphrase_key(passphrase: &str, salt: &[u8; 16], params: &Argon2Parameters) -> Result<Zeroizing<[u8; 32]>> {
    params.validate()?;
    let params = Params::new(params.memory_kib, params.iterations, params.parallelism, Some(32))
        .map_err(|_| CatalogueError::Invalid("invalid Argon2 parameters".into()))?;
    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut output = Zeroizing::new([0_u8; 32]);
    argon2
        .hash_password_into(passphrase.as_bytes(), salt, output.as_mut())
        .map_err(|_| CatalogueError::DecryptionFailed)?;
    Ok(output)
}

fn decode_fixed<const N: usize>(encoded: &str, label: &str) -> Result<[u8; N]> {
    let bytes = B64
        .decode(encoded)
        .map_err(|_| CatalogueError::Invalid(format!("invalid {label}")))?;
    bytes
        .try_into()
        .map_err(|_| CatalogueError::Invalid(format!("invalid {label} length")))
}

#[derive(Debug, Serialize, Deserialize)]
struct _EnsureSerdeStaysLinked;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{build_payload, inspect_header, CatalogueManifest};

    fn sqlite_bytes() -> Vec<u8> {
        let connection = rusqlite::Connection::open_in_memory().unwrap();
        connection
            .execute_batch(super::super::portable::PORTABLE_SCHEMA)
            .unwrap();
        connection.serialize("main").unwrap().to_vec()
    }

    fn payload() -> Vec<u8> {
        build_payload(
            CatalogueManifest {
                schema_version: 1,
                library_id: Uuid::new_v4().to_string(),
                shoot_id: Uuid::new_v4().to_string(),
                published_revision: 1,
                created_at: "2026-09-05T00:00:00Z".into(),
                catalogue_blake3: String::new(),
                media_count: 1,
            },
            &sqlite_bytes(),
        )
        .unwrap()
    }

    fn fast_argon2() -> Argon2Parameters {
        // The production validator deliberately enforces >=64 MiB. A single
        // iteration keeps unit tests tolerable while exercising that boundary.
        Argon2Parameters {
            memory_kib: 65_536,
            iterations: 1,
            parallelism: 1,
        }
    }

    #[test]
    fn device_and_passphrase_both_open_the_same_package() {
        let device = generate_device_keypair("device-a");
        let signer = SigningKeyPair::generate("backend-dev");
        let bytes = publish_package(
            &payload(),
            PublishOptions {
                package_id: Uuid::new_v4(),
                revision_id: Uuid::new_v4(),
                recipients: &[Recipient {
                    key_id: device.key_id.clone(),
                    public_key: device.public_key.clone(),
                }],
                passphrase: Some("a long offline passphrase"),
                passphrase_key_id: "offline-owner",
                argon2: fast_argon2(),
            },
            &signer,
        )
        .unwrap();
        assert_eq!(
            open_package(
                &bytes,
                OpenCredential::Device(&device),
                &signer.verifying_key_bytes(),
                PackageLimits::default()
            )
            .unwrap()
            .manifest
            .media_count,
            1
        );
        assert!(open_package(
            &bytes,
            OpenCredential::Passphrase("a long offline passphrase"),
            &signer.verifying_key_bytes(),
            PackageLimits::default()
        )
        .is_ok());
    }

    #[test]
    fn wrong_passphrase_and_other_device_fail() {
        let device = generate_device_keypair("device-a");
        let other = generate_device_keypair("device-b");
        let signer = SigningKeyPair::generate("backend-dev");
        let bytes = publish_package(
            &payload(),
            PublishOptions {
                package_id: Uuid::new_v4(),
                revision_id: Uuid::new_v4(),
                recipients: &[Recipient {
                    key_id: device.key_id.clone(),
                    public_key: device.public_key.clone(),
                }],
                passphrase: Some("a long offline passphrase"),
                passphrase_key_id: "offline-owner",
                argon2: fast_argon2(),
            },
            &signer,
        )
        .unwrap();
        assert!(open_package(
            &bytes,
            OpenCredential::Passphrase("wrong but long passphrase"),
            &signer.verifying_key_bytes(),
            PackageLimits::default()
        )
        .is_err());
        assert!(matches!(
            open_package(
                &bytes,
                OpenCredential::Device(&other),
                &signer.verifying_key_bytes(),
                PackageLimits::default()
            ),
            Err(CatalogueError::NoMatchingRecipient)
        ));
    }

    #[test]
    fn tampering_with_header_ciphertext_or_signature_fails() {
        let signer = SigningKeyPair::generate("backend-dev");
        let bytes = publish_package(
            &payload(),
            PublishOptions {
                package_id: Uuid::new_v4(),
                revision_id: Uuid::new_v4(),
                recipients: &[],
                passphrase: Some("a long offline passphrase"),
                passphrase_key_id: "offline-owner",
                argon2: fast_argon2(),
            },
            &signer,
        )
        .unwrap();
        let key = signer.verifying_key_bytes();

        let mut ciphertext_tamper = bytes.clone();
        *ciphertext_tamper.last_mut().unwrap() ^= 1;
        assert!(matches!(
            open_package(
                &ciphertext_tamper,
                OpenCredential::Passphrase("a long offline passphrase"),
                &key,
                PackageLimits::default()
            ),
            Err(CatalogueError::InvalidSignature)
        ));

        let mut header_tamper = bytes.clone();
        let package_text = Uuid::new_v4().to_string();
        let existing = inspect_header(&bytes, PackageLimits::default())
            .unwrap()
            .authenticated
            .package_id
            .to_string();
        let offset = header_tamper
            .windows(existing.len())
            .position(|part| part == existing.as_bytes())
            .unwrap();
        header_tamper[offset..offset + existing.len()].copy_from_slice(package_text.as_bytes());
        assert!(matches!(
            open_package(
                &header_tamper,
                OpenCredential::Passphrase("a long offline passphrase"),
                &key,
                PackageLimits::default()
            ),
            Err(CatalogueError::InvalidSignature)
        ));
    }

    #[test]
    fn backend_can_add_a_recipient_without_reencrypting_the_payload() {
        let backend = generate_device_keypair("backend-wrap");
        let recipient = generate_device_keypair("device-b");
        let signer = SigningKeyPair::generate("backend-dev");
        let original = publish_package(
            &payload(),
            PublishOptions {
                package_id: Uuid::new_v4(),
                revision_id: Uuid::new_v4(),
                recipients: &[Recipient {
                    key_id: backend.key_id.clone(),
                    public_key: backend.public_key.clone(),
                }],
                passphrase: None,
                passphrase_key_id: "unused",
                argon2: fast_argon2(),
            },
            &signer,
        )
        .unwrap();
        let original_header = inspect_header(&original, PackageLimits::default()).unwrap();
        let rewrapped = rewrap_package(
            &original,
            &backend,
            Recipient {
                key_id: recipient.key_id.clone(),
                public_key: recipient.public_key.clone(),
            },
            &signer,
            &signer.verifying_key_bytes(),
        )
        .unwrap();
        let new_header = inspect_header(&rewrapped, PackageLimits::default()).unwrap();
        assert_eq!(
            original_header.authenticated.payload_nonce,
            new_header.authenticated.payload_nonce
        );
        assert!(open_package(
            &rewrapped,
            OpenCredential::Device(&recipient),
            &signer.verifying_key_bytes(),
            PackageLimits::default()
        )
        .is_ok());
    }

    #[test]
    fn encrypted_file_does_not_reveal_catalogue_text() {
        let signer = SigningKeyPair::generate("backend-dev");
        let payload = build_payload(
            CatalogueManifest {
                schema_version: 1,
                library_id: "secret-library".into(),
                shoot_id: "secret-shoot".into(),
                published_revision: 1,
                created_at: "now".into(),
                catalogue_blake3: String::new(),
                media_count: 1,
            },
            &sqlite_bytes(),
        )
        .unwrap();
        let package = publish_package(
            &payload,
            PublishOptions {
                package_id: Uuid::new_v4(),
                revision_id: Uuid::new_v4(),
                recipients: &[],
                passphrase: Some("a long offline passphrase"),
                passphrase_key_id: "offline-owner",
                argon2: fast_argon2(),
            },
            &signer,
        )
        .unwrap();
        assert!(!package
            .windows("secret-library".len())
            .any(|part| part == b"secret-library"));
        assert!(!package
            .windows("secret-shoot".len())
            .any(|part| part == b"secret-shoot"));
        assert!(!package
            .windows("manifest.json".len())
            .any(|part| part == b"manifest.json"));
        assert!(!package
            .windows("catalog.sqlite".len())
            .any(|part| part == b"catalog.sqlite"));
    }
}
