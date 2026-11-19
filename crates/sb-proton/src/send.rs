//! Send crypto and packages (spec/05 "Proton submission" steps 1-4;
//! plan M4.3d1). Pure and network-free: the recipient key lookup, the
//! draft body, the attachment upload packets and the send packages are
//! built here from `proton-crypto`; the HTTP calls that carry them are
//! M4.3d2. Written from research/proton-api `keys-crypto.md` §3-5 and
//! the fake's view of a send (sb-test-support `proton/send.rs`).
//!
//! Session keys and key material stay in zeroizing buffers and
//! `proton-crypto` types; `Debug` redacts them (spec/10).

use std::collections::BTreeMap;
use std::fmt;

use base64::Engine as _;
use proton_crypto::crypto::{
    DataEncoding, Encryptor, EncryptorSync, PGPProviderSync, SessionKey as _, SessionKeyAlgorithm,
    Signer, SignerSync, SigningMode, WritingMode,
};
use proton_crypto_account::keys::UnlockedAddressKey;
use serde::ser::SerializeStruct;
use serde::{Deserialize, Serialize, Serializer};

use crate::keys::SecretBuffer;

/// Proton's recipient type for an internal (Proton) address
/// (research `keys-crypto.md` §5).
pub const RECIPIENT_TYPE_INTERNAL: u8 = 1;
/// Proton's recipient type for an external address
/// (research `keys-crypto.md` §5).
pub const RECIPIENT_TYPE_EXTERNAL: u8 = 2;

/// The internal scheme: the session keys are encrypted to the
/// recipient's public key and carried as `BodyKeyPacket`
/// (research `keys-crypto.md` §4; the fake's `INTERNAL_SCHEME`).
pub const INTERNAL_SCHEME: u8 = 1;
/// The clear scheme: the session key is carried in `BodyKey`
/// (research `keys-crypto.md` §4; the fake's `CLEAR_SCHEME`).
pub const CLEAR_SCHEME: u8 = 4;

/// A signed body, as Proton's clients mark it (`Signature` 1; the
/// signature travels inside the encrypted data packet).
pub const SIGNATURE_DETACHED: u8 = 1;

/// A typed failure building a send's crypto (spec/10: the error carries
/// no key material).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendError {
    /// A recipient's public key could not be selected or imported.
    RecipientKey,
    /// The OpenPGP provider refused an operation.
    Crypto,
    /// A package needs at least one recipient.
    NoRecipients,
}

impl fmt::Display for SendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::RecipientKey => "a recipient's public key is missing or unusable",
            Self::Crypto => "the OpenPGP provider refused the operation",
            Self::NoRecipients => "a package needs at least one recipient",
        })
    }
}

impl std::error::Error for SendError {}

/// One public key `GET /core/v4/keys` serves for an address
/// (research `keys-crypto.md` §5; `go-proton-api/keys_types.go`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct RecipientPublicKey {
    /// Proton's key-state bitmap: 1 trusted, 2 active
    /// (`go-proton-api/keys_types.go`).
    #[serde(default)]
    pub flags: u32,
    /// The armored public key.
    pub public_key: String,
    /// Whether Proton marks it primary.
    #[serde(default)]
    pub primary: bool,
}

/// The public keys an address lookup returned (spec/05 step 1), as
/// `GET /core/v4/keys?Email=` answers it. An internal address is
/// encrypted to its keys; an external address without keys is sent in
/// clear; an external address with keys is refused by the caller
/// before any draft is created (M4.3e).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct RecipientKeys {
    #[serde(default)]
    pub keys: Vec<RecipientPublicKey>,
    /// 1 internal, 2 external (research `keys-crypto.md` §5).
    pub recipient_type: u8,
}

impl RecipientKeys {
    /// Whether the address is a Proton address.
    pub fn is_internal(&self) -> bool {
        self.recipient_type == RECIPIENT_TYPE_INTERNAL
    }

    /// The key to encrypt to: the primary key, else the first. The
    /// lookup already filters the keys Proton will accept.
    pub fn encryption_key(&self) -> Option<&str> {
        self.keys
            .iter()
            .find(|key| key.primary)
            .or_else(|| self.keys.first())
            .map(|key| key.public_key.as_str())
    }
}

