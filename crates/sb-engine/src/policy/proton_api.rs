//! The `proton_api` policy (spec/04-sync-proton "Mailbox mapping"):
//! which mailbox each Proton label is. System locations come from the
//! fixed table, since every Proton account has them; folders and
//! labels come from the listing, keyed by label id so a rename keeps
//! its mailbox.

use std::collections::BTreeSet;

use sb_resources::proton_sync::ObservedLabel;
use sb_resources::{Label, LabelKind, MailboxKind, MailboxRole};

/// Proton joins a folder's path with `/` (research `mail-sync.md` §1).
const FOLDER_DELIMITER: char = '/';

/// The mapped system locations: label id, mailbox name, kind and role.
/// All Drafts, All Sent and Outbox aren't mailboxes (spec/04).
const SYSTEM_MAILBOXES: [(&str, &str, MailboxKind, Option<MailboxRole>); 9] = [
    ("0", "Inbox", MailboxKind::Folder, Some(MailboxRole::Inbox)),
    (
        "8",
        "Drafts",
        MailboxKind::Folder,
        Some(MailboxRole::Drafts),
    ),
    ("7", "Sent", MailboxKind::Folder, Some(MailboxRole::Sent)),
    (
        "6",
        "Archive",
        MailboxKind::Folder,
        Some(MailboxRole::Archive),
    ),
    ("4", "Spam", MailboxKind::Folder, Some(MailboxRole::Junk)),
    ("3", "Trash", MailboxKind::Folder, Some(MailboxRole::Trash)),
    (
        "10",
        "Starred",
        MailboxKind::Virtual,
        Some(MailboxRole::Flagged),
    ),
    (
        "5",
        "All Mail",
        MailboxKind::Virtual,
        Some(MailboxRole::All),
    ),
    ("12", "Scheduled", MailboxKind::Virtual, None),
];

/// The account's mailboxes for one label listing: the system
/// locations, then folders by path and labels by name, in listing
/// order. Proton keeps folders and labels in separate namespaces, but
/// `mailboxes.remote_name` is unique per account, so a name already
/// taken gets the label id appended, as the store does for a gone
/// mailbox it moves aside.
pub(crate) fn observed_labels(labels: &[Label]) -> Vec<ObservedLabel> {
    let mut taken = BTreeSet::new();
    let mut observed = Vec::with_capacity(SYSTEM_MAILBOXES.len() + labels.len());
    for (id, name, kind, role) in SYSTEM_MAILBOXES {
        taken.insert(name.to_owned());
        observed.push(ObservedLabel {
            provider_key: id.to_owned(),
            remote_name: name.to_owned(),
            delimiter: None,
            kind,
            role,
            selectable: true,
        });
    }
    for label in labels {
        let (name, kind, delimiter) = match label.kind {
            LabelKind::System => continue,
            LabelKind::Folder => (&label.path, MailboxKind::Folder, Some(FOLDER_DELIMITER)),
            LabelKind::Label => (&label.name, MailboxKind::Label, None),
        };
        let mut remote_name = name.clone();
        if !taken.insert(remote_name.clone()) {
            remote_name = format!("{name} ({})", label.id);
            taken.insert(remote_name.clone());
        }
        observed.push(ObservedLabel {
            provider_key: label.id.clone(),
            remote_name,
            delimiter,
            kind,
            role: None,
            selectable: true,
        });
    }
    observed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn label(id: &str, kind: LabelKind, name: &str, path: &str) -> Label {
        Label {
            id: id.to_owned(),
            parent_id: None,
            name: name.to_owned(),
            path: path.to_owned(),
            color: "#000000".to_owned(),
            kind,
        }
    }

    #[test]
    fn system_locations_follow_the_table_and_unmapped_ones_are_left_out() {
        // All Drafts (1), All Sent (2) and Outbox (9) are listed but
        // aren't mailboxes; the table's locations exist even unlisted.
        let listed = [
            label("1", LabelKind::System, "All Drafts", "All Drafts"),
            label("2", LabelKind::System, "All Sent", "All Sent"),
            label("9", LabelKind::System, "Outbox", "Outbox"),
        ];
        let observed = observed_labels(&listed);
        let keys: Vec<_> = observed.iter().map(|o| o.provider_key.as_str()).collect();
        assert_eq!(keys, ["0", "8", "7", "6", "4", "3", "10", "5", "12"]);
        let inbox = &observed[0];
        assert_eq!(
            (inbox.remote_name.as_str(), inbox.kind, inbox.role),
            ("Inbox", MailboxKind::Folder, Some(MailboxRole::Inbox))
        );
        let all = observed.iter().find(|o| o.provider_key == "5").unwrap();
        assert_eq!(
            (all.kind, all.role),
            (MailboxKind::Virtual, Some(MailboxRole::All))
        );
        let scheduled = observed.iter().find(|o| o.provider_key == "12").unwrap();
        assert_eq!(
            (scheduled.kind, scheduled.role),
            (MailboxKind::Virtual, None)
        );
    }

    #[test]
    fn folders_take_their_path_labels_their_name_and_names_stay_unique() {
        let listed = [
            label("f1", LabelKind::Folder, "Receipts", "Work/Receipts"),
            label("l1", LabelKind::Label, "Work/Receipts", "Work/Receipts"),
            label("l2", LabelKind::Label, "Inbox", "Inbox"),
            label("l3", LabelKind::Label, "Travel", "Travel"),
        ];
        let observed = observed_labels(&listed);
        let users: Vec<_> = observed[SYSTEM_MAILBOXES.len()..]
            .iter()
            .map(|o| {
                (
                    o.provider_key.as_str(),
                    o.remote_name.as_str(),
                    o.kind,
                    o.delimiter,
                )
            })
            .collect();
        assert_eq!(
            users,
            [
                ("f1", "Work/Receipts", MailboxKind::Folder, Some('/')),
                ("l1", "Work/Receipts (l1)", MailboxKind::Label, None),
                ("l2", "Inbox (l2)", MailboxKind::Label, None),
                ("l3", "Travel", MailboxKind::Label, None),
            ]
        );
        assert!(observed.iter().all(|o| o.selectable));
        assert!(
            observed[SYSTEM_MAILBOXES.len()..]
                .iter()
                .all(|o| o.role.is_none())
        );
    }
}
