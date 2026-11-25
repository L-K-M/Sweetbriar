//! Account senders and recipient allowances (spec/02 "Account senders
//! and recipient allowances", M6.7a). The sender table is derived by the
//! received-address sweep; the allowances are user data that survive
//! rebuilds and backups. Together they carry 05's recipient check: an
//! address that has written only to other accounts is blocked until the
//! user allows it for the sending account.

use std::collections::{BTreeMap, BTreeSet};

use rusqlite::{OptionalExtension, Transaction, params, params_from_iter};

use crate::StoreError;
use crate::query::AddressOperand;
use crate::schema::classify;

/// The `store_meta` watermark the account-sender history starts at.
const SENDERS_FROM_KEY: &str = "account_senders_from";
/// The durable revision the received-address sweep has published.
const SWEPT_KEY: &str = "seen_address_swept";

/// How many canonical addresses one `IN` query binds; well below
/// SQLite's variable limit however large a route's recipient list is.
const CANONICAL_CHUNK: usize = 400;

/// One allowed recipient as stored (spec/02).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredAllowance {
    pub account_id: String,
    pub canonical: String,
    pub email: String,
    pub created_at: i64,
}

/// One recipient the check blocks for a sending account (spec/05
/// "Recipient check").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredBlockedRecipient {
    /// The address as the draft spells it.
    pub address: String,
    /// The accounts it has written to, by account id.
    pub accounts: Vec<String>,
}

/// The recipient check for one sending account: whether it is ready and
/// what it blocks. Nothing is blocked before it is ready (spec/02
/// "Readiness").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredRecipientFence {
    pub ready: bool,
    pub blocked: Vec<StoredBlockedRecipient>,
}

/// The recipient check (spec/05): each address that has written to some
/// account but never to `account_id`, and isn't allowed for it, in the
/// order given, once per canonical address. An address that doesn't
/// parse is left to the send's own address validation.
pub fn recipient_fence(
    tx: &Transaction,
    account_id: &str,
    addresses: &[&str],
) -> Result<StoredRecipientFence, StoreError> {
    if !account_senders_ready(tx)? {
        return Ok(StoredRecipientFence {
            ready: false,
            blocked: Vec::new(),
        });
    }
    let mut recipients = Vec::new();
    let mut seen = BTreeSet::new();
    for address in addresses {
        let Ok(parsed) = AddressOperand::parse_received(address) else {
            continue;
        };
        if seen.insert(parsed.key().to_owned()) {
            recipients.push((parsed.key().to_owned(), (*address).to_owned()));
        }
    }
    let canonicals: Vec<String> = recipients.iter().map(|(key, _)| key.clone()).collect();
    let senders = sender_accounts(tx, &canonicals)?;
    let allowances = allowed(tx, account_id, &canonicals)?;
    let blocked = recipients
        .into_iter()
        .filter_map(|(canonical, address)| {
            let accounts = senders.get(&canonical)?;
            let passes = accounts.is_empty()
                || accounts.contains(account_id)
                || allowances.contains(&canonical);
            (!passes).then(|| StoredBlockedRecipient {
                address,
                accounts: accounts.iter().cloned().collect(),
            })
        })
        .collect();
    Ok(StoredRecipientFence {
        ready: true,
        blocked,
    })
}

/// Rebuild the derived sender history (spec/02 "Account senders"): empty
/// it, advance `seen_address_inputs` and move `account_senders_from` to
/// that revision, so the check waits for the next complete pass, which
/// records the history again. Allowances and every other table stay.
pub fn rebuild_account_senders(tx: &Transaction) -> Result<(), StoreError> {
    tx.execute("DELETE FROM d_account_senders", [])
        .map_err(sql_error)?;
    crate::seen::note_inputs(tx)?;
    let from = crate::seen::revisions(tx)?.inputs;
    let updated = tx
        .execute(
            "UPDATE store_meta SET value = ?1 WHERE key = ?2",
            params![from.to_string(), SENDERS_FROM_KEY],
        )
        .map_err(sql_error)?;
    if updated != 1 {
        return Err(StoreError::corrupt(format!(
            "store_meta.{SENDERS_FROM_KEY} is missing"
        )));
    }
    Ok(())
}