/// One recipient of a send: its address and the keys the lookup
/// returned. `None` is an external address without keys, which gets
/// the clear scheme (spec/05 step 1).
#[derive(Debug, Clone)]
pub struct SendRecipient {
    pub address: String,
    pub keys: Option<RecipientKeys>,
}

/// One body part to package: its MIME type and plaintext bytes
/// (spec/05 step 4; a plain draft has `text/plain`, a Markdown draft
/// `text/html`).
pub struct SendBody<'a> {
    pub mime_type: &'a str,
    pub data: &'a [u8],
}

/// An attachment's session key, as the upload produced it, with the
/// attachment id the draft reported (research `keys-crypto.md` §4:
/// `AttachmentKeys`/`AttachmentKeyPackets` are keyed by attachment id).
pub struct SendAttachment {
    pub id: String,
    pub session_key: SessionKey,
}

/// A session key a package carries: the key bytes, zeroized on drop,
/// and the algorithm name (research `keys-crypto.md` §4). `Debug`
/// redacts the bytes (spec/10).
pub struct SessionKey {
    key: SecretBuffer,
    algorithm: SessionKeyAlgorithm,
}

impl Clone for SessionKey {
    fn clone(&self) -> Self {
        Self::new(self.key.as_slice().to_vec(), self.algorithm)
    }
}

impl SessionKey {
    /// A session key from exported bytes, as the upload's or a clear
    /// package's.
    pub fn new(key: Vec<u8>, algorithm: SessionKeyAlgorithm) -> Self {
        Self {
            key: SecretBuffer::new(key),
            algorithm,
        }
    }

    /// The raw key bytes.
    pub fn key(&self) -> &[u8] {
        self.key.as_slice()
    }

    /// The algorithm Proton names (`aes256`).
    pub fn algorithm(&self) -> SessionKeyAlgorithm {
        self.algorithm
    }
}

impl fmt::Debug for SessionKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SessionKey")
            .field("key", &"[redacted]")
            .field("algorithm", &self.algorithm)
            .finish()
    }
}

impl Serialize for SessionKey {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut value = serializer.serialize_struct("SessionKey", 2)?;
        value.serialize_field("Key", &encode_base64(self.key.as_slice()))?;
        value.serialize_field("Algorithm", &self.algorithm)?;
        value.end()
    }
}

/// The packages of one send, as `POST /mail/v4/messages/{id}` carries
/// them (research `keys-crypto.md` §4).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct SendPackages {
    pub packages: Vec<SendPackage>,
}

/// One package of a send: a MIME type, its body data packet, the clear
/// session key and each recipient's key packet (research
/// `keys-crypto.md` §4). `Debug` redacts the packets and session keys
/// (spec/10).
#[derive(Clone, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct SendPackage {
    /// The recipients by address.
    pub addresses: BTreeMap<String, PackageRecipient>,
    #[serde(rename = "MIMEType")]
    pub mime_type: String,
    /// The OR of the recipients' schemes.
    #[serde(rename = "Type")]
    pub scheme: u8,
    /// The body data packet, base64.
    #[serde(serialize_with = "serialize_base64")]
    pub body: Vec<u8>,
    /// The clear session key, present when a recipient is clear.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body_key: Option<SessionKey>,
    /// The clear attachment session keys by attachment id, present
    /// when a recipient is clear.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub attachment_keys: BTreeMap<String, SessionKey>,
}

impl fmt::Debug for SendPackage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SendPackage")
            .field("mime_type", &self.mime_type)
            .field("scheme", &self.scheme)
            .field("body", &"[redacted]")
            .field("body_key", &self.body_key)
            .field("attachment_keys", &self.attachment_keys)
            .field("addresses", &self.addresses)
            .finish()
    }
}

/// One recipient's entry in a package (research `keys-crypto.md` §4).
/// `Debug` redacts the key packets (spec/10).
#[derive(Clone, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct PackageRecipient {
    /// The scheme: 1 internal, 4 clear.
    #[serde(rename = "Type")]
    pub scheme: u8,
    /// The signature mark: 1 detached (a signed body).
    pub signature: u8,
    /// The body session key encrypted to the recipient's key, base64.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body_key_packet: Option<String>,
    /// The attachments' session keys encrypted to the recipient's key
    /// by attachment id, base64.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub attachment_key_packets: BTreeMap<String, String>,
}

