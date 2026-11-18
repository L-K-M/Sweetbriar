//! Proton account rows (spec/02 "Proton accounts"): the SQL helpers
//! behind sign-in (plan M2.13b) and the read-only sync's cursors and
//! body failures (M2.15, M2.16). They write inside the caller's
//! transaction only; the credential itself stays in the secret store,
//! named by `credential_ref` (spec/10).

use std::collections::BTreeMap;

use rusqlite::{OptionalExtension, Transaction, params};
use sb_types::time::Timestamp;

use crate::StoreError;
use crate::accounts::{CredentialPause, IdentitySendable, NewIdentity, next_account_sort_order};
use crate::appearance::initialize_account_appearance;
use crate::schema::classify;

/// `proton_accounts.password_mode` (spec/02): 1 is one login password,
/// 2 a separate mailbox password.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProtonPasswordMode {
    Login,
    Mailbox,
}

impl ProtonPasswordMode {
    fn from_stored(value: i64) -> Result<Self, StoreError> {
        match value {
            1 => Ok(Self::Login),
            2 => Ok(Self::Mailbox),
            _ => Err(StoreError::corrupt(format!(
                "a Proton account's password_mode is {value}"
            ))),
        }
    }

    fn as_i64(self) -> i64 {
        match self {
            Self::Login => 1,
            Self::Mailbox => 2,
        }
    }
}

/// `proton_accounts.listing_state` (spec/02).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProtonListingState {
    None,
    Listing,
    Complete,
}

impl ProtonListingState {
    fn from_stored(value: &str) -> Result<Self, StoreError> {
        match value {
            "none" => Ok(Self::None),
            "listing" => Ok(Self::Listing),
            "complete" => Ok(Self::Complete),
            _ => Err(StoreError::corrupt(format!(
                "a Proton account's listing_state is '{value}'"
            ))),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Listing => "listing",
            Self::Complete => "complete",
        }
    }
}

/// One Proton account's durable row (spec/02 `proton_accounts`). The
/// `revision` changes with the cursors, listing state and diagnostics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtonAccount {
    pub account_id: String,
    pub api_base_url: String,
    pub username: String,
    pub proton_user_id: String,
    pub credential_ref: String,
    pub password_mode: ProtonPasswordMode,
    pub key_generation: i64,
    /// Last applied event id; `None` until the first bootstrap completes.
    pub event_cursor: Option<String>,
    /// Cursor read before the running bootstrap's listing.
    pub pending_cursor: Option<String>,
    pub listing_state: ProtonListingState,
    pub listing_checkpoint: Option<String>,
    pub refresh_count: i64,
    /// Last refusal code and message, redacted (spec/10).
    pub diagnostics_json: Option<String>,
    pub revision: i64,
}

/// A new Proton account's fields, as sign-in learns them. The remaining
/// columns take their defaults.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewProtonAccount {
    pub account_id: String,
    pub api_base_url: String,
    pub username: String,
    pub proton_user_id: String,
    pub credential_ref: String,
    pub password_mode: ProtonPasswordMode,
}

/// Insert the account's Proton row in the caller's transaction. A
/// Proton user belongs to one account: a sign-in as another user is
/// `Conflict`, never merged (spec/02).
pub fn insert_proton_account(
    tx: &Transaction,
    new: &NewProtonAccount,
) -> Result<ProtonAccount, StoreError> {
    if let Some(existing) = proton_account_by_user_id(tx, &new.proton_user_id)? {
        return Err(StoreError::conflict(format!(
            "Proton user {} already belongs to account {}",
            new.proton_user_id, existing.account_id
        )));
    }
    if proton_account(tx, &new.account_id)?.is_some() {
        return Err(StoreError::conflict(format!(
            "account {} already has a Proton user",
            new.account_id
        )));
    }
    tx.execute(
        "INSERT INTO proton_accounts
         (account_id, api_base_url, username, proton_user_id, credential_ref, password_mode,
          listing_state)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            new.account_id,
            new.api_base_url,
            new.username,
            new.proton_user_id,
            new.credential_ref,
            new.password_mode.as_i64(),
            ProtonListingState::None.as_str(),
        ],
    )
    .map_err(|e| classify(e, "create the Proton account"))?;
    let stored = proton_account(tx, &new.account_id)?
        .ok_or_else(|| StoreError::corrupt("the Proton account vanished after writing"))?;
    Ok(stored)
}

