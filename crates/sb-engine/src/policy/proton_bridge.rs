//! Proton Bridge's dialect (spec/04 "Proton Bridge facts"): fixed
//! well-known mailboxes, `Folders/` and `Labels/` prefixes for user
//! structure, and the injected `X-Pm-Internal-Id` as the evidence
//! that groups one Proton message's many appearances.

use super::{INBOX_NAME, ListedMailbox, attribute_role, header_field_values};
use crate::{MailboxKind, MailboxRole};

/// The header field carrying the Proton message id Bridge injects
/// into fetched headers (spec/04).
const GROUPING_FIELD: &str = "X-Pm-Internal-Id";

/// The policy's identity header (spec/04).
pub(super) const IDENTITY_HEADER: Option<&'static str> = Some(GROUPING_FIELD);

/// Bridge's well-known virtual collections (spec/04); Scheduled is
/// read-only and carries no role.
const VIRTUAL_NAMES: &[&str] = &["Starred", "All Mail", "Scheduled"];

/// The namespaces Bridge gives user structure: each appears as a
/// bare `\Noselect` parent and as a `Parent/` prefix (spec/04).
const FOLDERS_PARENT: &str = "Folders";
const LABELS_PARENT: &str = "Labels";
const FOLDERS_PREFIX: &str = "Folders/";
const LABELS_PREFIX: &str = "Labels/";

/// The roles Bridge's well-known names carry when no LIST attribute
/// names one (spec/04). INBOX is matched separately: IMAP requires
/// case-insensitivity, and Scheduled carries no role.
const NAME_ROLES: &[(&str, MailboxRole)] = &[
    ("Drafts", MailboxRole::Drafts),
    ("Sent", MailboxRole::Sent),
    ("Starred", MailboxRole::Flagged),
    ("Archive", MailboxRole::Archive),
    ("Spam", MailboxRole::Junk),
    ("Trash", MailboxRole::Trash),
    ("All Mail", MailboxRole::All),
];

/// spec/04: `Folders/` names and their bare parent are folders,
/// `Labels/` names and their parent are labels, and the virtual
/// collections are `virtual`; every other name is a folder.
pub(super) fn kind(listed: &ListedMailbox<'_>) -> MailboxKind {
    if listed.name.starts_with(FOLDERS_PREFIX) || listed.name == FOLDERS_PARENT {
        return MailboxKind::Folder;
    }
    if listed.name.starts_with(LABELS_PREFIX) || listed.name == LABELS_PARENT {
        return MailboxKind::Label;
    }
    if VIRTUAL_NAMES.contains(&listed.name) {
        return MailboxKind::Virtual;
    }
    MailboxKind::Folder
}

/// spec/04: roles come from LIST attributes first, then from the
/// well-known names. A name inside `Folders/` or `Labels/` never
/// gets a name-derived role: `Folders/Sent` is not the Sent mailbox.
pub(super) fn role(listed: &ListedMailbox<'_>) -> Option<MailboxRole> {
    if let Some(role) = attribute_role(listed.attributes) {
        return Some(role);
    }
    if listed.name.eq_ignore_ascii_case(INBOX_NAME) {
        return Some(MailboxRole::Inbox);
    }
    if listed.name.starts_with(FOLDERS_PREFIX) || listed.name.starts_with(LABELS_PREFIX) {
        return None;
    }
    NAME_ROLES
        .iter()
        .find(|&&(name, _)| listed.name == name)
        .map(|&(_, role)| role)
}

