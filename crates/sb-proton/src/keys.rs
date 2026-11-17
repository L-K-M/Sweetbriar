//! Keys, salts, addresses and decryption (spec/04 "Sign-in: Keys and
//! Identities", "Restore on start"; spec/10 "Proton private keys").
//! Key material and the key passphrase live in zeroizing buffers and
//! are never written anywhere (spec/10).

use std::fmt;

use hyper::Method;
use proton_crypto::ProtonPGP;
use proton_crypto::crypto::{
    DataEncoding, Decryptor, DecryptorSync, PGPProviderSync, VerifiedData,
};
use proton_crypto_account::keys::{
    AddressKeys, LockedKey, UnlockResult, UnlockedAddressKey, UnlockedUserKey, UnlockedUserKeys,
    UserKeys,
};
use proton_crypto_account::salts::{KeySecret, Salts};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use zeroize::Zeroizing;

use crate::auth::Session;
use crate::client::{Client, Priority, expect_success, read_response};
use crate::error::ProtonError;

/// `GET /core/v4/keys/salts` (research `auth-session.md` §7).
pub const KEY_SALTS_PATH: &str = "/core/v4/keys/salts";
/// `GET /core/v4/users`: the user and its locked keys (research
/// `auth-session.md` §7).
pub const USER_PATH: &str = "/core/v4/users";
/// `GET /core/v4/addresses` (spec/04 "Identities").
pub const ADDRESSES_PATH: &str = "/core/v4/addresses";
/// Proton's enabled address status (`go-proton-api/address_types.go`).
pub const ADDRESS_STATUS_ENABLED: u8 = 1;

/// `GET /core/v4/keys/salts` (research `auth-session.md` §7).
#[derive(Debug, Deserialize)]
struct SaltsResponse {
    #[serde(rename = "KeySalts")]
    salts: Salts,
}

/// `GET /core/v4/users` (research `auth-session.md` §7).
#[derive(Debug, Deserialize)]
struct UserResponse {
    #[serde(rename = "User")]
    user: User,
}

#[derive(Debug, Deserialize)]
struct User {
    #[serde(rename = "Keys")]
    keys: UserKeys,
}

/// `GET /core/v4/addresses` (spec/04 "Identities").
#[derive(Debug, Deserialize)]
struct AddressesResponse {
    #[serde(rename = "Addresses")]
    addresses: Vec<Address>,
}

/// One address from `GET /core/v4/addresses`
/// (`go-proton-api/address_types.go`), with its locked keys.
#[derive(Debug, Clone, Deserialize)]
pub struct Address {
    #[serde(rename = "ID")]
    pub id: String,
    #[serde(rename = "Email")]
    pub email: String,
    #[serde(rename = "DisplayName", default)]
    pub display_name: String,
    #[serde(rename = "Send", default)]
    pub send: bool,
    #[serde(rename = "Receive", default)]
    pub receive: bool,
    #[serde(rename = "Status", default)]
    pub status: u8,
    #[serde(rename = "Order", default)]
    pub order: u32,
    /// An address with no keys (an external one) still lists as an
    /// identity; it simply has nothing to unlock.
    #[serde(rename = "Keys", default = "empty_address_keys")]
    pub keys: AddressKeys,
}

fn empty_address_keys() -> AddressKeys {
    AddressKeys::new([])
}

/// The sendable state of an identity (spec/02).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sendable {
    /// Proton allows sending from it and an unlocked key exists.
    Validated,
    /// User-provisioned and unconfirmed (not produced by the Proton
    /// API; spec/02's send-as case).
    Provisional,
    /// The address can't send right now.
    Unavailable,
}

impl fmt::Display for Sendable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Validated => "validated",
            Self::Provisional => "provisional",
            Self::Unavailable => "unavailable",
        })
    }
}

/// One enabled address as an identity (spec/04 "Identities").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub id: String,
    pub email: String,
    pub display_name: String,
    pub order: u32,
    pub sendable: Sendable,
    /// Whether an address key unlocked. A key that can't be unlocked is
    /// recorded and shown; it does not fail the sign-in (spec/04).
    pub unlocked_key: bool,
}

/// Why a key set could not be unlocked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyUnlockError {
    /// The account has no user key.
    NoUserKey,
    /// No address key unlocked, so the sign-in cannot continue.
    NoAddressKey,
    /// The key passphrase could not be derived from the salt.
    Salt,
}

