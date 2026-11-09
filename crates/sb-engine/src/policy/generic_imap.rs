//! The generic IMAP dialect (spec/04; Dovecot in CI) for servers the
//! engine knows nothing about: roles come only from LIST special-use
//! attributes and the INBOX name, and the pilot fetches no
//! OBJECTID/EMAILID, so no evidence groups occurrences.

use super::{INBOX_NAME, ListedMailbox, attribute_role};
use crate::{MailboxKind, MailboxRole};

/// A generic server has no provider identity header (spec/04).
pub(super) const IDENTITY_HEADER: Option<&'static str> = None;

/// RFC 6154's virtual collections: `\All` and `\Flagged` name
/// search-style collections, not real folders.
const VIRTUAL_ATTRIBUTES: &[&str] = &["\\All", "\\Flagged"];

/// spec/04 defines no generic kinds beyond folders; the RFC 6154
/// virtual collections map to `virtual`, everything else a folder.
pub(super) fn kind(listed: &ListedMailbox<'_>) -> MailboxKind {
    let virtual_collection = listed.attributes.iter().any(|attribute| {
        VIRTUAL_ATTRIBUTES
            .iter()
            .any(|&spelling| attribute.eq_ignore_ascii_case(spelling))
    });
    if virtual_collection {
        MailboxKind::Virtual
    } else {
        MailboxKind::Folder
    }
}

/// Roles come only from the LIST attributes plus INBOX's name,
/// matched case-insensitively as IMAP requires; no other name carries
/// a role on an unprofiled server.
pub(super) fn role(listed: &ListedMailbox<'_>) -> Option<MailboxRole> {
    if let Some(role) = attribute_role(listed.attributes) {
        return Some(role);
    }
    if listed.name.eq_ignore_ascii_case(INBOX_NAME) {
        return Some(MailboxRole::Inbox);
    }
    None
}

/// The pilot fetches no OBJECTID/EMAILID (RFC 8474) from generic
/// servers, so no provider evidence ever groups occurrences (spec/02).
pub(super) fn grouping_key(_header_literal: &[u8]) -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::{MailboxMapping, Policy, attribute_list, listed};

    /// A mapping table row: the listing's name and attribute
    /// spellings, and the expected `kind`, `role` and `selectable`.
    type Case<'a> = (
        &'a str,
        &'a [&'a str],
        MailboxKind,
        Option<MailboxRole>,
        bool,
    );

    #[test]
    fn dovecot_listings_map_from_attributes() {
        // A Dovecot-shaped listing: roles ride on the special-use
        // attributes, INBOX on its name.
        let cases: &[Case] = &[
            (
                "INBOX",
                &[],
                MailboxKind::Folder,
                Some(MailboxRole::Inbox),
                true,
            ),
            (
                "Sent",
                &["\\Sent"],
                MailboxKind::Folder,
                Some(MailboxRole::Sent),
                true,
            ),
            (
                "Trash",
                &["\\Trash"],
                MailboxKind::Folder,
                Some(MailboxRole::Trash),
                true,
            ),
            ("Archive", &[], MailboxKind::Folder, None, true),
            (
                "All",
                &["\\All"],
                MailboxKind::Virtual,
                Some(MailboxRole::All),
                true,
            ),
            (
                "Starred",
                &["\\Flagged"],
                MailboxKind::Virtual,
                Some(MailboxRole::Flagged),
                true,
            ),
            ("Lists", &["\\Noselect"], MailboxKind::Folder, None, false),
        ];
        for &(name, spellings, kind, role, selectable) in cases {
            let attributes = attribute_list(spellings);
            let mapping = Policy::GenericImap.map_mailbox(&listed(name, &attributes));
            assert_eq!(
                mapping,
                MailboxMapping {
                    kind,
                    role,
                    selectable
                },
                "mailbox {name}"
            );
        }
    }

    #[test]
    fn inbox_is_the_only_name_derived_role() {
        // A generic "Sent" folder without `\Sent` carries no role;
        // "inbox" still matches case-insensitively.
        for (name, role) in [("Sent", None), ("inbox", Some(MailboxRole::Inbox))] {
            let mapping = Policy::GenericImap.map_mailbox(&listed(name, &[]));
            assert_eq!(mapping.role, role, "mailbox {name}");
        }
    }

    #[test]
    fn no_grouping_evidence_in_the_pilot() {
        let literal = b"X-Pm-Internal-Id: pm-1\r\nMessage-ID: <a@b>\r\n";
        assert_eq!(Policy::GenericImap.grouping_key(literal), None);
    }
}