/// Whether the account-sender history covers every message the sweep has
/// read so far: ready once `seen_address_swept` reaches
/// `account_senders_from`. A missing or malformed key is `Corrupt`, so
/// readiness is never guessed from absent metadata (spec/02
/// "Readiness").
pub fn account_senders_ready(tx: &Transaction) -> Result<bool, StoreError> {
    Ok(counter(tx, SWEPT_KEY)? >= counter(tx, SENDERS_FROM_KEY)?)
}

/// For each requested canonical address, the account ids it has written
/// to, ordered by account id. Every input canonical gets an entry, empty
/// when no account records it (spec/02 "Account senders").
pub fn sender_accounts(
    tx: &Transaction,
    canonicals: &[String],
) -> Result<BTreeMap<String, BTreeSet<String>>, StoreError> {
    let mut senders: BTreeMap<String, BTreeSet<String>> = canonicals
        .iter()
        .map(|canonical| (canonical.clone(), BTreeSet::new()))
        .collect();
    for chunk in canonicals.chunks(CANONICAL_CHUNK) {
        let placeholders = vec!["?"; chunk.len()].join(",");
        let sql = format!(
            "SELECT canonical, account_id FROM d_account_senders
             WHERE canonical IN ({placeholders})"
        );
        let mut statement = tx.prepare(&sql).map_err(sql_error)?;
        let rows = statement
            .query_map(params_from_iter(chunk), |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(sql_error)?;
        for row in rows {
            let (canonical, account_id) = row.map_err(sql_error)?;
            if let Some(accounts) = senders.get_mut(&canonical) {
                accounts.insert(account_id);
            }
        }
    }
    Ok(senders)
}

/// Which of these canonical addresses the account allows, ordered by
/// canonical (spec/02 "Allowances").
pub fn allowed(
    tx: &Transaction,
    account_id: &str,
    canonicals: &[String],
) -> Result<BTreeSet<String>, StoreError> {
    let mut statement = tx
        .prepare_cached("SELECT canonical FROM recipient_allowances WHERE account_id = ?1")
        .map_err(sql_error)?;
    let rows = statement
        .query_map([account_id], |row| row.get::<_, String>(0))
        .map_err(sql_error)?;
    let stored: BTreeSet<String> = rows.collect::<Result<_, _>>().map_err(sql_error)?;
    Ok(canonicals
        .iter()
        .filter(|canonical| stored.contains(*canonical))
        .cloned()
        .collect())
}

/// Allow one address for an account. The address is canonicalized for
/// matching but stored as given; the first allowance writes and returns
/// true, an already allowed address writes nothing and returns false. An
/// invalid address is `InvalidInput`, an unknown account `NotFound`, and
/// the local archive `InvalidInput` before any allowance is read or
/// written (spec/02 "Allowances").
pub fn allow_recipient(
    tx: &Transaction,
    account_id: &str,
    address: &str,
    now_ms: i64,
) -> Result<bool, StoreError> {
    let canonical = parse_address(address)?;
    require_sendable_account(tx, account_id)?;
    let already: bool = tx
        .prepare_cached(
            "SELECT EXISTS(SELECT 1 FROM recipient_allowances
             WHERE account_id = ?1 AND canonical = ?2)",
        )
        .map_err(sql_error)?
        .query_row(params![account_id, canonical.key()], |row| row.get(0))
        .map_err(sql_error)?;
    if already {
        return Ok(false);
    }
    tx.prepare_cached(
        "INSERT INTO recipient_allowances(account_id, canonical, email, created_at)
         VALUES(?1, ?2, ?3, ?4)",
    )
    .map_err(sql_error)?
    .execute(params![account_id, canonical.key(), address, now_ms])
    .map_err(sql_error)?;
    Ok(true)
}

/// Revoke one allowance. The same validation as [`allow_recipient`]; a
/// missing allowance is `NotFound` (spec/02 "Allowances").
pub fn remove_recipient_allowance(
    tx: &Transaction,
    account_id: &str,
    address: &str,
) -> Result<(), StoreError> {
    let canonical = parse_address(address)?;
    require_sendable_account(tx, account_id)?;
    let deleted = tx
        .prepare_cached(
            "DELETE FROM recipient_allowances
             WHERE account_id = ?1 AND canonical = ?2",
        )
        .map_err(sql_error)?
        .execute(params![account_id, canonical.key()])
        .map_err(sql_error)?;
    if deleted == 0 {
        return Err(StoreError::not_found("recipient allowance"));
    }
    Ok(())
}

