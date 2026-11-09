//! Provider policies (spec/04 "Structure"): a policy declares the
//! provider's dialect, what each listed mailbox means and which
//! evidence groups occurrences into one logical message, so the
//! engine can plan sync while `sb-imap` stays dialect-free. The pilot
//! ships `proton_bridge` and `generic_imap` (Dovecot in CI); the
//! account worker adds their caller and later steps their mutation
//! rules.

mod generic_imap;
mod proton_bridge;

use crate::{MailboxKind, MailboxRole};

/// RFC 6154's special-use attributes and the `mailboxes.role` each
/// names (spec/04). There is no `\Inbox`; INBOX is recognized by
/// name.
const SPECIAL_USE_ATTRIBUTES: &[(&str, MailboxRole)] = &[
    ("\\All", MailboxRole::All),
    ("\\Archive", MailboxRole::Archive),
    ("\\Drafts", MailboxRole::Drafts),
    ("\\Flagged", MailboxRole::Flagged),
    ("\\Junk", MailboxRole::Junk),
    ("\\Sent", MailboxRole::Sent),
    ("\\Trash", MailboxRole::Trash),
];

/// LIST attributes that refuse selection (spec/04): `\Noselect`
/// marks hierarchy-only parents, `\NonExistent` a stale subscription.
const UNSELECTABLE_ATTRIBUTES: &[&str] = &["\\Noselect", "\\NonExistent"];

/// IMAP's one mailbox name with a mandatory case-insensitive reading
/// (RFC 3501).
const INBOX_NAME: &str = "INBOX";

/// One LIST row, shaped like `sb-imap`'s `MailboxSummary` but declared
/// here so the engine never depends on transport types (spec/01 rule
/// 3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ListedMailbox<'a> {
    /// The decoded mailbox name.
    pub name: &'a str,
    /// The hierarchy delimiter, or none for a flat namespace.
    pub delimiter: Option<char>,
    /// The LIST attributes as the server spelled them.
    pub attributes: &'a [String],
}

/// The `mailboxes` columns a policy derives from one listing
/// (spec/02).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MailboxMapping {
    /// The `mailboxes.kind` the policy assigns.
    pub kind: MailboxKind,
    /// The `mailboxes.role` the evidence yields, if any.
    pub role: Option<MailboxRole>,
    /// Whether the mailbox accepts SELECT.
    pub selectable: bool,
}

/// An account's provider dialect (spec/04): the pilot's two policies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Policy {
    /// Proton Bridge (spec/04 "Proton Bridge facts").
    ProtonBridge,
    /// Unprofiled IMAP servers; Dovecot in CI.
    GenericImap,
}

impl Policy {
    /// Map one listed mailbox to its `mailboxes` shape. `selectable`
    /// is a protocol rule shared by every policy; `kind` and `role`
    /// are the policy's own.
    pub(crate) fn map_mailbox(&self, listed: &ListedMailbox<'_>) -> MailboxMapping {
        let (kind, role) = match self {
            Self::ProtonBridge => (proton_bridge::kind(listed), proton_bridge::role(listed)),
            Self::GenericImap => (generic_imap::kind(listed), generic_imap::role(listed)),
        };
        MailboxMapping {
            kind,
            role,
            selectable: selectable(listed.attributes),
        }
    }

    /// The provider evidence that groups occurrences into one logical
    /// message (spec/02), read from a fetched header-fields literal.
    /// Equal bytes or an equal Message-ID never group.
    pub(crate) fn grouping_key(&self, header_literal: &[u8]) -> Option<String> {
        match self {
            Self::ProtonBridge => proton_bridge::grouping_key(header_literal),
            Self::GenericImap => generic_imap::grouping_key(header_literal),
        }
    }

    /// The header the policy reads as provider identity, if it has
    /// one (spec/04).
    pub(crate) fn identity_header(&self) -> Option<&'static str> {
        match self {
            Self::ProtonBridge => proton_bridge::IDENTITY_HEADER,
            Self::GenericImap => generic_imap::IDENTITY_HEADER,
        }
    }
}

/// The `mailboxes.role` one LIST attribute names, or `None` for other
/// attributes. Spellings match case-insensitively (spec/04).
fn special_use_role(attribute: &str) -> Option<MailboxRole> {
    SPECIAL_USE_ATTRIBUTES
        .iter()
        .find(|&&(spelling, _)| attribute.eq_ignore_ascii_case(spelling))
        .map(|&(_, role)| role)
}