impl fmt::Display for KeyUnlockError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::NoUserKey => "the account has no user key",
            Self::NoAddressKey => "no address key could be unlocked",
            Self::Salt => "the key passphrase could not be derived from the salt",
        })
    }
}

impl std::error::Error for KeyUnlockError {}

/// A typed decryption failure: a damaged ciphertext is never a partial
/// plaintext (spec/04 "Fault handling").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecryptError {
    /// No unlocked address key exists.
    NoKey,
    /// The ciphertext is damaged or was not encrypted to these keys.
    Damaged,
}

impl fmt::Display for DecryptError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::NoKey => "no unlocked address key exists",
            Self::Damaged => "the ciphertext is damaged or not decryptable",
        })
    }
}

impl std::error::Error for DecryptError {}

/// A byte buffer that is zeroized on drop (spec/10): decrypted
/// plaintext and key material in memory. The `Zeroizing` drop the test
/// observes is the same one this wrapper relies on.
pub struct SecretBuffer(Zeroizing<Vec<u8>>);

impl SecretBuffer {
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(Zeroizing::new(bytes))
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.0
    }

    /// Consumes the buffer and yields the plaintext, still zeroized on
    /// drop (spec/10).
    pub fn into_vec(self) -> Zeroizing<Vec<u8>> {
        self.0
    }
}

impl fmt::Debug for SecretBuffer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretBuffer([redacted])")
    }
}

/// A signed-in account's unlocked keys and identities (spec/04 "Keys",
/// "Identities"). The private keys stay in memory only.
pub struct KeySet<P: PGPProviderSync> {
    provider: P,
    user_keys: UnlockedUserKeys<P>,
    address_keys: Vec<UnlockedAddressKey<P>>,
    /// Key ids that failed to unlock (spec/04: recorded and shown).
    pub failed: Vec<String>,
    pub identities: Vec<Identity>,
}

impl<P: PGPProviderSync> fmt::Debug for KeySet<P> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KeySet")
            .field("user_keys", &self.user_keys.len())
            .field("address_keys", &self.address_keys.len())
            .field("failed", &self.failed)
            .field("identities", &self.identities)
            .finish()
    }
}

/// The key-set operations the engine and M2.16 need, object-safe so a
/// worker can hold one without naming the provider type.
pub trait KeyStore: Send + Sync + fmt::Debug {
    /// The identities mapped from the enabled addresses (spec/04).
    fn identities(&self) -> &[Identity];

    /// Key ids that failed to unlock (spec/04: recorded and shown).
    fn failed(&self) -> &[String];

    /// Decrypts an armored message body (M2.16).
    fn decrypt_body(&self, armored: &[u8]) -> Result<SecretBuffer, DecryptError>;

    /// Decrypts an attachment's key packets and data packet (M2.16).
    fn decrypt_attachment(
        &self,
        key_packets: &[u8],
        data_packet: &[u8],
    ) -> Result<SecretBuffer, DecryptError>;
}

impl<P: PGPProviderSync> KeyStore for KeySet<P> {
    fn identities(&self) -> &[Identity] {
        &self.identities
    }

    fn failed(&self) -> &[String] {
        &self.failed
    }

    fn decrypt_body(&self, armored: &[u8]) -> Result<SecretBuffer, DecryptError> {
        KeySet::decrypt_body(self, armored)
    }

    fn decrypt_attachment(
        &self,
        key_packets: &[u8],
        data_packet: &[u8],
    ) -> Result<SecretBuffer, DecryptError> {
        KeySet::decrypt_attachment(self, key_packets, data_packet)
    }
}

impl<P: PGPProviderSync> KeySet<P> {
    /// Derives the key passphrase from `password` (the Proton password
    /// in one-password mode, the mailbox password in two) and the key
    /// salt, then unlocks the user and address keys (spec/04 "Keys").
    pub fn unlock(
        provider: P,
        password: &[u8],
        salts: &Salts,
        user_keys: &UserKeys,
        addresses: &[Address],
    ) -> Result<Self, KeyUnlockError> {
        Ok(Self::unlock_for_storage(provider, password, salts, user_keys, addresses)?.0)
    }