/// One completed sign-in's rows (spec/02, spec/07): account, Proton row,
/// identities and appearance, in the caller's transaction beside the
/// receipt.
pub struct NewProtonSignIn {
    pub account_id: String,
    pub display_name: String,
    pub api_base_url: String,
    pub username: String,
    pub proton_user_id: String,
    pub credential_ref: String,
    pub password_mode: ProtonPasswordMode,
    /// Enabled addresses, in Proton's order; the first is the default.
    pub identities: Vec<NewIdentity>,
}

/// Commit a completed sign-in. A new account inserts its rows and
/// lands paused with `CredentialPause::NotStored` until the secret is
/// stored (spec/10); an existing account is signed in again in place:
/// same UUID, `config_generation` and `revision` bumped, username and
/// password mode replaced, identities upserted (spec/07). A Proton
/// user another account holds, or another user than the account's, is
/// `Conflict`, never merged (spec/02).
pub fn commit_proton_sign_in(
    tx: &Transaction,
    new: &NewProtonSignIn,
    now: Timestamp,
) -> Result<(), StoreError> {
    if let Some(current) = proton_account(tx, &new.account_id)? {
        return replace_proton_account(tx, new, &current);
    }
    tx.execute(
        "INSERT INTO accounts (account_id, kind, provider, display_name,
          state, state_reason, sort_order, created_at)
         VALUES (?1, 'proton', 'proton_api', ?2, 'needs_attention', ?5, ?3, ?4)",
        params![
            new.account_id,
            new.display_name,
            next_account_sort_order(tx)?,
            now.as_millis(),
            CredentialPause::NotStored.reason()
        ],
    )
    .map_err(|e| classify(e, "create the account"))?;
    insert_proton_account(
        tx,
        &NewProtonAccount {
            account_id: new.account_id.clone(),
            api_base_url: new.api_base_url.clone(),
            username: new.username.clone(),
            proton_user_id: new.proton_user_id.clone(),
            credential_ref: new.credential_ref.clone(),
            password_mode: new.password_mode,
        },
    )?;
    initialize_account_appearance(tx, &new.account_id)?;
    for (index, identity) in new.identities.iter().enumerate() {
        tx.execute(
            "INSERT INTO identities (identity_id, account_id, email, display_name, is_default, sendable)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                identity.identity_id,
                new.account_id,
                identity.email,
                identity.display_name,
                i64::from(index == 0),
                identity.sendable.as_str(),
            ],
        )
        .map_err(|e| classify(e, "write an identity"))?;
    }
    Ok(())
}

/// Sign an existing account in again (spec/07): update its rows in
/// place, keeping the UUID and every other account's data. The state
/// is left as it is; the caller activates the account once the new
/// secret is stored.
fn replace_proton_account(
    tx: &Transaction,
    new: &NewProtonSignIn,
    current: &ProtonAccount,
) -> Result<(), StoreError> {
    if current.proton_user_id != new.proton_user_id {
        return Err(StoreError::conflict(format!(
            "Proton user {} does not match account {}, which holds {}",
            new.proton_user_id, new.account_id, current.proton_user_id
        )));
    }
    tx.execute(
        "UPDATE accounts
         SET display_name = ?2,
             config_generation = config_generation + 1, revision = revision + 1
         WHERE account_id = ?1",
        params![new.account_id, new.display_name],
    )
    .map_err(|e| classify(e, "update the account"))?;
    tx.execute(
        "UPDATE proton_accounts
         SET api_base_url = ?2, username = ?3, credential_ref = ?4,
             password_mode = ?5, key_generation = key_generation + 1,
             revision = revision + 1
         WHERE account_id = ?1",
        params![
            new.account_id,
            new.api_base_url,
            new.username,
            new.credential_ref,
            new.password_mode.as_i64(),
        ],
    )
    .map_err(|e| classify(e, "update the Proton account"))?;
    upsert_identities(tx, &new.account_id, &new.identities)
}

