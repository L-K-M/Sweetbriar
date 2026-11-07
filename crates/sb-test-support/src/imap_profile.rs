//! The Proton Bridge profile (spec/04's Bridge facts, spec/11): the
//! fake server's Bridge-shaped answers. Capabilities without
//! CONDSTORE, QRESYNC, NAMESPACE or LITERAL+; the `Folders/` and
//! `Labels/` layout with roles as LIST attributes; one synthetic
//! message materialized in several mailboxes under a single
//! `X-Pm-Internal-Id`; and UIDPLUS receipts. The SMTP half of the
//! profile (Bridge saves the Sent copy itself) arrives with the fake
//! SMTP server.

use crate::imap::{Expect, Reply, Step};

/// Capability tokens Bridge advertises (spec/04): no CONDSTORE or
/// QRESYNC, no NAMESPACE, no LITERAL+, no SPECIAL-USE.
const CAPABILITIES: &[&str] = &["IMAP4rev1", "IDLE", "UIDPLUS", "MOVE", "UNSELECT", "ID"];

/// `LIST "" "*"` answers: the standard set plus `Folders/` and
/// `Labels/`, both `\Noselect`, delimiter `/`; `Scheduled` is hidden.
const LIST_LINES: &[&str] = &[
    "LIST (\\HasNoChildren) \"/\" \"INBOX\"",
    "LIST (\\Drafts \\HasNoChildren) \"/\" Drafts",
    "LIST (\\Sent \\HasNoChildren) \"/\" Sent",
    "LIST (\\Flagged \\HasNoChildren) \"/\" Starred",
    "LIST (\\Archive \\HasNoChildren) \"/\" Archive",
    "LIST (\\Junk \\HasNoChildren) \"/\" Spam",
    "LIST (\\Trash \\HasNoChildren) \"/\" Trash",
    "LIST (\\All \\HasNoChildren) \"/\" \"All Mail\"",
    "LIST (\\Noselect \\HasChildren) \"/\" Folders",
    "LIST (\\HasNoChildren) \"/\" \"Folders/Travel\"",
    "LIST (\\Noselect \\HasChildren) \"/\" Labels",
    "LIST (\\HasNoChildren) \"/\" \"Labels/Receipts\"",
];

/// The epoch Bridge assigned these mailboxes at creation (spec/04).
const UIDVALIDITY: u32 = 4100;

/// The header fields spec/04's sync loop fetches.
const HEADER_FIELDS: &str = "DATE FROM REPLY-TO TO CC SUBJECT MESSAGE-ID IN-REPLY-TO REFERENCES LIST-ID DELIVERED-TO X-ORIGINAL-TO X-PM-INTERNAL-ID";

/// The headers Bridge reconstructs for the shared message: every
/// membership's header FETCH returns these same bytes.
const HEADERS: &str = concat!(
    "Date: Mon, 02 Nov 2026 09:15:00 +0000\r\n",
    "From: tickets@example.org\r\n",
    "To: owner@example.org\r\n",
    "Cc: travel@example.org\r\n",
    "Subject: Re: boarding pass\r\n",
    "Message-ID: <msg-7@example.org>\r\n",
    "In-Reply-To: <thread-7@example.org>\r\n",
    "X-Pm-Internal-Id: pm-7f3a9c21\r\n",
    "\r\n",
);

/// `(mailbox, uid, flags)`: where the shared message appears.
const MEMBERSHIPS: &[(&str, u32, &str)] = &[
    ("INBOX", 7, "\\Seen"),
    ("Folders/Travel", 3, ""),
    ("Labels/Receipts", 5, "\\Seen \\Flagged"),
    ("Starred", 2, "\\Flagged"),
    ("All Mail", 9, "\\Seen \\Flagged"),
];

/// The raw-UTF-8 ENVELOPE fault Bridge is known to emit (spec/04,
/// Gluon issue #498): the subject arrives as raw UTF-8 in a literal.
/// imap-codec alpha.9 accepts that — a literal is arbitrary octets —
/// so the eleventh envelope slot keeps the response undecodable: a
/// client must close, never apply it. `{17}` counts bytes.
pub const RAW_UTF8_ENVELOPE: &[u8] = b"* 1 FETCH (UID 7 ENVELOPE (\"Mon, 02 Nov 2026 09:15:00 +0000\" {17}\r\nGr\xc3\xbc\xc3\x9fe aus K\xc3\xb6ln NIL NIL NIL NIL NIL NIL NIL \"<msg-7@example.org>\" \"<spare-7@example.org>\"))\r\n";

/// The Bridge profile: step builders for the answers Bridge gives on
/// the sync client's read path (spec/04).
pub struct Profile {
    /// The advertised tokens. Bridge's own set by default; a script
    /// varies them for spec/11's advertise-or-withhold knob.
    capabilities: Vec<&'static str>,
}

impl Profile {
    /// Bridge as spec/04 describes it.
    pub fn bridge() -> Self {
        Self {
            capabilities: CAPABILITIES.to_vec(),
        }
    }