    /// [`KeySet::unlock`] plus the derived passphrase bytes (spec/10).
    pub fn unlock_for_storage(
        provider: P,
        password: &[u8],
        salts: &Salts,
        user_keys: &UserKeys,
        addresses: &[Address],
    ) -> Result<(Self, Zeroizing<Vec<u8>>), KeyUnlockError> {
        let Some(primary) = primary_user_key(user_keys) else {
            return Err(KeyUnlockError::NoUserKey);
        };
        let srp = proton_crypto::ProtonSRP::new_sync();
        let key_secret = salts
            .salt_for_key(&srp, &primary.id, password)
            .map_err(|_| KeyUnlockError::Salt)?;
        let passphrase = Zeroizing::new(key_secret.as_bytes().to_vec());
        let keys = Self::unlock_with_secret(provider, &key_secret, user_keys, addresses)?;
        Ok((keys, passphrase))
    }

    /// Unlocks with an already-derived key passphrase, as restored from
    /// the secret store on start (spec/04 "Restore on start", 10).
    pub fn unlock_with_secret(
        provider: P,
        key_secret: &KeySecret,
        user_keys: &UserKeys,
        addresses: &[Address],
    ) -> Result<Self, KeyUnlockError> {
        // Only active keys are unlocked, as Bridge does
        // (go-proton-api `Keys.Unlock`): Proton marks a key inactive
        // when it knows the key can't be decrypted, so trying it would
        // only report an expected failure.
        let active_user_keys = UserKeys(
            user_keys
                .0
                .iter()
                .filter(|key| key.active)
                .cloned()
                .collect(),
        );
        let UnlockResult {
            unlocked_keys,
            mut failed,
        } = active_user_keys.unlock(&provider, key_secret);
        let user_keys = UnlockedUserKeys::from(unlocked_keys);
        let user_key_refs: &[UnlockedUserKey<P>] = user_keys.as_ref();
        let all_address_keys = AddressKeys::new(
            addresses
                .iter()
                .flat_map(|address| address.keys.0.iter().filter(|key| key.active).cloned()),
        );
        let UnlockResult {
            unlocked_keys: address_keys,
            failed: address_failed,
        } = all_address_keys.unlock(&provider, user_key_refs, Some(key_secret));
        failed.extend(address_failed);
        if address_keys.is_empty() {
            return Err(KeyUnlockError::NoAddressKey);
        }
        let failed = failed.into_iter().map(|error| error.to_string()).collect();
        let identities = addresses
            .iter()
            .filter(|address| address.status == ADDRESS_STATUS_ENABLED)
            .map(|address| {
                let unlocked_key = address_keys
                    .iter()
                    .any(|key| address.keys.0.iter().any(|locked| locked.id == key.id));
                Identity {
                    id: address.id.clone(),
                    email: address.email.clone(),
                    display_name: address.display_name.clone(),
                    order: address.order,
                    sendable: if address.send && unlocked_key {
                        Sendable::Validated
                    } else {
                        Sendable::Unavailable
                    },
                    unlocked_key,
                }
            })
            .collect();
        Ok(Self {
            provider,
            user_keys,
            address_keys,
            failed,
            identities,
        })
    }

    /// Decrypts an armored message body (M2.16). The plaintext is a
    /// zeroizing buffer; damaged ciphertext is a typed failure.
    pub fn decrypt_body(&self, armored: &[u8]) -> Result<SecretBuffer, DecryptError> {
        let keys = self.private_keys();
        if keys.is_empty() {
            return Err(DecryptError::NoKey);
        }
        let verified = self
            .provider
            .new_decryptor()
            .with_decryption_keys(keys)
            .decrypt(armored, DataEncoding::Armor)
            .map_err(|_| DecryptError::Damaged)?;
        Ok(SecretBuffer::new(verified.into_vec()))
    }

    /// Decrypts an attachment's key packets and data packet (M2.16).
    pub fn decrypt_attachment(
        &self,
        key_packets: &[u8],
        data_packet: &[u8],
    ) -> Result<SecretBuffer, DecryptError> {
        let keys = self.private_keys();
        if keys.is_empty() {
            return Err(DecryptError::NoKey);
        }
        let session_key = self
            .provider
            .new_decryptor()
            .with_decryption_keys(keys)
            .decrypt_session_key(key_packets)
            .map_err(|_| DecryptError::Damaged)?;
        let verified = self
            .provider
            .new_decryptor()
            .with_session_key(session_key)
            .decrypt(data_packet, DataEncoding::Bytes)
            .map_err(|_| DecryptError::Damaged)?;
        Ok(SecretBuffer::new(verified.into_vec()))
    }

