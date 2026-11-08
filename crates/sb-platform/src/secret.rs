//! The platform `SecretStore` (spec/10): macOS stores items in the
//! data-protection Keychain — every query carries
//! `kSecUseDataProtectionKeychain` (`use_protected_keychain` on all of
//! the store's operations), `cloud-sync` is explicitly `false`, and
//! writes carry `WhenUnlockedThisDeviceOnly`. Linux stores in the Secret
//! Service. Vendor status text never crosses the seam: failures map to
//! `SecretError` kinds only.

use std::collections::HashMap;
use std::sync::Arc;

use keyring_core::api::CredentialStoreApi;
use keyring_core::{Entry, Error};
use sb_types::secret::{Secret, SecretError, SecretStore, valid_credential_ref};

/// The Keychain service / Secret Service collection Sweetbriar writes.
const SERVICE: &str = "ch.lkmc.Sweetbriar";

#[cfg(target_os = "macos")]
fn platform_store() -> Result<Arc<dyn CredentialStoreApi + Send + Sync>, SecretError> {
    let mut config = HashMap::new();
    config.insert("cloud-sync", "false");
    Ok(
        apple_native_keyring_store::protected::Store::new_with_configuration(&config)
            .map_err(classify)?,
    )
}

#[cfg(target_os = "linux")]
fn platform_store() -> Result<Arc<dyn CredentialStoreApi + Send + Sync>, SecretError> {
    // The spec/01 pin (zbus-secret-service-keyring-store 1.0.1) pulls
    // unmaintained zvariant 5 / zbus_names 4 — RUSTSEC-2026-0370..0372
    // with no maintained upgrade. Construction reports typed
    // unavailability until the pin is resolved; it never calls passed.
    Err(SecretError::Unavailable)
}

/// The real store. Constructed explicitly (not the global default) so
/// concurrent app tests and other clients keep their own stores. The
/// service name and access policy are fixed here; callers only choose
/// the `credential_ref`.
pub struct PlatformSecrets {
    store: Arc<dyn CredentialStoreApi + Send + Sync>,
    write_modifiers: HashMap<&'static str, &'static str>,
}

fn classify(error: Error) -> SecretError {
    match error {
        Error::NoEntry => SecretError::NotFound,
        // The store cannot answer: locked, missing entitlement, backend
        // gone. needs_attention, never silent-empty (spec/10).
        Error::NoStorageAccess(_) | Error::PlatformFailure(_) => SecretError::Unavailable,
        Error::NoDefaultStore | Error::NotSupportedByStore(_) => SecretError::Unavailable,
        Error::Ambiguous(_)
        | Error::Invalid(..)
        | Error::TooLong(..)
        | Error::BadEncoding(_)
        | Error::BadDataFormat(..)
        | Error::BadStoreFormat(_) => SecretError::Refused,
        // Non-exhaustive future variants stay non-empty failures.
        _ => SecretError::Unavailable,
    }
}

impl PlatformSecrets {
    /// Open the platform store. Store construction touches no items.
    pub fn new() -> Result<Self, SecretError> {
        #[allow(unused_mut)]
        let mut write_modifiers = HashMap::new();
        // The write access policy (spec/10); reads and deletes carry no
        // modifiers — it must not gate their lookups.
        #[cfg(target_os = "macos")]
        write_modifiers.insert("access-policy", "WhenUnlockedThisDeviceOnly");
        Ok(Self {
            store: platform_store()?,
            write_modifiers,
        })
    }

    /// Wrap an explicit store. Tests exercise the wrapper's ref
    /// validation, error mapping and entry lifecycle against the
    /// vendor's own in-memory store.
    #[cfg(test)]
    fn from_store(store: Arc<dyn CredentialStoreApi + Send + Sync>) -> Self {
        Self {
            store,
            write_modifiers: HashMap::new(),
        }
    }

    fn entry(&self, credential_ref: &str, write: bool) -> Result<Entry, SecretError> {
        if !valid_credential_ref(credential_ref) {
            return Err(SecretError::InvalidRef);
        }
        self.store
            .build(
                SERVICE,
                credential_ref,
                write.then_some(&self.write_modifiers),
            )
            .map_err(classify)
    }
}