/// Refresh a Proton account's identities from its addresses, as an
/// address, user or key event asks (spec/04 "Sync loop" step 2): the
/// first is the account's default, and an address no longer returned
/// keeps its row (spec/02) but can't send. Writes, and bumps the
/// account's `config_generation`, only when an identity changed, since
/// Proton sends user events for unrelated changes; returns whether one
/// did. Only a Proton account; an unknown one is `NotFound`.
pub fn refresh_proton_identities(
    tx: &Transaction,
    account_id: &str,
    identities: &[NewIdentity],
) -> Result<bool, StoreError> {
    require_account(tx, account_id)?;
    let before = stored_identities(tx, account_id)?;
    let mut after: BTreeMap<String, StoredIdentity> = before
        .iter()
        .map(|(id, (email, name, _, _))| {
            let unavailable = IdentitySendable::Unavailable.as_str().to_owned();
            (
                id.clone(),
                (email.clone(), name.clone(), false, unavailable),
            )
        })
        .collect();
    for (index, identity) in identities.iter().enumerate() {
        after.insert(
            identity.identity_id.clone(),
            (
                identity.email.clone(),
                identity.display_name.clone(),
                index == 0,
                identity.sendable.as_str().to_owned(),
            ),
        );
    }
    if after == before {
        return Ok(false);
    }
    upsert_identities(tx, account_id, identities)?;
    tx.execute(
        "UPDATE accounts
         SET config_generation = config_generation + 1, revision = revision + 1
         WHERE account_id = ?1",
        params![account_id],
    )
    .map_err(|e| classify(e, "update the account"))?;
    Ok(true)
}

/// An identity's email, display name, default flag and sendable state.
type StoredIdentity = (String, String, bool, String);

fn stored_identities(
    tx: &Transaction,
    account_id: &str,
) -> Result<BTreeMap<String, StoredIdentity>, StoreError> {
    let read = |e| classify(e, "read the account's identities");
    let mut statement = tx
        .prepare(
            "SELECT identity_id, email, display_name, is_default, sendable
             FROM identities WHERE account_id = ?1",
        )
        .map_err(read)?;
    let rows = statement
        .query_map(params![account_id], |row| {
            Ok((
                row.get(0)?,
                (row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?),
            ))
        })
        .map_err(read)?;
    rows.collect::<Result<_, _>>().map_err(read)
}

fn upsert_identities(
    tx: &Transaction,
    account_id: &str,
    identities: &[NewIdentity],
) -> Result<(), StoreError> {
    // Every existing default clears first, so the upsert's first
    // identity is the account's one default. An address the new session
    // no longer returns keeps its row, since removal never deletes
    // (spec/02), but can't send; the upsert restores the ones returned.
    tx.execute(
        "UPDATE identities
         SET is_default = 0, sendable = ?2, revision = revision + 1
         WHERE account_id = ?1",
        params![account_id, IdentitySendable::Unavailable.as_str()],
    )
    .map_err(|e| classify(e, "clear the account's identities"))?;
    for (index, identity) in identities.iter().enumerate() {
        // An id another account holds is never moved here: the guarded
        // update changes nothing, which is refused below.
        let changed = tx.execute(
            "INSERT INTO identities (identity_id, account_id, email, display_name, is_default, sendable)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(identity_id) DO UPDATE SET
               email = excluded.email,
               display_name = excluded.display_name,
               is_default = excluded.is_default,
               sendable = excluded.sendable,
               revision = identities.revision + 1
             WHERE identities.account_id = excluded.account_id",
            params![
                identity.identity_id,
                account_id,
                identity.email,
                identity.display_name,
                i64::from(index == 0),
                identity.sendable.as_str(),
            ],
        )
        .map_err(|e| classify(e, "upsert an identity"))?;
        if changed == 0 {
            return Err(StoreError::conflict(format!(
                "identity {} belongs to another account than {}",
                identity.identity_id, account_id
            )));
        }
    }
    Ok(())
}