    fn private_keys(&self) -> Vec<&P::PrivateKey> {
        self.address_keys
            .iter()
            .map(|key| &key.private_key)
            .collect()
    }
}

/// The user key whose salt derives the key passphrase: the one marked
/// primary, since the API doesn't promise to list it first; the first
/// key when none is marked.
fn primary_user_key(user_keys: &UserKeys) -> Option<&LockedKey> {
    let mut keys = user_keys.0.iter();
    keys.clone().find(|key| key.primary).or_else(|| keys.next())
}

impl Client {
    /// `GET /core/v4/keys/salts` (research `auth-session.md` §7).
    pub async fn key_salts(&self, session: &Session) -> Result<Salts, ProtonError> {
        let response: SaltsResponse = self
            .get_json(KEY_SALTS_PATH, session, Priority::Interactive)
            .await?;
        Ok(response.salts)
    }

    /// `GET /core/v4/users`: the user's locked keys.
    pub async fn user_keys(&self, session: &Session) -> Result<UserKeys, ProtonError> {
        let response: UserResponse = self
            .get_json(USER_PATH, session, Priority::Interactive)
            .await?;
        Ok(response.user.keys)
    }

    /// `GET /core/v4/addresses` (spec/04 "Identities").
    pub async fn addresses(&self, session: &Session) -> Result<Vec<Address>, ProtonError> {
        let response: AddressesResponse = self
            .get_json(ADDRESSES_PATH, session, Priority::Interactive)
            .await?;
        Ok(response.addresses)
    }

    /// Fetches the salts, user keys and addresses and unlocks them with
    /// the password that unlocks the keys (spec/04 "Keys"). A key that
    /// fails is reported in `KeySet::failed`; inactive keys are skipped;
    /// only none unlocking is an error.
    pub async fn unlock_keys(
        &self,
        session: &Session,
        password: &[u8],
    ) -> Result<Box<dyn KeyStore>, ProtonError> {
        let provider = ProtonPGP::new_rust_sync();
        let salts = self.key_salts(session).await?;
        let user_keys = self.user_keys(session).await?;
        let addresses = self.addresses(session).await?;
        let keys = KeySet::unlock(provider, password, &salts, &user_keys, &addresses).map_err(
            |error| ProtonError::Unavailable {
                message: error.to_string(),
            },
        )?;
        Ok(Box::new(keys))
    }

    /// The unlocked keys plus the derived key passphrase (spec/10). A
    /// key that fails is reported; only none unlocking is an error.
    pub async fn unlock_keys_for_storage(
        &self,
        session: &Session,
        password: &[u8],
    ) -> Result<(Box<dyn KeyStore>, Zeroizing<Vec<u8>>), ProtonError> {
        let provider = ProtonPGP::new_rust_sync();
        let salts = self.key_salts(session).await?;
        let user_keys = self.user_keys(session).await?;
        let addresses = self.addresses(session).await?;
        let (keys, passphrase) =
            KeySet::unlock_for_storage(provider, password, &salts, &user_keys, &addresses)
                .map_err(ProtonError::KeyUnlock)?;
        Ok((Box::new(keys), passphrase))
    }

    /// Restores a session from the secret store's UID, refresh token and
    /// key passphrase and unlocks the keys without the password
    /// (spec/04 "Restore on start").
    pub async fn restore_session(
        &self,
        uid: &str,
        refresh_token: &str,
        key_passphrase: &[u8],
    ) -> Result<(Session, Box<dyn KeyStore>), ProtonError> {
        let session = self.refresh(uid, refresh_token).await?;
        let keys = self
            .unlock_keys_with_secret(&session, key_passphrase)
            .await
            .map_err(|error| match error {
                ProtonError::KeyUnlock(error) => ProtonError::Unavailable {
                    message: error.to_string(),
                },
                other => other,
            })?;
        Ok((session, keys))
    }