/// The logical message's provider key: Bridge's injected id (spec/02,
/// 04). The unfolded value, trimmed, must be non-empty printable
/// ASCII; several fields with different values are ambiguous
/// evidence and group nothing.
pub(super) fn grouping_key(header_literal: &[u8]) -> Option<String> {
    let fields = header_field_values(header_literal, GROUPING_FIELD);
    let mut values = fields.iter().map(|value| value.trim_ascii());
    let first = values.next()?;
    if values.any(|value| value != first) {
        return None;
    }
    let key = str::from_utf8(first).ok()?;
    if key.is_empty() || !key.bytes().all(|byte| byte.is_ascii_graphic()) {
        return None;
    }
    Some(key.to_owned())
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
    fn bridge_mailboxes_map_to_kinds_roles_and_selectable() {
        // spec/04's mailbox row: Bridge's full listing.
        let cases: &[Case] = &[
            (
                "INBOX",
                &[],
                MailboxKind::Folder,
                Some(MailboxRole::Inbox),
                true,
            ),
            (
                "Drafts",
                &["\\Drafts"],
                MailboxKind::Folder,
                Some(MailboxRole::Drafts),
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
                "Starred",
                &["\\Flagged"],
                MailboxKind::Virtual,
                Some(MailboxRole::Flagged),
                true,
            ),
            (
                "Archive",
                &["\\Archive"],
                MailboxKind::Folder,
                Some(MailboxRole::Archive),
                true,
            ),
            (
                "Spam",
                &["\\Junk"],
                MailboxKind::Folder,
                Some(MailboxRole::Junk),
                true,
            ),
            (
                "Trash",
                &["\\Trash"],
                MailboxKind::Folder,
                Some(MailboxRole::Trash),
                true,
            ),
            (
                "All Mail",
                &["\\All"],
                MailboxKind::Virtual,
                Some(MailboxRole::All),
                true,
            ),
            ("Scheduled", &[], MailboxKind::Virtual, None, true),
            ("Folders", &["\\Noselect"], MailboxKind::Folder, None, false),
            ("Folders/Work", &[], MailboxKind::Folder, None, true),
            ("Folders/Work/2026", &[], MailboxKind::Folder, None, true),
            ("Labels", &["\\Noselect"], MailboxKind::Label, None, false),
            ("Labels/Important", &[], MailboxKind::Label, None, true),
        ];
        for &(name, spellings, kind, role, selectable) in cases {
            let attributes = attribute_list(spellings);
            let mapping = Policy::ProtonBridge.map_mailbox(&listed(name, &attributes));
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
    fn an_attribute_role_wins_over_the_name() {
        let attributes = attribute_list(&["\\Junk"]);
        let mapping = Policy::ProtonBridge.map_mailbox(&listed("Sent", &attributes));
        assert_eq!(mapping.role, Some(MailboxRole::Junk));
    }

    #[test]
    fn attributes_still_apply_inside_folders_and_labels() {
        // Only name-derived roles are suppressed in the user
        // namespaces; special-use attributes still carry a role.
        let attributes = attribute_list(&["\\Sent"]);
        let mapping = Policy::ProtonBridge.map_mailbox(&listed("Folders/Keep", &attributes));
        assert_eq!(mapping.role, Some(MailboxRole::Sent));
        assert_eq!(mapping.kind, MailboxKind::Folder);
    }

    #[test]
    fn well_known_names_supply_roles_without_attributes() {
        // spec/04's fallback when no special-use attribute names a
        // role: every well-known name, and Scheduled's lack of one.
        let attributes = attribute_list(&["\\HasNoChildren"]);
        let expected = [
            ("INBOX", Some(MailboxRole::Inbox)),
            ("Drafts", Some(MailboxRole::Drafts)),
            ("Sent", Some(MailboxRole::Sent)),
            ("Starred", Some(MailboxRole::Flagged)),
            ("Archive", Some(MailboxRole::Archive)),
            ("Spam", Some(MailboxRole::Junk)),
            ("Trash", Some(MailboxRole::Trash)),
            ("All Mail", Some(MailboxRole::All)),
            ("Scheduled", None),
        ];
        for (name, role) in expected {
            let mapping = Policy::ProtonBridge.map_mailbox(&listed(name, &attributes));
            assert_eq!(mapping.role, role, "mailbox {name}");
        }
    }

    #[test]
    fn names_inside_folders_and_labels_give_no_role() {
        for name in ["Folders/Sent", "Labels/Trash"] {
            let mapping = Policy::ProtonBridge.map_mailbox(&listed(name, &[]));
            assert_eq!(mapping.role, None, "mailbox {name}");
        }
    }

    #[test]
    fn inbox_matches_case_insensitively() {
        for name in ["INBOX", "inbox", "Inbox"] {
            let mapping = Policy::ProtonBridge.map_mailbox(&listed(name, &[]));
            assert_eq!(mapping.role, Some(MailboxRole::Inbox), "mailbox {name}");
            assert_eq!(mapping.kind, MailboxKind::Folder);
        }
    }

    #[test]
    fn grouping_key_reads_the_injected_id() {
        assert_eq!(
            Policy::ProtonBridge.grouping_key(b"Subject: x\r\nX-Pm-Internal-Id: pm-1234\r\n"),
            Some("pm-1234".to_owned())
        );
    }

    #[test]
    fn grouping_key_matches_field_names_case_insensitively() {
        assert_eq!(
            Policy::ProtonBridge.grouping_key(b"x-pm-internal-id: pm-1234\n"),
            Some("pm-1234".to_owned())
        );
    }

    #[test]
    fn grouping_key_accepts_lf_lines_and_folds() {
        // An LF-folded field still names the id once unfolded and
        // trimmed.
        assert_eq!(
            Policy::ProtonBridge.grouping_key(b"X-Pm-Internal-Id:\n\t pm-1234\n"),
            Some("pm-1234".to_owned())
        );
    }

    #[test]
    fn grouping_key_rejects_missing_or_unusable_values() {
        for literal in [
            b"Subject: nothing\r\n".as_slice(),
            b"X-Pm-Internal-Id:\r\n".as_slice(),
            b"X-Pm-Internal-Id: \t \r\n".as_slice(),
            b"X-Pm-Internal-Id: pm 1234\r\n".as_slice(),
            b"X-Pm-Internal-Id: pm-1234\r\n\tfolded\r\n".as_slice(),
            b"X-Pm-Internal-Id: pm-\xce\xa9\r\n".as_slice(),
        ] {
            assert_eq!(
                Policy::ProtonBridge.grouping_key(literal),
                None,
                "{literal:?}"
            );
        }
    }

    #[test]
    fn grouping_key_reads_only_the_exact_field_name() {
        // A longer name sharing the prefix is another field.
        assert_eq!(
            Policy::ProtonBridge.grouping_key(b"X-Pm-Internal-Id-Other: pm-9\r\n"),
            None
        );
        assert_eq!(
            Policy::ProtonBridge
                .grouping_key(b"X-Pm-Internal-Id-Other: pm-9\r\nX-Pm-Internal-Id: pm-1\r\n"),
            Some("pm-1".to_owned())
        );
    }

    #[test]
    fn grouping_key_rejects_ambiguous_duplicates() {
        assert_eq!(
            Policy::ProtonBridge
                .grouping_key(b"X-Pm-Internal-Id: one\r\nX-Pm-Internal-Id: two\r\n"),
            None
        );
        assert_eq!(
            Policy::ProtonBridge
                .grouping_key(b"X-Pm-Internal-Id: same\r\nX-PM-INTERNAL-ID: same\r\n"),
            Some("same".to_owned())
        );
    }
}