/// The account's Proton row, or `None`. `Corrupt` for a stored value
/// outside its range.
pub fn proton_account(
    tx: &Transaction,
    account_id: &str,
) -> Result<Option<ProtonAccount>, StoreError> {
    read_account(tx, "account_id", account_id)
}

/// The account a Proton user id belongs to, or `None`: the sign-in
/// refusal's lookup (spec/02).
pub fn proton_account_by_user_id(
    tx: &Transaction,
    proton_user_id: &str,
) -> Result<Option<ProtonAccount>, StoreError> {
    read_account(tx, "proton_user_id", proton_user_id)
}

/// spec/04's user-visible reason for a Proton account whose session
/// could not be restored: the owner must sign in again.
pub const SESSION_REFUSED_REASON: &str = "Sign in again";

/// Put an active or already-attentioned Proton account in
/// `needs_attention` with [`SESSION_REFUSED_REASON`], keeping every
/// row and its data (spec/04 "Account states and health"). A
/// user-paused or disconnected account is left as it is; an unknown
/// account is `NotFound`.
pub fn pause_for_session(tx: &Transaction, account_id: &str) -> Result<(), StoreError> {
    let changed = tx
        .execute(
            "UPDATE accounts
             SET state = 'needs_attention', state_reason = ?2, revision = revision + 1
             WHERE account_id = ?1 AND kind = 'proton'
               AND state IN ('active', 'needs_attention')",
            params![account_id, SESSION_REFUSED_REASON],
        )
        .map_err(|e| classify(e, "pause the Proton account's session"))?;
    if changed == 1 || proton_account(tx, account_id)?.is_some() {
        return Ok(());
    }
    Err(StoreError::not_found(format!("account {account_id}")))
}

/// Record a Proton sign-in's credential failure (spec/10): the account
/// waits for its new secret with `pause`'s reason. The user's own
/// sign-in outranks a pause or disconnect, so unlike the sync workers'
/// [`crate::accounts::pause_for_credential`] this applies to any
/// Proton account state; every row and all data are kept. `NotFound`
/// for an unknown account.
pub fn pause_for_proton_credential(
    tx: &Transaction,
    account_id: &str,
    pause: CredentialPause,
) -> Result<(), StoreError> {
    let changed = tx
        .execute(
            "UPDATE accounts
             SET state = 'needs_attention', state_reason = ?2, revision = revision + 1
             WHERE account_id = ?1 AND kind = 'proton'",
            params![account_id, pause.reason()],
        )
        .map_err(|e| classify(e, "pause the Proton account's credential"))?;
    if changed == 1 || proton_account(tx, account_id)?.is_some() {
        return Ok(());
    }
    Err(StoreError::not_found(format!("account {account_id}")))
}