impl fmt::Debug for PackageRecipient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PackageRecipient")
            .field("scheme", &self.scheme)
            .field("signature", &self.signature)
            .field(
                "body_key_packet",
                &self.body_key_packet.as_deref().map(|_| "[redacted]"),
            )
            .field(
                "attachment_key_packets",
                &self.attachment_key_packets.keys().collect::<Vec<_>>(),
            )
            .finish()
    }
}

/// An attachment's upload packets (research `keys-crypto.md` §3): the
/// key packets and data packet the draft's sender key decrypts, the
/// detached signature over the raw bytes, and the session key the send
/// packages carry.
#[derive(Debug, Clone)]
pub struct EncryptedAttachment {
    pub key_packets: Vec<u8>,
    pub data_packet: Vec<u8>,
    pub signature: Vec<u8>,
    pub session_key: SessionKey,
}

/// Encrypts and signs a draft body to the sender's address key
/// (spec/05 step 2): an armored PGP message, as `Message.Body`.
pub fn encrypt_draft_body<P: PGPProviderSync>(
    provider: &P,
    sender: &UnlockedAddressKey<P>,
    body: &[u8],
) -> Result<Vec<u8>, SendError> {
    let mut armored = Vec::new();
    provider
        .new_encryptor()
        .with_encryption_key(&sender.public_key)
        .with_signing_key(&sender.private_key)
        .encrypt_to_writer(
            body,
            DataEncoding::Armor,
            SigningMode::Inline,
            WritingMode::All,
            &mut armored,
        )
        .map_err(|_| SendError::Crypto)?;
    Ok(armored)
}

/// Encrypts an attachment for upload (research `keys-crypto.md` §3):
/// a fresh session key, the key packets to the sender's address key,
/// the data packet, and a binary detached signature over the raw bytes.
pub fn encrypt_attachment<P: PGPProviderSync>(
    provider: &P,
    sender: &UnlockedAddressKey<P>,
    data: &[u8],
) -> Result<EncryptedAttachment, SendError> {
    let session = provider
        .session_key_generate(SessionKeyAlgorithm::Aes256)
        .map_err(|_| SendError::Crypto)?;
    let key_packets = provider
        .new_encryptor()
        .with_encryption_key(&sender.public_key)
        .encrypt_session_key(&session)
        .map_err(|_| SendError::Crypto)?;
    let mut data_packet = Vec::new();
    provider
        .new_encryptor()
        .with_session_key(session.clone())
        .encrypt_to_writer(
            data,
            DataEncoding::Bytes,
            SigningMode::Inline,
            WritingMode::All,
            &mut data_packet,
        )
        .map_err(|_| SendError::Crypto)?;
    let signature = provider
        .new_signer()
        .with_signing_key(&sender.private_key)
        .sign_detached(data, DataEncoding::Bytes)
        .map_err(|_| SendError::Crypto)?;
    Ok(EncryptedAttachment {
        key_packets,
        data_packet,
        signature,
        session_key: SessionKey::new(session.export().as_ref().to_vec(), session.algorithm()),
    })
}