/// Every allowance, ordered by account id then canonical (spec/02).
pub fn recipient_allowances(tx: &Transaction) -> Result<Vec<StoredAllowance>, StoreError> {
    let mut statement = tx
        .prepare(
            "SELECT account_id, canonical, email, created_at
             FROM recipient_allowances ORDER BY account_id, canonical",
        )
        .map_err(sql_error)?;
    let rows = statement
        .query_map([], |row| {
            Ok(StoredAllowance {
                account_id: row.get(0)?,
                canonical: row.get(1)?,
                email: row.get(2)?,
                created_at: row.get(3)?,
            })
        })
        .map_err(sql_error)?;
    rows.collect::<Result<_, _>>().map_err(sql_error)
}

/// Record one eligible source's first From for its account, in the
/// sweep's own transaction. Called only after all the source's keys have
/// been observed, so rerunning a pass never revisits it.
pub(crate) fn record_sender(
    tx: &Transaction,
    account_id: &str,
    sender: Option<&str>,
    observed: Option<i64>,
) -> Result<(), StoreError> {
    let Some(sender) = sender else {
        return Ok(());
    };
    // An eligible source always carries an observation time; without one
    // there is no defensible `first_seen_at`, so the row is skipped
    // rather than dated with a default.
    let Some(observed) = observed else {
        return Ok(());
    };
    let kind: Option<String> = tx
        .prepare_cached("SELECT kind FROM accounts WHERE account_id = ?1")
        .map_err(sql_error)?
        .query_row([account_id], |row| row.get(0))
        .optional()
        .map_err(sql_error)?;
    let Some(kind) = kind else {
        return Err(StoreError::corrupt(
            "received message names a missing account",
        ));
    };
    // Imported mail doesn't say which sending account received it.
    if kind == "local" {
        return Ok(());
    }
    if account_identity_matches(tx, account_id, sender)? {
        return Ok(());
    }
    tx.prepare_cached(
        "INSERT INTO d_account_senders(canonical, account_id, first_seen_at)
         VALUES(?1, ?2, ?3) ON CONFLICT DO NOTHING",
    )
    .map_err(sql_error)?
    .execute(params![sender, account_id, observed])
    .map_err(sql_error)?;
    Ok(())
}

/// Whether the canonical equals any of the account's identity addresses:
/// the account's own sent mail is not a sender (spec/02).
fn account_identity_matches(
    tx: &Transaction,
    account_id: &str,
    sender: &str,
) -> Result<bool, StoreError> {
    let mut statement = tx
        .prepare_cached("SELECT email FROM identities WHERE account_id = ?1")
        .map_err(sql_error)?;
    let rows = statement
        .query_map([account_id], |row| row.get::<_, String>(0))
        .map_err(sql_error)?;
    for row in rows {
        let email = row.map_err(sql_error)?;
        if let Ok(identity) = AddressOperand::parse_received(&email)
            && identity.key() == sender
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// An existing account that can send: the local archive is refused with
/// `InvalidInput`, an absent account with `NotFound`.
fn require_sendable_account(tx: &Transaction, account_id: &str) -> Result<(), StoreError> {
    let kind: Option<String> = tx
        .prepare_cached("SELECT kind FROM accounts WHERE account_id = ?1")
        .map_err(sql_error)?
        .query_row([account_id], |row| row.get(0))
        .optional()
        .map_err(sql_error)?;
    match kind.as_deref() {
        Some("local") => Err(invalid("the local archive cannot send")),
        Some(_) => Ok(()),
        None => Err(StoreError::not_found("account")),
    }
}

fn parse_address(address: &str) -> Result<AddressOperand, StoreError> {
    AddressOperand::parse_received(address)
        .map_err(|error| invalid(format!("recipient address: {error}")))
}

/// A `store_meta` decimal counter; a missing or malformed value is
/// `Corrupt`, never a default.
fn counter(tx: &Transaction, key: &str) -> Result<u64, StoreError> {
    let value: Option<String> = tx
        .query_row(
            "SELECT value FROM store_meta WHERE key = ?1",
            [key],
            |row| row.get(0),
        )
        .optional()
        .map_err(sql_error)?;
    value
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| StoreError::corrupt(format!("store_meta.{key} is missing or malformed")))
}

fn invalid(message: impl Into<String>) -> StoreError {
    StoreError::InvalidInput {
        message: message.into(),
    }
}

fn sql_error(error: rusqlite::Error) -> StoreError {
    classify(error, "recipient allowances")
}

#[cfg(test)]
mod tests;