/// Return a Proton account to `active` once its new secret is stored
/// (spec/04, 07): the user has just signed in, so any prior state
/// clears. A sign-in again's `Complete` is one of the transactions that
/// make a restored account active, so it reconciles `restore_pending`
/// here (07): the listing resets for the bootstrap. An already-active
/// account writes nothing. `NotFound` for an unknown account.
pub fn reactivate_proton_account(tx: &Transaction, account_id: &str) -> Result<(), StoreError> {
    let restore_pending = tx
        .query_row(
            "SELECT restore_pending FROM accounts WHERE account_id = ?1",
            params![account_id],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map_err(|e| classify(e, "read the account's restore flag"))?
        .ok_or_else(|| StoreError::not_found(format!("account {account_id}")))?;
    if restore_pending != 0 {
        // A sign-in again's `Complete` is one of the transactions that
        // make a restored account active, so it reconciles here (07):
        // the listing resets for the bootstrap.
        reset_proton_for_restore(tx, account_id)?;
    }
    let changed = tx
        .execute(
            "UPDATE accounts
             SET state = 'active', state_reason = NULL, restore_pending = 0,
                 revision = revision + 1
             WHERE account_id = ?1 AND kind = 'proton' AND state != 'active'",
            params![account_id],
        )
        .map_err(|e| classify(e, "reactivate the Proton account"))?;
    if changed == 1 || proton_account(tx, account_id)?.is_some() {
        return Ok(());
    }
    Err(StoreError::not_found(format!("account {account_id}")))
}

/// Disconnect a Proton account: `state='disconnected'`, every other row
/// and all of its data kept (spec/02, 04). A repeated disconnect
/// writes nothing; another account kind is `Conflict`; an unknown
/// account is `NotFound`.
pub fn disconnect_proton_account(tx: &Transaction, account_id: &str) -> Result<(), StoreError> {
    let changed = tx
        .execute(
            "UPDATE accounts
             SET state = 'disconnected', state_reason = NULL, revision = revision + 1
             WHERE account_id = ?1 AND kind = 'proton' AND state != 'disconnected'",
            params![account_id],
        )
        .map_err(|e| classify(e, "disconnect the Proton account"))?;
    if changed == 1 {
        return Ok(());
    }
    let kind: Option<String> = tx
        .query_row(
            "SELECT kind FROM accounts WHERE account_id = ?1",
            params![account_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| classify(e, "read the account kind"))?;
    match kind.as_deref() {
        Some("proton") => Ok(()),
        Some(_) => Err(StoreError::conflict(
            "only a Proton account can be disconnected here",
        )),
        None => Err(StoreError::not_found(format!("account {account_id}"))),
    }
}

/// Reset a Proton account's listing so its worker starts with the
/// bootstrap again (07 `resume_account`, 03 "Restore" step 5): the
/// event cursor and listing state clear, and the CHECK-constrained
/// pending cursor and checkpoint clear with them. A non-Proton account
/// matches no row and is left alone.
pub fn reset_proton_for_restore(tx: &Transaction, account_id: &str) -> Result<(), StoreError> {
    tx.execute(
        "UPDATE proton_accounts
         SET event_cursor = NULL, pending_cursor = NULL, listing_checkpoint = NULL,
             listing_state = 'none', revision = revision + 1
         WHERE account_id = ?1",
        params![account_id],
    )
    .map_err(|e| classify(e, "reset the Proton listing"))?;
    Ok(())
}

/// One account read by a fixed key column. `Corrupt` for a stored
/// value outside its range.
fn read_account(
    tx: &Transaction,
    key: &str,
    value: &str,
) -> Result<Option<ProtonAccount>, StoreError> {
    let sql = format!("SELECT {ACCOUNT_COLUMNS} FROM proton_accounts WHERE {key} = ?1");
    let row = tx
        .query_row(&sql, params![value], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
                row.get(7)?,
                row.get(8)?,
                row.get(9)?,
                row.get(10)?,
                row.get(11)?,
                row.get(12)?,
                row.get(13)?,
            ))
        })
        .optional()
        .map_err(|e| classify(e, "read the Proton account"))?;
    row.map(stored_account).transpose()
}

/// The `proton_accounts` columns both reads share, leading in each
/// query.
const ACCOUNT_COLUMNS: &str = "account_id, api_base_url, username, proton_user_id,
    credential_ref, password_mode, key_generation, event_cursor, pending_cursor,
    listing_state, listing_checkpoint, refresh_count, diagnostics_json, revision";