/// Builds the packages of one send (spec/05 step 4): one per body MIME
/// type, each body encrypted to a fresh session key and signed with the
/// sender's address key, each recipient with keys getting the session
/// key (and the attachments' session keys) encrypted to its public key
/// with the internal scheme, and each recipient without keys getting
/// the clear scheme carrying the session keys themselves.
pub fn build_packages<P: PGPProviderSync>(
    provider: &P,
    sender: &UnlockedAddressKey<P>,
    recipients: &[SendRecipient],
    bodies: &[SendBody<'_>],
    attachments: &[SendAttachment],
) -> Result<SendPackages, SendError> {
    if recipients.is_empty() {
        return Err(SendError::NoRecipients);
    }
    let packages = bodies
        .iter()
        .map(|body| build_package(provider, sender, recipients, body, attachments))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(SendPackages { packages })
}

/// One package: a fresh session key, the body data packet, and one
/// entry per recipient.
fn build_package<P: PGPProviderSync>(
    provider: &P,
    sender: &UnlockedAddressKey<P>,
    recipients: &[SendRecipient],
    body: &SendBody<'_>,
    attachments: &[SendAttachment],
) -> Result<SendPackage, SendError> {
    let session = provider
        .session_key_generate(SessionKeyAlgorithm::Aes256)
        .map_err(|_| SendError::Crypto)?;
    let data_packet = encrypt_body_packet(provider, sender, &session, body.data)?;
    let clear_key = SessionKey::new(session.export().as_ref().to_vec(), session.algorithm());
    let has_clear = recipients.iter().any(|recipient| recipient.keys.is_none());

    let mut addresses = BTreeMap::new();
    let mut scheme = 0;
    for recipient in recipients {
        let Some(keys) = &recipient.keys else {
            // An external recipient without keys: the clear scheme
            // carries the session key itself (spec/05 step 1).
            scheme |= CLEAR_SCHEME;
            addresses.insert(
                recipient.address.clone(),
                PackageRecipient {
                    scheme: CLEAR_SCHEME,
                    signature: SIGNATURE_DETACHED,
                    body_key_packet: None,
                    attachment_key_packets: BTreeMap::new(),
                },
            );
            continue;
        };
        let recipient_key = encrypt_recipient_key(provider, keys, &session, attachments)?;
        scheme |= INTERNAL_SCHEME;
        addresses.insert(recipient.address.clone(), recipient_key);
    }

    let body_key = has_clear.then(|| clear_key.clone());
    let attachment_keys = if has_clear {
        attachments
            .iter()
            .map(|attachment| (attachment.id.clone(), attachment.session_key.clone()))
            .collect()
    } else {
        BTreeMap::new()
    };
    Ok(SendPackage {
        addresses,
        mime_type: body.mime_type.to_owned(),
        scheme,
        body: data_packet,
        body_key,
        attachment_keys,
    })
}

/// One internal recipient's entry: the body session key and each
/// attachment session key encrypted to the recipient's public key.
fn encrypt_recipient_key<P: PGPProviderSync>(
    provider: &P,
    keys: &RecipientKeys,
    session: &P::SessionKey,
    attachments: &[SendAttachment],
) -> Result<PackageRecipient, SendError> {
    let Some(public_armored) = keys.encryption_key() else {
        return Err(SendError::RecipientKey);
    };
    let public = provider
        .public_key_import(public_armored.as_bytes(), DataEncoding::Armor)
        .map_err(|_| SendError::RecipientKey)?;
    let key_packets = provider
        .new_encryptor()
        .with_encryption_key(&public)
        .encrypt_session_key(session)
        .map_err(|_| SendError::Crypto)?;
    let mut attachment_key_packets = BTreeMap::new();
    for attachment in attachments {
        let attachment_session = provider
            .session_key_import(
                attachment.session_key.key(),
                attachment.session_key.algorithm(),
            )
            .map_err(|_| SendError::Crypto)?;
        let encrypted = provider
            .new_encryptor()
            .with_encryption_key(&public)
            .encrypt_session_key(&attachment_session)
            .map_err(|_| SendError::Crypto)?;
        attachment_key_packets.insert(attachment.id.clone(), encode_base64(&encrypted));
    }
    Ok(PackageRecipient {
        scheme: INTERNAL_SCHEME,
        signature: SIGNATURE_DETACHED,
        body_key_packet: Some(encode_base64(&key_packets)),
        attachment_key_packets,
    })
}

/// The body data packet: encrypted to the session key and signed with
/// the sender's address key (the WebClient's `encryptMessage` with
/// `sessionKey` and `signingKeys`).
fn encrypt_body_packet<P: PGPProviderSync>(
    provider: &P,
    sender: &UnlockedAddressKey<P>,
    session: &P::SessionKey,
    plaintext: &[u8],
) -> Result<Vec<u8>, SendError> {
    let mut data = Vec::new();
    provider
        .new_encryptor()
        .with_session_key(session.clone())
        .with_signing_key(&sender.private_key)
        .encrypt_to_writer(
            plaintext,
            DataEncoding::Bytes,
            SigningMode::Inline,
            WritingMode::All,
            &mut data,
        )
        .map_err(|_| SendError::Crypto)?;
    Ok(data)
}

fn encode_base64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn serialize_base64<S: Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(&encode_base64(bytes))
}