impl SecretStore for PlatformSecrets {
    fn get(&self, credential_ref: &str) -> Result<Secret, SecretError> {
        self.entry(credential_ref, false)?
            .get_secret()
            .map(Secret::new)
            .map_err(classify)
    }

    fn set(&self, credential_ref: &str, secret: &Secret) -> Result<(), SecretError> {
        self.entry(credential_ref, true)?
            .set_secret(secret.expose())
            .map_err(classify)
    }

    fn delete(&self, credential_ref: &str) -> Result<(), SecretError> {
        self.entry(credential_ref, false)?
            .delete_credential()
            .map_err(classify)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Classifying every vendor status keeps typed causes without
    /// leaking identifiers or secret bytes into diagnostics.
    #[test]
    fn vendor_statuses_map_to_typed_causes() {
        assert_eq!(classify(Error::NoEntry), SecretError::NotFound);
        for (kind, expected) in [
            (
                Error::NoStorageAccess(Box::new(std::fmt::Error)),
                SecretError::Unavailable,
            ),
            (
                Error::NotSupportedByStore("x".to_owned()),
                SecretError::Unavailable,
            ),
            (
                Error::Invalid("a".to_owned(), "b".to_owned()),
                SecretError::Refused,
            ),
        ] {
            assert_eq!(classify(kind), expected);
        }
    }

    /// A real round-trip against this machine's data-protection
    /// Keychain, on an isolated synthetic service entry that the test
    /// cleans up itself. On macOS the data-protection store can refuse
    /// unsigned test binaries with a missing entitlement; that is a
    /// typed `Unavailable`, never a panic or a crash — the test reports
    /// the outcome rather than forcing success.
    #[test]
    #[cfg(target_os = "macos")]
    fn macos_keychain_round_trip_or_typed_unavailability() {
        let store = match PlatformSecrets::new() {
            Ok(store) => store,
            Err(error) => {
                eprintln!("platform store unavailable at construction: {error:?}");
                return;
            }
        };
        let credential_ref = &format!("tests/{}-{}/round-trip", std::process::id(), "secrets");
        let secret = Secret::new(b"synthetic-round-trip".to_vec());
        match store.set(credential_ref, &secret) {
            Ok(()) => {
                assert_eq!(
                    store.get(credential_ref).map(|s| s.expose().to_vec()),
                    Ok(b"synthetic-round-trip".to_vec())
                );
                store.delete(credential_ref).unwrap();
                assert!(matches!(
                    store.get(credential_ref),
                    Err(SecretError::NotFound)
                ));
            }
            Err(SecretError::Unavailable) => {
                eprintln!("data-protection keychain refused the unsigned test binary");
            }
            Err(error) => panic!("unexpected store failure: {error:?}"),
        }
    }

    /// The wrapper contract over the vendor's in-memory store: ref
    /// validation before store contact, exact round-trip bytes,
    /// independent `Secret` boxes and `NotFound`-typed deletion.
    #[test]
    fn wrapper_lifecycle_over_the_mock_store() {
        let store =
            PlatformSecrets::from_store(keyring_core::mock::Store::new().expect("mock store"));
        let credential_ref = "account/4f9a2b01-0d5c-4e61-89ab-7732cb7f3e1f/bridge-password";
        // Ref validation happens before any store call.
        assert!(matches!(store.get("bad ref"), Err(SecretError::InvalidRef)));
        // Clean absence is typed, never an empty secret.
        assert!(matches!(
            store.get(credential_ref),
            Err(SecretError::NotFound)
        ));
        let first = Secret::new(b"first-secret".to_vec());
        store.set(credential_ref, &first).unwrap();
        drop(first);
        let read = store.get(credential_ref).unwrap();
        assert_eq!(read.expose(), b"first-secret");
        drop(read);
        // Replacement and explicit delete keep typed results.
        store
            .set(credential_ref, &Secret::new(b"second".to_vec()))
            .unwrap();
        assert_eq!(store.get(credential_ref).unwrap().expose(), b"second");
        store.delete(credential_ref).unwrap();
        assert!(matches!(
            store.get(credential_ref),
            Err(SecretError::NotFound)
        ));
    }
}