#[allow(clippy::type_complexity)]
type StoredAccount = (
    String,
    String,
    String,
    String,
    String,
    i64,
    i64,
    Option<String>,
    Option<String>,
    String,
    Option<String>,
    i64,
    Option<String>,
    i64,
);

/// The stored columns as a [`ProtonAccount`], validating the enumerations
/// and the revision.
fn stored_account(row: StoredAccount) -> Result<ProtonAccount, StoreError> {
    let (
        account_id,
        api_base_url,
        username,
        proton_user_id,
        credential_ref,
        password_mode,
        key_generation,
        event_cursor,
        pending_cursor,
        listing_state,
        listing_checkpoint,
        refresh_count,
        diagnostics_json,
        revision,
    ) = row;
    if revision < 1 {
        return Err(StoreError::corrupt(format!(
            "a Proton account's revision is {revision}"
        )));
    }
    Ok(ProtonAccount {
        account_id,
        api_base_url,
        username,
        proton_user_id,
        credential_ref,
        password_mode: ProtonPasswordMode::from_stored(password_mode)?,
        key_generation,
        event_cursor,
        pending_cursor,
        listing_state: ProtonListingState::from_stored(&listing_state)?,
        listing_checkpoint,
        refresh_count,
        diagnostics_json,
        revision,
    })
}

/// The account's row or `NotFound`; every update starts here.
fn require_account(tx: &Transaction, account_id: &str) -> Result<ProtonAccount, StoreError> {
    proton_account(tx, account_id)?
        .ok_or_else(|| StoreError::not_found(format!("account {account_id}")))
}

/// A stale revision is `Conflict`, even when the values match.
fn require_revision(current: &ProtonAccount, expected: i64) -> Result<(), StoreError> {
    if current.revision != expected {
        return Err(StoreError::conflict(format!(
            "the Proton account's revision is {}, not {expected}",
            current.revision
        )));
    }
    Ok(())
}

/// Record the cursor read before a bootstrap listing starts: the state
/// becomes `listing` and the pending cursor holds it, so a change made
/// during the listing is not lost (plan M2.15).
pub fn begin_listing(
    tx: &Transaction,
    account_id: &str,
    pending_cursor: &str,
    expected_revision: i64,
) -> Result<ProtonAccount, StoreError> {
    let current = require_account(tx, account_id)?;
    require_revision(&current, expected_revision)?;
    if current.listing_state == ProtonListingState::Listing
        && current.pending_cursor.as_deref() == Some(pending_cursor)
        && current.listing_checkpoint.is_none()
    {
        return Ok(current);
    }
    tx.execute(
        "UPDATE proton_accounts
         SET pending_cursor = ?2, listing_state = 'listing', listing_checkpoint = NULL,
             revision = revision + 1
         WHERE account_id = ?1",
        params![account_id, pending_cursor],
    )
    .map_err(|e| classify(e, "begin the Proton listing"))?;
    refresh::forget_listed(tx, account_id)?;
    require_account(tx, account_id)
}

/// Record the listing's last committed page position. Only valid while
/// listing, which the schema's CHECK also enforces.
pub fn update_listing_checkpoint(
    tx: &Transaction,
    account_id: &str,
    checkpoint: &str,
    expected_revision: i64,
) -> Result<ProtonAccount, StoreError> {
    let current = require_account(tx, account_id)?;
    require_revision(&current, expected_revision)?;
    if current.listing_state != ProtonListingState::Listing {
        return Err(StoreError::conflict("the Proton account is not listing"));
    }
    if current.listing_checkpoint.as_deref() == Some(checkpoint) {
        return Ok(current);
    }
    tx.execute(
        "UPDATE proton_accounts
         SET listing_checkpoint = ?2, revision = revision + 1
         WHERE account_id = ?1",
        params![account_id, checkpoint],
    )
    .map_err(|e| classify(e, "record the Proton listing checkpoint"))?;
    require_account(tx, account_id)
}