    /// Fetches the user keys and addresses again and unlocks them with
    /// the stored key passphrase, as address, user and key events ask
    /// (spec/04 "Sync loop" step 2). Keys the passphrase no longer
    /// unlocks, as after a password change, are
    /// [`ProtonError::KeyUnlock`].
    pub async fn unlock_keys_with_secret(
        &self,
        session: &Session,
        key_passphrase: &[u8],
    ) -> Result<Box<dyn KeyStore>, ProtonError> {
        let provider = ProtonPGP::new_rust_sync();
        let user_keys = self.user_keys(session).await?;
        let addresses = self.addresses(session).await?;
        let key_secret = KeySecret::new(key_passphrase.to_vec());
        let keys = KeySet::unlock_with_secret(provider, &key_secret, &user_keys, &addresses)
            .map_err(ProtonError::KeyUnlock)?;
        Ok(Box::new(keys))
    }

    /// One authenticated `GET` that decodes a JSON body, or a typed
    /// failure (spec/04 "Fault handling"). Sign-in and key calls are
    /// interactive; the sync reads in `mail` pass
    /// [`Priority::Background`].
    pub(crate) async fn get_json<T: DeserializeOwned>(
        &self,
        path: &str,
        session: &Session,
        priority: Priority,
    ) -> Result<T, ProtonError> {
        let response = self
            .send(
                Method::GET,
                path,
                None,
                priority,
                Some((session.uid(), session.expose_access_token())),
                &[],
            )
            .await?;
        let (status, bytes) = read_response(response).await?;
        expect_success(status, &bytes)?;
        serde_json::from_slice(&bytes).map_err(|e| ProtonError::decode(path, e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zeroizing_buffers_wipe_their_contents_on_drop() {
        use std::sync::{Arc, Mutex};
        use zeroize::Zeroize;

        /// A byte that records its own value when the buffer zeroizes
        /// it, so the drop is observable.
        struct SpyByte(u8, Arc<Mutex<Vec<u8>>>);

        impl Zeroize for SpyByte {
            fn zeroize(&mut self) {
                self.1.lock().expect("the spy lock").push(self.0);
                self.0 = 0;
            }
        }

        let seen = Arc::new(Mutex::new(Vec::new()));
        {
            let buffer: Zeroizing<Vec<SpyByte>> = Zeroizing::new(vec![
                SpyByte(0xAB, Arc::clone(&seen)),
                SpyByte(0xCD, Arc::clone(&seen)),
            ]);
            assert_eq!(buffer.len(), 2);
        }
        assert_eq!(
            *seen.lock().expect("the spy lock"),
            vec![0xAB, 0xCD],
            "the zeroizing buffer wiped its contents on drop"
        );
    }

    #[test]
    fn secret_buffers_redact() {
        let buffer = SecretBuffer::new(b"synthetic-key-material".to_vec());
        assert_eq!(buffer.as_slice(), b"synthetic-key-material");
        assert_eq!(format!("{buffer:?}"), "SecretBuffer([redacted])");
    }

    #[test]
    fn a_key_set_without_address_keys_is_a_typed_failure() {
        let provider = ProtonPGP::new_rust_sync();
        let salts = Salts::new([]);
        let user_keys = UserKeys::new([]);
        let error = KeySet::<_>::unlock(provider, b"password", &salts, &user_keys, &[]);
        assert!(matches!(error, Err(KeyUnlockError::NoUserKey)));
    }

    fn locked_key(id: &str, primary: bool) -> LockedKey {
        LockedKey {
            id: id.to_owned().into(),
            version: 4,
            private_key: String::new().into(),
            token: None,
            signature: None,
            activation: None,
            primary,
            active: true,
            flags: None,
            recovery_secret: None,
            recovery_secret_signature: None,
            address_forwarding_id: None,
        }
    }

    #[test]
    fn the_primary_user_key_salts_the_passphrase_wherever_it_is_listed() {
        let keys = UserKeys::new([locked_key("old", false), locked_key("primary", true)]);
        let chosen = primary_user_key(&keys).map(|key| key.id.to_string());
        assert_eq!(chosen.as_deref(), Some("primary"));

        let unmarked = UserKeys::new([locked_key("first", false), locked_key("second", false)]);
        let chosen = primary_user_key(&unmarked).map(|key| key.id.to_string());
        assert_eq!(chosen.as_deref(), Some("first"));
        assert!(primary_user_key(&UserKeys::new([])).is_none());
    }
}
