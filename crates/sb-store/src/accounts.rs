//! Account rows (spec/02 `accounts`, `imap_endpoints`, `identities`):
//! the SQL helpers behind `add_bridge_account` (spec/07). They write
//! rows inside the caller's transaction only — probing, secrets and
//! ordering live in `sb-resources`; the credential itself never
//! appears here, only its `credential_ref` (spec/10).

use rusqlite::{OptionalExtension, Transaction, params};
use sb_types::time::Timestamp;

mod connection;
pub use connection::{
    ConnectionPause, ImapEndpoint, ImapProvider, clear_connection_pause, imap_endpoint,
    pause_for_connection, syncable_accounts,
};

use crate::StoreError;
use crate::appearance::initialize_account_appearance;
use crate::proton::reset_proton_for_restore;
use crate::schema::classify;

/// `imap_security`/`smtp_security` (spec/02).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EndpointSecurity {
    Tls,
    StartTls,
}

impl EndpointSecurity {
    fn as_str(self) -> &'static str {
        match self {
            Self::Tls => "tls",
            Self::StartTls => "starttls",
        }
    }
}

/// `identities.sendable` (spec/02): what the From list may offer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdentitySendable {
    Validated,
    Provisional,
    Unavailable,
}

impl IdentitySendable {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Validated => "validated",
            Self::Provisional => "provisional",
            Self::Unavailable => "unavailable",
        }
    }
}

/// `imap_endpoints.address_mode`, Proton Bridge only (spec/02).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AddressMode {
    Combined,
    Split,
}

impl AddressMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Combined => "combined",
            Self::Split => "split",
        }
    }
}

/// Why an account waits for its credential (spec/10's retry path),
/// shown as `state_reason` on the health surface (spec/04). Only these
/// pauses are lifted by [`reactivate_after_credential`]: spec/04's
/// connection pauses (Bridge not running, login rejected, certificate
/// changed) need a successful connect, never a bare secret write.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CredentialPause {
    /// Setup committed the rows; the secret is not stored yet.
    NotStored,
    /// The secret store could not be reached (locked or no access).
    StoreUnavailable,
    /// The secret store refused the item.
    StoreRefused,
    /// The secret store failed another way.
    StoreFailed,
}

impl CredentialPause {
    const ALL: [Self; 4] = [
        Self::NotStored,
        Self::StoreUnavailable,
        Self::StoreRefused,
        Self::StoreFailed,
    ];

    /// The `state_reason` text (spec/04's "User sees" column). It is
    /// persisted (spec/02 stores the shown text) and matched back by the
    /// reactivation guard, so rewording one needs a migration of the
    /// stored rows; the tests pin the exact strings.
    pub fn reason(self) -> &'static str {
        match self {
            Self::NotStored => "the credential is not yet stored",
            Self::StoreUnavailable => "the credential store is unavailable",
            Self::StoreRefused => "the credential store refused the password",
            Self::StoreFailed => "the credential store could not keep the password",
        }
    }

    fn from_reason(reason: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|pause| pause.reason() == reason)
    }
}

/// One Bridge account's durable rows. Hosts are the literal
/// `127.0.0.1` (spec/10 loopback only); `credential_ref` names the
/// platform secret-store item and is the only credential material the
/// database ever holds.
pub struct NewBridgeAccount {
    pub account_id: String,
    pub display_name: String,
    pub endpoint: NewImapEndpoint,
    pub identities: Vec<NewIdentity>,
}

pub struct NewImapEndpoint {
    pub imap_port: i64,
    pub imap_security: EndpointSecurity,
    pub smtp_port: i64,
    pub smtp_security: EndpointSecurity,
    pub username: String,
    pub credential_ref: String,
    pub trusted_cert_sha256: String,
    /// Proton Bridge only; `None` stays NULL.
    pub address_mode: Option<AddressMode>,
    /// The probe's compatibility ledger (spec/04), already serialized.
    pub capabilities_json: String,
}

pub struct NewIdentity {
    pub identity_id: String,
    pub email: String,
    pub display_name: String,
    pub is_default: bool,
    pub sendable: IdentitySendable,
}