/// Finish the listing: the pending cursor and checkpoint clear, the
/// event cursor takes the listing's starting cursor, and the state
/// becomes `complete` (plan M2.15).
pub fn complete_listing(
    tx: &Transaction,
    account_id: &str,
    event_cursor: &str,
    expected_revision: i64,
) -> Result<ProtonAccount, StoreError> {
    let current = require_account(tx, account_id)?;
    require_revision(&current, expected_revision)?;
    if current.listing_state == ProtonListingState::Complete
        && current.event_cursor.as_deref() == Some(event_cursor)
    {
        return Ok(current);
    }
    tx.execute(
        "UPDATE proton_accounts
         SET listing_state = 'complete', pending_cursor = NULL, listing_checkpoint = NULL,
             event_cursor = ?2, revision = revision + 1
         WHERE account_id = ?1",
        params![account_id, event_cursor],
    )
    .map_err(|e| classify(e, "complete the Proton listing"))?;
    require_account(tx, account_id)
}

/// Record the last applied event id (plan M2.15).
pub fn set_event_cursor(
    tx: &Transaction,
    account_id: &str,
    event_cursor: &str,
    expected_revision: i64,
) -> Result<ProtonAccount, StoreError> {
    let current = require_account(tx, account_id)?;
    require_revision(&current, expected_revision)?;
    if current.event_cursor.as_deref() == Some(event_cursor) {
        return Ok(current);
    }
    tx.execute(
        "UPDATE proton_accounts
         SET event_cursor = ?2, revision = revision + 1
         WHERE account_id = ?1",
        params![account_id, event_cursor],
    )
    .map_err(|e| classify(e, "record the Proton event cursor"))?;
    require_account(tx, account_id)
}

/// Record the last refusal's redacted diagnostics, or clear them
/// (spec/10).
pub fn set_diagnostics(
    tx: &Transaction,
    account_id: &str,
    diagnostics_json: Option<&str>,
    expected_revision: i64,
) -> Result<ProtonAccount, StoreError> {
    let current = require_account(tx, account_id)?;
    require_revision(&current, expected_revision)?;
    if current.diagnostics_json.as_deref() == diagnostics_json {
        return Ok(current);
    }
    tx.execute(
        "UPDATE proton_accounts
         SET diagnostics_json = ?2, revision = revision + 1
         WHERE account_id = ?1",
        params![account_id, diagnostics_json],
    )
    .map_err(|e| classify(e, "record the Proton diagnostics"))?;
    require_account(tx, account_id)
}

/// `proton_body_failures.reason` (spec/02).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProtonBodyFailureReason {
    NoKey,
    Decrypt,
    Attachment,
    TooLarge,
    Reconstruct,
}

impl ProtonBodyFailureReason {
    fn from_stored(value: &str) -> Result<Self, StoreError> {
        match value {
            "no_key" => Ok(Self::NoKey),
            "decrypt" => Ok(Self::Decrypt),
            "attachment" => Ok(Self::Attachment),
            "too_large" => Ok(Self::TooLarge),
            "reconstruct" => Ok(Self::Reconstruct),
            _ => Err(StoreError::corrupt(format!(
                "a Proton body failure's reason is '{value}'"
            ))),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::NoKey => "no_key",
            Self::Decrypt => "decrypt",
            Self::Attachment => "attachment",
            Self::TooLarge => "too_large",
            Self::Reconstruct => "reconstruct",
        }
    }
}

/// One message's body failure (spec/02 `proton_body_failures`). It is
/// retried once the account's `key_generation` moves past
/// `key_generation` (plan M2.16).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtonBodyFailure {
    pub message_id: i64,
    pub reason: ProtonBodyFailureReason,
    pub detail: Option<String>,
    pub attempts: i64,
    pub key_generation: i64,
    pub last_at: i64,
}

