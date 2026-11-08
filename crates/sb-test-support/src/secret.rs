//! A scripted `SecretStore` fake: an in-memory item map holding real
//! `Secret`s, per-operation fault injection and a call log. `get`
//! returns an independent `Secret` copy — mutating or dropping it never
//! aliases the stored credential.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

use sb_types::secret::{Secret, SecretError, SecretStore, valid_credential_ref};

/// One `SecretStore` operation, for scripting faults and reading counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SecretOp {
    Get,
    Set,
    Delete,
}

struct Inner {
    items: HashMap<String, Secret>,
    /// Errors consumed in order, one per call of the named operation.
    faults: HashMap<SecretOp, VecDeque<SecretError>>,
    calls: HashMap<SecretOp, u64>,
    log: Vec<(SecretOp, String)>,
}

/// The in-memory store. All methods take `&self` through a mutex; items
/// are `Secret`s so the fake exercises the same move/redact contract.
pub struct FakeSecrets {
    inner: Mutex<Inner>,
}

impl FakeSecrets {
    /// An empty store.
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Inner {
                items: HashMap::new(),
                faults: HashMap::new(),
                calls: HashMap::new(),
                log: Vec::new(),
            }),
        }
    }

    /// Queue an error for the next call of `op`. Queued errors fire in
    /// order; a consumed fault is gone.
    pub fn fail_next(&self, op: SecretOp, error: SecretError) {
        self.inner
            .lock()
            .expect("fake secrets lock poisoned")
            .faults
            .entry(op)
            .or_default()
            .push_back(error);
    }

    /// Calls recorded for `op`, including refused and faulted ones.
    pub fn calls(&self, op: SecretOp) -> u64 {
        *self
            .inner
            .lock()
            .expect("fake secrets lock poisoned")
            .calls
            .get(&op)
            .unwrap_or(&0)
    }

    /// The (op, credential_ref) history, in order.
    pub fn recorded(&self) -> Vec<(SecretOp, String)> {
        self.inner
            .lock()
            .expect("fake secrets lock poisoned")
            .log
            .clone()
    }

    fn record(inner: &mut Inner, op: SecretOp, credential_ref: &str) -> Result<(), SecretError> {
        *inner.calls.entry(op).or_insert(0) += 1;
        inner.log.push((op, credential_ref.to_owned()));
        if !valid_credential_ref(credential_ref) {
            return Err(SecretError::InvalidRef);
        }
        if let Some(error) = inner.faults.get_mut(&op).and_then(VecDeque::pop_front) {
            return Err(error);
        }
        Ok(())
    }
}

impl Default for FakeSecrets {
    fn default() -> Self {
        Self::new()
    }
}

impl SecretStore for FakeSecrets {
    fn get(&self, credential_ref: &str) -> Result<Secret, SecretError> {
        let mut inner = self.inner.lock().expect("fake secrets lock poisoned");
        Self::record(&mut inner, SecretOp::Get, credential_ref)?;
        match inner.items.get(credential_ref) {
            Some(secret) => Ok(Secret::new(secret.expose().to_vec())),
            None => Err(SecretError::NotFound),
        }
    }

    fn set(&self, credential_ref: &str, secret: &Secret) -> Result<(), SecretError> {
        let mut inner = self.inner.lock().expect("fake secrets lock poisoned");
        Self::record(&mut inner, SecretOp::Set, credential_ref)?;
        inner.items.insert(
            credential_ref.to_owned(),
            Secret::new(secret.expose().to_vec()),
        );
        Ok(())
    }

    fn delete(&self, credential_ref: &str) -> Result<(), SecretError> {
        let mut inner = self.inner.lock().expect("fake secrets lock poisoned");
        Self::record(&mut inner, SecretOp::Delete, credential_ref)?;
        match inner.items.remove(credential_ref) {
            // Deleting an absent reference is `NotFound`, like the
            // platform stores' `NoEntry`.
            Some(_) => Ok(()),
            None => Err(SecretError::NotFound),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REF: &str = "account/4f9a2b01-0d5c-4e61-89ab-7732cb7f3e1f/bridge-password";

    #[test]
    fn round_trips_and_isolates_stored_bytes() {
        let store = FakeSecrets::new();
        let secret = Secret::new(b"synthetic-bridge-password".to_vec());
        store.set(REF, &secret).unwrap();
        let read = store.get(REF).unwrap();
        assert_eq!(read.expose(), b"synthetic-bridge-password");
        // The returned Secret is an independent box: dropping it leaves
        // the stored copy intact, and the stored copy is not aliased.
        drop(read);
        assert_eq!(
            store.get(REF).unwrap().expose(),
            b"synthetic-bridge-password"
        );
        store.delete(REF).unwrap();
        assert!(matches!(store.get(REF), Err(SecretError::NotFound)));
    }

    #[test]
    fn faults_are_consumed_and_typed() {
        let store = FakeSecrets::new();
        store.fail_next(SecretOp::Get, SecretError::Unavailable);
        store.fail_next(SecretOp::Get, SecretError::Refused);
        assert!(matches!(store.get(REF), Err(SecretError::Unavailable)));
        assert!(matches!(store.get(REF), Err(SecretError::Refused)));
        // Faults are consumed: an unfaulted get reaches the item map.
        assert!(matches!(store.get(REF), Err(SecretError::NotFound)));
        assert_eq!(store.calls(SecretOp::Get), 3);
    }

    #[test]
    fn invalid_refs_fail_before_scripted_faults() {
        let store = FakeSecrets::new();
        store.fail_next(SecretOp::Get, SecretError::Unavailable);
        assert!(matches!(store.get("bad ref"), Err(SecretError::InvalidRef)));
        // The scripted fault survived: it was not consumed by the refusal.
        assert!(matches!(store.get(REF), Err(SecretError::Unavailable)));
        assert_eq!(
            store.recorded(),
            vec![
                (SecretOp::Get, "bad ref".to_owned()),
                (SecretOp::Get, REF.to_owned()),
            ]
        );
    }

    #[test]
    fn set_replaces_and_delete_removes_explicitly() {
        let store = FakeSecrets::new();
        store.set(REF, &Secret::new(b"first".to_vec())).unwrap();
        store.set(REF, &Secret::new(b"second".to_vec())).unwrap();
        assert_eq!(store.get(REF).unwrap().expose(), b"second");
        store.delete(REF).unwrap();
        assert!(matches!(store.get(REF), Err(SecretError::NotFound)));
        assert_eq!(store.calls(SecretOp::Set), 2);
        assert_eq!(store.calls(SecretOp::Delete), 1);
    }

    /// A faulted write or delete must leave the stored state untouched
    /// — the trait's failed-update contract, not just the `Get` path.
    #[test]
    fn set_and_delete_faults_preserve_state() {
        let store = FakeSecrets::new();
        store.set(REF, &Secret::new(b"first".to_vec())).unwrap();
        store.fail_next(SecretOp::Set, SecretError::Unavailable);
        assert!(matches!(
            store.set(REF, &Secret::new(b"second".to_vec())),
            Err(SecretError::Unavailable)
        ));
        assert_eq!(store.get(REF).unwrap().expose(), b"first");
        store.fail_next(SecretOp::Delete, SecretError::Refused);
        assert!(matches!(store.delete(REF), Err(SecretError::Refused)));
        assert_eq!(store.get(REF).unwrap().expose(), b"first");
        store.delete(REF).unwrap();
        assert!(matches!(store.get(REF), Err(SecretError::NotFound)));
    }
}