/// Insert the account, its endpoint and its identities plus the
/// default appearance assignment, in the caller's transaction. The
/// account lands `needs_attention` awaiting its credential: until the
/// secret is stored there is no safe active state, and a write that
/// fails before activation leaves a recoverable pause rather than an
/// active account with no credential (spec/10).
pub fn insert_bridge_account(
    tx: &Transaction,
    new: &NewBridgeAccount,
    now: Timestamp,
) -> Result<(), StoreError> {
    tx.execute(
        "INSERT INTO accounts
         (account_id, kind, provider, display_name, state, state_reason, sort_order, created_at)
         VALUES (?1, 'imap', 'proton_bridge', ?2, 'needs_attention', ?5, ?3, ?4)",
        params![
            new.account_id,
            new.display_name,
            next_account_sort_order(tx)?,
            now.as_millis(),
            CredentialPause::NotStored.reason()
        ],
    )
    .map_err(|e| classify(e, "create the account"))?;
    let endpoint = &new.endpoint;
    tx.execute(
        "INSERT INTO imap_endpoints
         (account_id, imap_host, imap_port, imap_security,
          smtp_host, smtp_port, smtp_security,
          username, credential_ref, trusted_cert_sha256,
          server_saves_sent, address_mode, capabilities_json)
         VALUES (?1, '127.0.0.1', ?2, ?3, '127.0.0.1', ?4, ?5, ?6, ?7, ?8, 1, ?9, ?10)",
        params![
            new.account_id,
            endpoint.imap_port,
            endpoint.imap_security.as_str(),
            endpoint.smtp_port,
            endpoint.smtp_security.as_str(),
            endpoint.username,
            endpoint.credential_ref,
            endpoint.trusted_cert_sha256,
            endpoint.address_mode.map(AddressMode::as_str),
            endpoint.capabilities_json,
        ],
    )
    .map_err(|e| classify(e, "create the endpoint"))?;
    for identity in &new.identities {
        tx.execute(
            "INSERT INTO identities
             (identity_id, account_id, email, display_name, is_default, sendable)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                identity.identity_id,
                new.account_id,
                identity.email,
                identity.display_name,
                i64::from(identity.is_default),
                identity.sendable.as_str(),
            ],
        )
        .map_err(|e| classify(e, "create an identity"))?;
    }
    initialize_account_appearance(tx, &new.account_id)?;
    Ok(())
}

/// The stored byte bound on an account's display name (spec/02
/// "Validation", spec/07 "Account management").
pub const MAX_DISPLAY_NAME_BYTES: usize = 128;

/// `update_account`'s name (spec/07): surrounding whitespace trimmed,
/// then 1–128 UTF-8 bytes with no control characters. The trimmed name
/// is what stores.
fn valid_display_name(name: &str) -> Result<String, StoreError> {
    let trimmed = name.trim();
    if trimmed.is_empty() || trimmed.len() > MAX_DISPLAY_NAME_BYTES {
        return Err(StoreError::InvalidInput {
            message: format!(
                "a display name is 1 through {MAX_DISPLAY_NAME_BYTES} UTF-8 bytes once trimmed"
            ),
        });
    }
    if trimmed.chars().any(char::is_control) {
        return Err(StoreError::InvalidInput {
            message: "a display name has no control characters".to_owned(),
        });
    }
    Ok(trimmed.to_owned())
}

/// `update_account(id, params)` (spec/07): the display name only,
/// trimmed and validated, with `revision` bumped and
/// `config_generation` untouched. An unknown account is `NotFound`.
pub fn update_account(
    tx: &Transaction,
    account_id: &str,
    display_name: &str,
) -> Result<(), StoreError> {
    let name = valid_display_name(display_name)?;
    let changed = tx
        .execute(
            "UPDATE accounts
             SET display_name = ?2, revision = revision + 1
             WHERE account_id = ?1",
            params![account_id, name],
        )
        .map_err(|e| classify(e, "rename the account"))?;
    if changed == 0 {
        return Err(StoreError::not_found(format!("account {account_id}")));
    }
    Ok(())
}