/// Record a body failure, or increment its attempts when the same
/// message fails again.
pub fn record_body_failure(
    tx: &Transaction,
    message_id: i64,
    reason: ProtonBodyFailureReason,
    detail: Option<&str>,
    key_generation: i64,
    now: Timestamp,
) -> Result<(), StoreError> {
    tx.execute(
        "INSERT INTO proton_body_failures
         (message_id, reason, detail, attempts, key_generation, last_at)
         VALUES (?1, ?2, ?3, 1, ?4, ?5)
         ON CONFLICT(message_id) DO UPDATE SET
           reason = excluded.reason, detail = excluded.detail,
           attempts = proton_body_failures.attempts + 1,
           key_generation = excluded.key_generation,
           last_at = excluded.last_at",
        params![
            message_id,
            reason.as_str(),
            detail,
            key_generation,
            now.as_millis(),
        ],
    )
    .map_err(|e| classify(e, "record the Proton body failure"))?;
    Ok(())
}

/// One message's failure, or `None`.
pub fn body_failure(
    tx: &Transaction,
    message_id: i64,
) -> Result<Option<ProtonBodyFailure>, StoreError> {
    let row = tx
        .query_row(
            "SELECT message_id, reason, detail, attempts, key_generation, last_at
             FROM proton_body_failures WHERE message_id = ?1",
            params![message_id],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                ))
            },
        )
        .optional()
        .map_err(|e| classify(e, "read the Proton body failure"))?;
    row.map(stored_failure).transpose()
}

/// Clear a message's failure once its body is acquired or it is
/// abandoned. An unknown message is `NotFound`.
pub fn clear_body_failure(tx: &Transaction, message_id: i64) -> Result<(), StoreError> {
    let removed = tx
        .execute(
            "DELETE FROM proton_body_failures WHERE message_id = ?1",
            params![message_id],
        )
        .map_err(|e| classify(e, "clear the Proton body failure"))?;
    if removed == 0 {
        return Err(StoreError::not_found(format!(
            "the body failure of message {message_id}"
        )));
    }
    Ok(())
}

/// Bump the account's `key_generation` (spec/02): key unlocking
/// changed what it unlocked, so body failures recorded under an older
/// generation become eligible again. Returns the new generation.
pub fn bump_key_generation(tx: &Transaction, account_id: &str) -> Result<i64, StoreError> {
    let current = require_account(tx, account_id)?;
    let next = current
        .key_generation
        .checked_add(1)
        .ok_or_else(|| StoreError::corrupt("a Proton key generation is exhausted"))?;
    tx.execute(
        "UPDATE proton_accounts SET key_generation = ?2, revision = revision + 1
         WHERE account_id = ?1",
        params![account_id, next],
    )
    .map_err(|e| classify(e, "bump the Proton key generation"))?;
    Ok(next)
}

/// The stored columns as a [`ProtonBodyFailure`].
fn stored_failure(
    row: (i64, String, Option<String>, i64, i64, i64),
) -> Result<ProtonBodyFailure, StoreError> {
    let (message_id, reason, detail, attempts, key_generation, last_at) = row;
    Ok(ProtonBodyFailure {
        message_id,
        reason: ProtonBodyFailureReason::from_stored(&reason)?,
        detail,
        attempts,
        key_generation,
        last_at,
    })
}

mod bodies;
mod breaker;
mod events;
mod listing;
mod refresh;
pub use bodies::{
    MAX_PENDING_BODIES, PendingBody, ProtonBody, ProtonBodyCommit, ProtonOpenState,
    commit_proton_body, pending_proton_bodies, proton_open_state,
};
pub use breaker::{BreakerResolution, Classified, resolve_breaker_event};
pub use events::{AppliedEvents, EventChunk, ProtonChange, apply_event_chunk};
pub use listing::{
    ListedMessage, ListedPage, ListedPageStats, MailboxCoverage, ObservedLabel, ProtonCoverage,
    commit_listed_page, finish_listing, proton_coverage, reconcile_proton_labels,
};
pub use refresh::withhold_unlisted;

#[cfg(test)]
mod model_tests;
#[cfg(test)]
mod tests;