    /// Advertises `capabilities` instead of Bridge's set.
    pub fn with_capabilities(mut self, capabilities: &[&'static str]) -> Self {
        self.capabilities = capabilities.to_vec();
        self
    }

    /// The CAPABILITY answer, sent before and after STARTTLS alike.
    pub fn capability_step(&self) -> Step {
        Step::on(
            Expect::command("CAPABILITY"),
            vec![
                Reply::untagged(format!("CAPABILITY {}", self.capabilities.join(" "))),
                Reply::ok("CAPABILITY completed"),
            ],
        )
    }

    /// The `LIST "" "*"` answer: folders, labels and roles.
    pub fn list_step(&self) -> Step {
        let mut sends: Vec<Reply> = LIST_LINES.iter().copied().map(Reply::untagged).collect();
        sends.push(Reply::ok("LIST completed"));
        Step::on(Expect::command("LIST"), sends)
    }

    /// `EXAMINE` for one mailbox: EXISTS count, epoch and predicted
    /// next UID from the shared message's memberships.
    pub fn examine_step(&self, mailbox: &str) -> Step {
        let members = memberships(mailbox);
        let uidnext = members
            .iter()
            .map(|&(_, uid, _)| uid + 1)
            .max()
            .unwrap_or(1);
        Step::on(
            Expect::command(format!("EXAMINE {mailbox}")),
            vec![
                Reply::untagged("FLAGS (\\Seen \\Answered \\Flagged \\Deleted \\Draft)"),
                Reply::untagged(format!("{} EXISTS", members.len())),
                Reply::untagged("0 RECENT"),
                Reply::untagged(format!("OK [UIDVALIDITY {UIDVALIDITY}] UIDs valid")),
                Reply::untagged(format!("OK [UIDNEXT {uidnext}] Predicted next UID")),
                Reply::ok("[READ-ONLY] EXAMINE completed"),
            ],
        )
    }

    /// `UID SEARCH ALL`, including an explicit SEARCH frame when empty.
    pub fn uid_search_step(&self, mailbox: &str) -> Step {
        let uids = memberships(mailbox)
            .iter()
            .map(|(_, uid, _)| uid.to_string())
            .collect::<Vec<_>>()
            .join(" ");
        let row = if uids.is_empty() {
            "SEARCH".to_string()
        } else {
            format!("SEARCH {uids}")
        };
        Step::on(
            Expect::command("UID SEARCH ALL"),
            vec![Reply::untagged(row), Reply::ok("UID SEARCH completed")],
        )
    }

    /// `UID FETCH … (UID FLAGS)` for the mailbox's memberships.
    pub fn fetch_flags_step(&self, mailbox: &str) -> Step {
        let mut sends: Vec<Reply> = Vec::new();
        for (seq, uid, flags) in memberships(mailbox) {
            sends.push(Reply::untagged(format!(
                "{seq} FETCH (UID {uid} FLAGS ({flags}))"
            )));
        }
        sends.push(Reply::ok("UID FETCH completed"));
        Step::on(Expect::command("UID FETCH"), sends)
    }

    /// `UID FETCH … BODY[HEADER.FIELDS (…)]` for the mailbox's
    /// memberships: the same reconstructed header bytes everywhere.
    pub fn fetch_headers_step(&self, mailbox: &str) -> Step {
        let mut sends: Vec<Reply> = Vec::new();
        for (seq, uid, _) in memberships(mailbox) {
            sends.push(Reply::UntaggedLiteral {
                head: format!("{seq} FETCH (UID {uid} BODY[HEADER.FIELDS ({HEADER_FIELDS})]"),
                bytes: HEADERS.as_bytes().to_vec(),
                tail: ")".to_string(),
            });
        }
        sends.push(Reply::ok("UID FETCH completed"));
        Step::on(Expect::command("UID FETCH"), sends)
    }

    /// The UIDPLUS `APPENDUID` receipt for an APPEND's tagged OK.
    pub fn appenduid(&self, uid: u32) -> String {
        format!("[APPENDUID {UIDVALIDITY} {uid}] APPEND completed")
    }

    /// The UIDPLUS `COPYUID` receipt for a COPY's tagged OK.
    pub fn copyuid(&self, source: &str, destination: &str) -> String {
        format!("[COPYUID {UIDVALIDITY} {source} {destination}] COPY completed")
    }
}

/// The shared message's `(seq, uid, flags)` in one mailbox: the FETCH
/// prefix is the one-based sequence position, never the UID (RFC 3501
/// 6.4.8 — even UID FETCH answers by sequence number).
fn memberships(mailbox: &str) -> Vec<(u32, u32, &'static str)> {
    MEMBERSHIPS
        .iter()
        .filter(|(name, _, _)| *name == mailbox)
        .enumerate()
        .map(|(i, &(_, uid, flags))| (i as u32 + 1, uid, flags))
        .collect()
}