#[cfg(test)]
mod tests {
    use proton_crypto::ProtonPGP;
    use proton_crypto::crypto::{
        DataEncoding, Decryptor, DecryptorSync, KeyGenerator, KeyGeneratorSync,
        SessionKeyAlgorithm, VerifiedData, Verifier, VerifierSync,
    };
    use proton_crypto_account::keys::{DecryptedAddressKey, KeyFlag, KeyId};
    use sb_test_support::{FakeProtonServer, TestAccount};

    use super::*;

    const USER: &str = "owner@example.org";
    const INTERNAL: &str = "owner@example.test";
    const PASSWORD: &str = "synthetic-owner-password";

    async fn server() -> FakeProtonServer {
        FakeProtonServer::serve(vec![TestAccount::new(USER, PASSWORD)])
            .await
            .expect("serve")
    }

    /// A generated sender address key, standing in for the unlocked key
    /// the engine holds for the identity.
    fn sender_key<P: PGPProviderSync>(provider: &P) -> UnlockedAddressKey<P> {
        let private = provider
            .new_key_generator()
            .with_user_id("Owner", "sender@example.test")
            .generate()
            .expect("the sender key generates");
        let public = provider
            .private_key_to_public_key(&private)
            .expect("the sender public key");
        let key: UnlockedAddressKey<P> = DecryptedAddressKey {
            id: KeyId::from("sender-key".to_owned()),
            flags: KeyFlag::default(),
            primary: true,
            is_v6: false,
            private_key: private,
            public_key: public,
        };
        key
    }

    fn internal_keys(public_armored: &str) -> RecipientKeys {
        RecipientKeys {
            keys: vec![RecipientPublicKey {
                flags: 3,
                public_key: public_armored.to_owned(),
                primary: true,
            }],
            recipient_type: RECIPIENT_TYPE_INTERNAL,
        }
    }

    fn decode_base64(value: &str) -> Vec<u8> {
        base64::engine::general_purpose::STANDARD
            .decode(value)
            .expect("base64")
    }

    #[test]
    fn a_draft_body_decrypts_and_verifies_with_the_sender_key() {
        let provider = ProtonPGP::new_rust_sync();
        let sender = sender_key(&provider);
        let armored = encrypt_draft_body(&provider, &sender, b"draft body").expect("the body");
        let verified = provider
            .new_decryptor()
            .with_decryption_key(&sender.private_key)
            .with_verification_key(&sender.public_key)
            .decrypt(&armored, DataEncoding::Armor)
            .expect("the draft body decrypts");
        assert_eq!(verified.as_bytes(), b"draft body");
        assert!(
            verified.verification_result().is_ok(),
            "the sender's signature verifies"
        );
    }

    #[tokio::test]
    async fn an_internal_package_decrypts_with_the_recipient_key_and_the_sender_signature() {
        let server = server().await;
        let provider = ProtonPGP::new_rust_sync();
        let sender = sender_key(&provider);
        let recipient = server
            .recipient_key(USER, INTERNAL)
            .expect("the recipient key");
        let packages = build_packages(
            &provider,
            &sender,
            &[SendRecipient {
                address: INTERNAL.to_owned(),
                keys: Some(internal_keys(recipient.public_key())),
            }],
            &[SendBody {
                mime_type: "text/plain",
                data: b"hello",
            }],
            &[],
        )
        .expect("the packages");

        assert_eq!(packages.packages.len(), 1);
        let package = &packages.packages[0];
        assert_eq!(package.scheme, INTERNAL_SCHEME);
        assert!(package.body_key.is_none());
        let entry = package.addresses.get(INTERNAL).expect("the recipient");
        assert_eq!(entry.scheme, INTERNAL_SCHEME);
        let packet = decode_base64(entry.body_key_packet.as_deref().expect("the key packet"));
        assert_eq!(
            recipient
                .decrypt_body(&packet, &package.body)
                .expect("the body decrypts"),
            b"hello"
        );

        // The same session key, decrypted by the recipient, verifies the
        // body against the sender's public key.
        let session_bytes = recipient
            .decrypt_session_key(&packet)
            .expect("the session key");
        let session = provider
            .session_key_import(session_bytes.as_slice(), SessionKeyAlgorithm::Aes256)
            .expect("the session key imports");
        let verified = provider
            .new_decryptor()
            .with_session_key(session)
            .with_verification_key(&sender.public_key)
            .decrypt(&package.body, DataEncoding::Bytes)
            .expect("the body decrypts");
        assert_eq!(
            verified
                .as_verified_bytes()
                .expect("the signature verifies"),
            b"hello"
        );
    }