/// The mailbox's role from its LIST attributes: the first special-use
/// attribute wins (spec/04).
fn attribute_role(attributes: &[String]) -> Option<MailboxRole> {
    attributes
        .iter()
        .find_map(|attribute| special_use_role(attribute))
}

/// Whether the listing allows selecting the mailbox: `\Noselect` and
/// `\NonExistent`, matched case-insensitively, refuse it (spec/04).
fn selectable(attributes: &[String]) -> bool {
    !attributes.iter().any(|attribute| {
        UNSELECTABLE_ATTRIBUTES
            .iter()
            .any(|&spelling| attribute.eq_ignore_ascii_case(spelling))
    })
}

/// Every unfolded value `field_name` carries in a fetched
/// header-fields literal (RFC 5322): `Name: value` lines separated by
/// CRLF or LF, and a line starting with SP or HTAB unfolds into the
/// field before it. Names match ASCII case-insensitively.
fn header_field_values(literal: &[u8], field_name: &str) -> Vec<Vec<u8>> {
    let mut values: Vec<Vec<u8>> = Vec::new();
    // Whether the field the last line read belongs to `field_name`;
    // fold lines only extend the field they follow.
    let mut matched = false;
    for line in literal.split(|byte| *byte == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.is_empty() {
            matched = false;
            continue;
        }
        if matches!(line[0], b' ' | b'\t') {
            if matched && let Some(value) = values.last_mut() {
                value.extend_from_slice(line);
            }
            continue;
        }
        let Some(colon) = line.iter().position(|byte| *byte == b':') else {
            matched = false;
            continue;
        };
        matched = line[..colon].eq_ignore_ascii_case(field_name.as_bytes());
        if matched {
            values.push(line[colon + 1..].to_vec());
        }
    }
    values
}

/// `MailboxSummary.attributes`-shaped owned spellings (tests).
#[cfg(test)]
fn attribute_list(spellings: &[&str]) -> Vec<String> {
    spellings
        .iter()
        .map(|spelling| (*spelling).to_owned())
        .collect()
}

/// A `ListedMailbox` on Bridge's `/` delimiter (tests).
#[cfg(test)]
fn listed<'a>(name: &'a str, attributes: &'a [String]) -> ListedMailbox<'a> {
    ListedMailbox {
        name,
        delimiter: Some('/'),
        attributes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn special_use_attributes_name_roles_case_insensitively() {
        assert_eq!(
            attribute_role(&attribute_list(&["\\drafts"])),
            Some(MailboxRole::Drafts)
        );
        assert_eq!(
            attribute_role(&attribute_list(&["\\Sent", "\\Archive"])),
            Some(MailboxRole::Sent)
        );
        assert_eq!(attribute_role(&attribute_list(&["\\HasChildren"])), None);
        assert_eq!(attribute_role(&[]), None);
    }

    #[test]
    fn noselect_and_nonexistent_refuse_selection() {
        for spelling in ["\\Noselect", "\\nonexistent"] {
            assert!(!selectable(&attribute_list(&[spelling])), "{spelling}");
        }
        assert!(selectable(&attribute_list(&["\\HasNoChildren"])));
        assert!(selectable(&[]));
    }

    #[test]
    fn header_field_values_matches_names_case_insensitively() {
        let literal = b"X-Test: one\r\nOther: x\nX-TEST: two\n";
        assert_eq!(
            header_field_values(literal, "x-test"),
            vec![b" one".to_vec(), b" two".to_vec()]
        );
    }

    #[test]
    fn header_field_values_unfolds_continuation_lines() {
        let literal = b"X-Test: one\r\n\tand more\nX-Other: separate\n";
        assert_eq!(
            header_field_values(literal, "X-Test"),
            vec![b" one\tand more".to_vec()]
        );
    }

    #[test]
    fn header_field_values_skips_malformed_lines() {
        let literal = b"no colon here\n X-Test: fold of nothing\nX-Test: v\r\n";
        assert_eq!(header_field_values(literal, "X-Test"), vec![b" v".to_vec()]);
    }

    #[test]
    fn the_policies_declare_their_identity_headers() {
        assert_eq!(
            Policy::ProtonBridge.identity_header(),
            Some("X-Pm-Internal-Id")
        );
        assert_eq!(Policy::GenericImap.identity_header(), None);
    }
}