/// `pause_account(id)` (spec/07): an `active` account becomes `paused`;
/// a `paused` one is a no-op. The local archive has no server:
/// `InvalidInput`. A `needs_attention` or `disconnected` account is
/// `Conflict`; an unknown one is `NotFound`.
pub fn pause_account(tx: &Transaction, account_id: &str) -> Result<(), StoreError> {
    let (state, provider): (String, String) = tx
        .query_row(
            "SELECT state, provider FROM accounts WHERE account_id = ?1",
            params![account_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(|e| classify(e, "read the account state"))?
        .ok_or_else(|| StoreError::not_found(format!("account {account_id}")))?;
    if provider == "local_archive" {
        return Err(StoreError::InvalidInput {
            message: "the local archive has no server to pause".to_owned(),
        });
    }
    match state.as_str() {
        "active" => {
            tx.execute(
                "UPDATE accounts
                 SET state = 'paused', state_reason = NULL, revision = revision + 1
                 WHERE account_id = ?1",
                params![account_id],
            )
            .map_err(|e| classify(e, "pause the account"))?;
        }
        "paused" => {}
        _ => {
            return Err(StoreError::conflict(format!(
                "account {account_id} is {state}, not active"
            )));
        }
    }
    Ok(())
}

/// `resume_account(id)` (spec/07): a `paused` account becomes `active`;
/// an `active` one is a no-op. The local archive is `InvalidInput`; a
/// `needs_attention` or `disconnected` account is `Conflict`; an
/// unknown one is `NotFound`. An account a restore paused
/// (`restore_pending`) first reconciles in this transaction: IMAP
/// cursors are discarded (03 "Restore" step 5) and a Proton account's
/// listing resets so its worker bootstraps again. Answers whether the
/// state changed, so the caller starts a worker only for a resumed one.
pub fn resume_account(tx: &Transaction, account_id: &str) -> Result<bool, StoreError> {
    let (state, provider, restore_pending): (String, String, i64) = tx
        .query_row(
            "SELECT state, provider, restore_pending FROM accounts
             WHERE account_id = ?1",
            params![account_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(|e| classify(e, "read the account state"))?
        .ok_or_else(|| StoreError::not_found(format!("account {account_id}")))?;
    if provider == "local_archive" {
        return Err(StoreError::InvalidInput {
            message: "the local archive has no server to resume".to_owned(),
        });
    }
    match state.as_str() {
        "paused" => {
            if restore_pending != 0 {
                clear_imap_cursors(tx, account_id)?;
                reset_proton_for_restore(tx, account_id)?;
            }
            tx.execute(
                "UPDATE accounts
                 SET state = 'active', state_reason = NULL, restore_pending = 0,
                     revision = revision + 1
                 WHERE account_id = ?1",
                params![account_id],
            )
            .map_err(|e| classify(e, "resume the account"))?;
            return Ok(true);
        }
        "active" => {}
        _ => {
            return Err(StoreError::conflict(format!(
                "account {account_id} is {state}, not paused"
            )));
        }
    }
    Ok(false)
}

/// Discard an IMAP account's sync cursors (03 "Restore" step 5), so a
/// resumed account resyncs from the server's current state. A local
/// mailbox keeps its NULL cursors.
pub fn clear_imap_cursors(tx: &Transaction, account_id: &str) -> Result<(), StoreError> {
    tx.execute(
        "UPDATE mailboxes
         SET uidvalidity = NULL, highest_modseq = NULL, uidnext = NULL
         WHERE account_id = ?1",
        params![account_id],
    )
    .map_err(|e| classify(e, "discard the account's IMAP cursors"))?;
    Ok(())
}

/// Pause an account for its credential: `needs_attention` with
/// `pause`'s reason, replacing a connection pause (the credential is the
/// more basic blocker). Never deletes the credential or any rows
/// (spec/10). `Conflict` for an account the user paused or
/// disconnected; `NotFound` for an unknown one.
pub fn pause_for_credential(
    tx: &Transaction,
    account_id: &str,
    pause: CredentialPause,
) -> Result<(), StoreError> {
    // The user's own pause or disconnect outranks it: only the user
    // lifts those, so a sync worker's credential failure leaves them.
    let changed = tx
        .execute(
            "UPDATE accounts
             SET state = 'needs_attention', state_reason = ?2, revision = revision + 1
             WHERE account_id = ?1 AND state NOT IN ('paused', 'disconnected')",
            params![account_id, pause.reason()],
        )
        .map_err(|e| classify(e, "pause the account"))?;
    if changed == 1 {
        return Ok(());
    }
    let exists = tx
        .query_row(
            "SELECT 1 FROM accounts WHERE account_id = ?1",
            params![account_id],
            |_| Ok(()),
        )
        .optional()
        .map_err(|e| classify(e, "read the account"))?
        .is_some();
    if exists {
        return Err(StoreError::conflict(
            "the account is paused or disconnected by the user",
        ));
    }
    Err(StoreError::not_found(format!("account {account_id}")))
}

/// Return a credential-paused account to `active` once its secret is
/// stored by the retained `credential_ref` (spec/10's retry path). The
/// guard sits in the same statement as the flip, so a pause recorded
/// since the caller's check (spec/04's connection pauses, which need a
/// successful connect) is never cleared here: `Conflict` when the
/// account is not paused for its credential, `NotFound` when absent.
pub fn reactivate_after_credential(tx: &Transaction, account_id: &str) -> Result<(), StoreError> {
    let [not_stored, unavailable, refused, failed] =
        CredentialPause::ALL.map(CredentialPause::reason);
    // A restored account reconciles in whatever transaction makes it
    // active (07): its IMAP cursors are discarded first.
    let restore_pending = tx
        .query_row(
            "SELECT restore_pending FROM accounts WHERE account_id = ?1",
            params![account_id],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map_err(|e| classify(e, "read the account's restore flag"))?
        .unwrap_or(0);
    if restore_pending != 0 {
        clear_imap_cursors(tx, account_id)?;
    }
    let changed = tx
        .execute(
            "UPDATE accounts
             SET state = 'active', state_reason = NULL, restore_pending = 0,
                 revision = revision + 1
             WHERE account_id = ?1 AND state = 'needs_attention'
               AND state_reason IN (?2, ?3, ?4, ?5)",
            params![account_id, not_stored, unavailable, refused, failed],
        )
        .map_err(|e| classify(e, "reactivate the account"))?;
    if changed == 0 {
        // Only for its NotFound: an absent account is NotFound, any
        // existing one falls through to Conflict.
        credential_pause(tx, account_id)?;
        return Err(StoreError::conflict(
            "the account is not awaiting its credential",
        ));
    }
    Ok(())
}

/// The sidebar slot after every existing account (`accounts.sort_order`).
pub(crate) fn next_account_sort_order(tx: &Transaction) -> Result<i64, StoreError> {
    tx.query_row(
        "SELECT COALESCE(MAX(sort_order), -1) + 1 FROM accounts",
        [],
        |row| row.get(0),
    )
    .map_err(|e| classify(e, "read the account order"))
}

/// The credential pause the account is in, or `None` when it is not
/// waiting for its credential (active, or paused for another reason).
/// `NotFound` when the account does not exist.
pub fn credential_pause(
    tx: &Transaction,
    account_id: &str,
) -> Result<Option<CredentialPause>, StoreError> {
    let (state, reason): (String, Option<String>) = tx
        .query_row(
            "SELECT state, state_reason FROM accounts WHERE account_id = ?1",
            params![account_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => {
                StoreError::not_found(format!("account {account_id}"))
            }
            other => classify(other, "read the account state"),
        })?;
    if state != "needs_attention" {
        return Ok(None);
    }
    Ok(reason.as_deref().and_then(CredentialPause::from_reason))
}

/// The account's stored `credential_ref` (spec/02): the retry path
/// re-sets the secret by it after a `needs_attention` pause.
pub fn credential_ref(tx: &Transaction, account_id: &str) -> Result<String, StoreError> {
    tx.query_row(
        "SELECT credential_ref FROM imap_endpoints WHERE account_id = ?1",
        params![account_id],
        |row| row.get(0),
    )
    .map_err(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => {
            StoreError::not_found(format!("account {account_id}"))
        }
        other => classify(other, "read the credential reference"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{MIGRATIONS, run_migrations};
    use crate::testing::test_seams;
    use rusqlite::{Connection, params};
    use sb_test_support::real_fs;

    fn store() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        run_migrations(&mut conn, &test_seams(real_fs()), MIGRATIONS).unwrap();
        conn
    }

    fn local_archive(conn: &Connection) -> String {
        conn.query_row(
            "SELECT account_id FROM accounts WHERE provider = 'local_archive'",
            [],
            |row| row.get(0),
        )
        .unwrap()
    }

    fn add_account(conn: &Connection, id: &str, kind: &str, provider: &str, state: &str) {
        conn.execute(
            "INSERT INTO accounts
             (account_id, kind, provider, display_name, state, sort_order, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, 0, 0)",
            params![id, kind, provider, id, state],
        )
        .unwrap();
    }

    fn account_row(conn: &Connection, id: &str) -> (String, i64, i64) {
        conn.query_row(
            "SELECT state, revision, config_generation FROM accounts WHERE account_id = ?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap()
    }

    #[test]
    fn update_account_trims_bumps_revision_and_keeps_the_generation() {
        let mut conn = store();
        let archive = local_archive(&conn);
        let tx = conn.transaction().unwrap();
        update_account(&tx, &archive, "  Owner  ").unwrap();
        let (name, revision, generation): (String, i64, i64) = tx
            .query_row(
                "SELECT display_name, revision, config_generation
                 FROM accounts WHERE account_id = ?1",
                [&archive],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(name, "Owner");
        assert_eq!(revision, 2, "a rename bumps the revision");
        assert_eq!(generation, 1, "a rename keeps the config generation");
        tx.commit().unwrap();
    }

    #[test]
    fn update_account_refuses_bad_names_and_unknown_ids() {
        let mut conn = store();
        let archive = local_archive(&conn);
        let too_long = "x".repeat(MAX_DISPLAY_NAME_BYTES + 1);
        let too_long_multibyte = "é".repeat(65);
        for bad in [
            "",
            "   ",
            "a\nb",
            too_long.as_str(),
            too_long_multibyte.as_str(),
        ] {
            let tx = conn.transaction().unwrap();
            let error = update_account(&tx, &archive, bad).unwrap_err();
            assert!(
                matches!(error, StoreError::InvalidInput { .. }),
                "{bad:?}: {error:?}"
            );
            tx.rollback().unwrap();
        }
        // A 128-byte name (ASCII or multibyte) is accepted.
        for good in ["x".repeat(MAX_DISPLAY_NAME_BYTES), "é".repeat(64)] {
            let tx = conn.transaction().unwrap();
            update_account(&tx, &archive, &good).unwrap();
            tx.rollback().unwrap();
        }
        let tx = conn.transaction().unwrap();
        assert!(matches!(
            update_account(&tx, "missing", "Owner"),
            Err(StoreError::NotFound { .. })
        ));
        tx.rollback().unwrap();
    }

    #[test]
    fn pause_and_resume_follow_the_state_table() {
        let mut conn = store();
        let archive = local_archive(&conn);
        for (id, state) in [
            ("a", "active"),
            ("b", "paused"),
            ("c", "needs_attention"),
            ("d", "disconnected"),
        ] {
            add_account(&conn, id, "imap", "generic_imap", state);
        }

        let tx = conn.transaction().unwrap();
        pause_account(&tx, "a").unwrap();
        pause_account(&tx, "b").unwrap();
        assert!(matches!(
            pause_account(&tx, "c"),
            Err(StoreError::Conflict { .. })
        ));
        assert!(matches!(
            pause_account(&tx, "d"),
            Err(StoreError::Conflict { .. })
        ));
        assert!(matches!(
            pause_account(&tx, &archive),
            Err(StoreError::InvalidInput { .. })
        ));
        assert!(matches!(
            pause_account(&tx, "missing"),
            Err(StoreError::NotFound { .. })
        ));
        tx.commit().unwrap();
        assert_eq!(account_row(&conn, "a").0, "paused");
        let b = account_row(&conn, "b");
        assert_eq!(b.0, "paused");
        assert_eq!(b.1, 1, "pausing a paused account writes nothing");
        assert_eq!(account_row(&conn, "c").0, "needs_attention");
        assert_eq!(account_row(&conn, "d").0, "disconnected");

        let tx = conn.transaction().unwrap();
        resume_account(&tx, "b").unwrap();
        resume_account(&tx, "a").unwrap();
        resume_account(&tx, "a").unwrap();
        assert!(matches!(
            resume_account(&tx, "c"),
            Err(StoreError::Conflict { .. })
        ));
        assert!(matches!(
            resume_account(&tx, "d"),
            Err(StoreError::Conflict { .. })
        ));
        assert!(matches!(
            resume_account(&tx, &archive),
            Err(StoreError::InvalidInput { .. })
        ));
        assert!(matches!(
            resume_account(&tx, "missing"),
            Err(StoreError::NotFound { .. })
        ));
        tx.commit().unwrap();
        assert_eq!(account_row(&conn, "a").0, "active");
        assert_eq!(account_row(&conn, "b").0, "active");
        assert_eq!(account_row(&conn, "c").0, "needs_attention");
        assert_eq!(account_row(&conn, "d").0, "disconnected");
    }

    #[test]
    fn resuming_a_restore_pending_account_clears_cursors_and_proton_listing() {
        let mut conn = store();
        add_account(&conn, "imap", "imap", "generic_imap", "paused");
        conn.execute(
            "UPDATE accounts SET restore_pending = 1 WHERE account_id = 'imap'",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO mailboxes
             (account_id, remote_name, kind, selectable, sync_state,
              uidvalidity, uidnext, highest_modseq)
             VALUES ('imap', 'Inbox', 'folder', 1, 'synced', 42, 99, 7)",
            [],
        )
        .unwrap();
        add_account(&conn, "proton", "proton", "proton_api", "paused");
        conn.execute(
            "UPDATE accounts SET restore_pending = 1 WHERE account_id = 'proton'",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO proton_accounts
             (account_id, api_base_url, username, proton_user_id, credential_ref,
              password_mode, event_cursor, pending_cursor, listing_state, listing_checkpoint)
             VALUES ('proton', 'https://mail.example/api', 'u', 'pu', 'cr', 1,
                     'evt', 'pending', 'listing', 'checkpoint')",
            [],
        )
        .unwrap();
        // A downloaded body (an acquisition) is local data the resume's
        // reconcile must leave alone.
        conn.execute(
            "INSERT INTO acquisitions
             (content_sha256, byte_length, object_state, first_acquired_at)
             VALUES ('aa', 1, 'published', 0)",
            [],
        )
        .unwrap();

        let tx = conn.transaction().unwrap();
        resume_account(&tx, "imap").unwrap();
        resume_account(&tx, "proton").unwrap();
        tx.commit().unwrap();

        let (state, pending): (String, i64) = conn
            .query_row(
                "SELECT state, restore_pending FROM accounts WHERE account_id = 'imap'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(state, "active");
        assert_eq!(pending, 0);
        let cursors: (Option<i64>, Option<i64>, Option<i64>) = conn
            .query_row(
                "SELECT uidvalidity, uidnext, highest_modseq FROM mailboxes
                 WHERE account_id = 'imap'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(cursors, (None, None, None));

        let proton: (Option<String>, Option<String>, String, Option<String>) = conn
            .query_row(
                "SELECT event_cursor, pending_cursor, listing_state, listing_checkpoint
                 FROM proton_accounts WHERE account_id = 'proton'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(proton, (None, None, "none".to_owned(), None));
        let (state, pending): (String, i64) = conn
            .query_row(
                "SELECT state, restore_pending FROM accounts WHERE account_id = 'proton'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(state, "active");
        assert_eq!(pending, 0);
        let bodies: i64 = conn
            .query_row("SELECT count(*) FROM acquisitions", [], |row| row.get(0))
            .unwrap();
        assert_eq!(bodies, 1, "the resume keeps the account's bodies");
    }

    /// The reasons are persisted and matched back, so they are frozen:
    /// changing one strands rows written under the old text.
    #[test]
    fn credential_pause_reasons_are_frozen_and_round_trip() {
        let pinned = [
            (
                CredentialPause::NotStored,
                "the credential is not yet stored",
            ),
            (
                CredentialPause::StoreUnavailable,
                "the credential store is unavailable",
            ),
            (
                CredentialPause::StoreRefused,
                "the credential store refused the password",
            ),
            (
                CredentialPause::StoreFailed,
                "the credential store could not keep the password",
            ),
        ];
        assert_eq!(pinned.len(), CredentialPause::ALL.len());
        for (pause, text) in pinned {
            assert_eq!(pause.reason(), text);
            assert_eq!(CredentialPause::from_reason(text), Some(pause));
        }
        assert_eq!(
            CredentialPause::from_reason("Bridge's certificate changed"),
            None
        );
    }
}