    #[test]
    fn an_external_recipient_gets_the_clear_scheme_and_the_session_key() {
        let provider = ProtonPGP::new_rust_sync();
        let sender = sender_key(&provider);
        let packages = build_packages(
            &provider,
            &sender,
            &[SendRecipient {
                address: "external@example.test".to_owned(),
                keys: None,
            }],
            &[SendBody {
                mime_type: "text/plain",
                data: b"hello",
            }],
            &[],
        )
        .expect("the packages");

        let package = &packages.packages[0];
        assert_eq!(package.scheme, CLEAR_SCHEME);
        let body_key = package.body_key.as_ref().expect("the clear body key");
        assert_eq!(body_key.algorithm(), SessionKeyAlgorithm::Aes256);
        let entry = package
            .addresses
            .get("external@example.test")
            .expect("the recipient");
        assert_eq!(entry.scheme, CLEAR_SCHEME);
        assert!(entry.body_key_packet.is_none());

        // The clear session key decrypts the body, and the sender's
        // signature verifies over it. (The fake's `decrypt_clear_body`
        // reads only unsigned literal packets, so this uses the
        // provider directly.)
        let session = provider
            .session_key_import(body_key.key(), body_key.algorithm())
            .expect("the session key imports");
        let verified = provider
            .new_decryptor()
            .with_session_key(session)
            .with_verification_key(&sender.public_key)
            .decrypt(&package.body, DataEncoding::Bytes)
            .expect("the clear body decrypts");
        assert_eq!(
            verified
                .as_verified_bytes()
                .expect("the signature verifies"),
            b"hello"
        );
    }

    #[test]
    fn two_mime_types_give_two_packages_with_distinct_session_keys() {
        let provider = ProtonPGP::new_rust_sync();
        let sender = sender_key(&provider);
        let packages = build_packages(
            &provider,
            &sender,
            &[SendRecipient {
                address: "external@example.test".to_owned(),
                keys: None,
            }],
            &[
                SendBody {
                    mime_type: "text/html",
                    data: b"<p>hello</p>",
                },
                SendBody {
                    mime_type: "text/plain",
                    data: b"hello",
                },
            ],
            &[],
        )
        .expect("the packages");

        assert_eq!(packages.packages.len(), 2);
        assert_eq!(packages.packages[0].mime_type, "text/html");
        assert_eq!(packages.packages[1].mime_type, "text/plain");
        let html_key = packages.packages[0]
            .body_key
            .as_ref()
            .expect("the html key");
        let plain_key = packages.packages[1]
            .body_key
            .as_ref()
            .expect("the plain key");
        assert_ne!(html_key.key(), plain_key.key());
    }

    #[tokio::test]
    async fn attachment_session_keys_are_carried_per_package() {
        let server = server().await;
        let provider = ProtonPGP::new_rust_sync();
        let sender = sender_key(&provider);
        let recipient = server
            .recipient_key(USER, INTERNAL)
            .expect("the recipient key");
        let encrypted = encrypt_attachment(&provider, &sender, b"attachment bytes")
            .expect("the attachment encrypts");

        // The upload packets decrypt with the sender's key and the
        // detached signature verifies over the raw bytes.
        let session = provider
            .new_decryptor()
            .with_decryption_key(&sender.private_key)
            .decrypt_session_key(&encrypted.key_packets)
            .expect("the upload session key");
        let plaintext = provider
            .new_decryptor()
            .with_session_key(session)
            .decrypt(&encrypted.data_packet, DataEncoding::Bytes)
            .expect("the upload data packet");
        assert_eq!(plaintext.as_bytes(), b"attachment bytes");
        assert!(
            provider
                .new_verifier()
                .with_verification_key(&sender.public_key)
                .verify_detached(
                    b"attachment bytes",
                    &encrypted.signature,
                    DataEncoding::Bytes
                )
                .is_ok(),
            "the detached signature verifies"
        );

        let attachments = [SendAttachment {
            id: "att-1".to_owned(),
            session_key: encrypted.session_key.clone(),
        }];
        let internal = build_packages(
            &provider,
            &sender,
            &[SendRecipient {
                address: INTERNAL.to_owned(),
                keys: Some(internal_keys(recipient.public_key())),
            }],
            &[SendBody {
                mime_type: "text/plain",
                data: b"hello",
            }],
            &attachments,
        )
        .expect("the internal packages");
        let entry = internal.packages[0]
            .addresses
            .get(INTERNAL)
            .expect("the recipient");
        let packet = decode_base64(
            entry
                .attachment_key_packets
                .get("att-1")
                .expect("the attachment key packet"),
        );
        assert_eq!(
            recipient
                .decrypt_session_key(&packet)
                .expect("the attachment session key"),
            encrypted.session_key.key()
        );

        let clear = build_packages(
            &provider,
            &sender,
            &[SendRecipient {
                address: "external@example.test".to_owned(),
                keys: None,
            }],
            &[SendBody {
                mime_type: "text/plain",
                data: b"hello",
            }],
            &attachments,
        )
        .expect("the clear packages");
        let carried = clear.packages[0]
            .attachment_keys
            .get("att-1")
            .expect("the clear attachment key");
        assert_eq!(carried.key(), encrypted.session_key.key());
    }

    #[tokio::test]
    async fn the_packages_serialize_to_the_json_the_fake_parses() {
        let server = server().await;
        let provider = ProtonPGP::new_rust_sync();
        let sender = sender_key(&provider);
        let recipient = server
            .recipient_key(USER, INTERNAL)
            .expect("the recipient key");
        let packages = build_packages(
            &provider,
            &sender,
            &[
                SendRecipient {
                    address: INTERNAL.to_owned(),
                    keys: Some(internal_keys(recipient.public_key())),
                },
                SendRecipient {
                    address: "external@example.test".to_owned(),
                    keys: None,
                },
            ],
            &[SendBody {
                mime_type: "text/plain",
                data: b"hello",
            }],
            &[],
        )
        .expect("the packages");

        let value = serde_json::to_value(&packages).expect("the packages serialize");
        let package = &value["Packages"][0];
        assert_eq!(package["MIMEType"], "text/plain");
        assert_eq!(package["Type"], 1 | CLEAR_SCHEME);
        assert_eq!(
            package["Addresses"][INTERNAL]["Type"],
            serde_json::json!(INTERNAL_SCHEME)
        );
        assert!(package["Addresses"][INTERNAL]["BodyKeyPacket"].is_string());
        assert_eq!(
            package["Addresses"]["external@example.test"]["Type"],
            serde_json::json!(CLEAR_SCHEME)
        );
        assert!(package["BodyKey"]["Key"].is_string());
        assert_eq!(package["BodyKey"]["Algorithm"], "aes256");
        assert!(package["Body"].is_string());
    }

    #[test]
    fn debug_output_redacts_session_keys_and_bodies() {
        let provider = ProtonPGP::new_rust_sync();
        let sender = sender_key(&provider);
        let packages = build_packages(
            &provider,
            &sender,
            &[SendRecipient {
                address: "external@example.test".to_owned(),
                keys: None,
            }],
            &[SendBody {
                mime_type: "text/plain",
                data: b"hello",
            }],
            &[],
        )
        .expect("the packages");

        let debug = format!("{packages:?}");
        let body_key = packages.packages[0]
            .body_key
            .as_ref()
            .expect("the body key");
        let encoded = base64::engine::general_purpose::STANDARD.encode(body_key.key());
        assert!(!debug.contains(&encoded), "the session key is redacted");
        assert!(!debug.contains("hello"), "the body is redacted");
        assert!(!debug.contains("PRIVATE KEY"), "no key material is shown");
    }
}
