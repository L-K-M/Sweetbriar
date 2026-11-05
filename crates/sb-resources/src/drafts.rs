//! The draft attachment's publication path (spec/02, spec/03
//! "Publication protocol"): bytes become a durable object plus its
//! one-shot reservation before the revision that references them is
//! written. The draft row work itself is `sb_store::drafts`, used
//! through [`Resources::write`]/[`Resources::read`].

use std::io::Read;

use sb_store::drafts::{AttachmentRef, attachment_source};
use sb_store::{ObjectStore, PublishedObject, StoreError};

use crate::Resources;

impl Resources {
    /// Publish one draft attachment's bytes: stream, hash, flush,
    /// reserve, install, flush (spec/03's order). The returned
    /// [`PublishedObject`] carries the reservation the revision's
    /// commit spends; an abandoned one pins the object until startup
    /// expiry. Blocking file I/O; run on a blocking thread, never the
    /// writer's.
    pub fn publish_draft_attachment(
        &self,
        bytes: impl Read,
    ) -> Result<PublishedObject, StoreError> {
        self.store.objects().publish(bytes)
    }

    /// Republish one stored attachment reference from an exact
    /// immutable draft revision (spec/02, spec/03, spec/07). The
    /// revision must belong to `draft_id` and `reference` must match
    /// one of its stored attachments in hash, size, filename and media
    /// type; an older, noncurrent or terminal revision is a valid
    /// source because this creates a new publication, never a mutation
    /// of that draft. The source object is opened through the ordinary
    /// verifying reader and published through the real object store, so
    /// the returned [`PublishedObject`] is a fresh publication with its
    /// own reservation for a following save to spend once. No receipt,
    /// no new revision or draft row. Blocking file I/O; run on a
    /// blocking thread.
    pub fn republish_draft_attachment(
        &self,
        draft_id: &str,
        revision_id: &str,
        reference: &AttachmentRef,
    ) -> Result<PublishedObject, StoreError> {
        let source = self.read(|tx| attachment_source(tx, draft_id, revision_id, reference))?;
        let reader = self.store.objects().open(&source)?;
        self.store.objects().publish(reader)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use sb_store::ObjectStore;
    use sb_test_support::{ScratchDir, block_on};

    use super::*;
    use crate::{
        ComposeFormat, DraftAttachment, DraftRecipient, DraftSource, RevisionInput, Timestamp,
        WritePriority, create_draft, save_draft,
    };

    fn input(revision_id: &str) -> RevisionInput {
        RevisionInput {
            revision_id: revision_id.to_owned(),
            identity_id: None,
            format: ComposeFormat::Plain,
            plain_flowed: true,
            subject: "a subject".to_owned(),
            to: vec![DraftRecipient {
                address: "to@example.test".to_owned(),
                name: None,
            }],
            cc: vec![],
            bcc: vec![],
            body_source: "the source text".to_owned(),
            attachments: vec![],
        }
    }

    fn save_with_attachment(
        res: &Resources,
        draft_id: &str,
        revision_id: &str,
        object: Arc<PublishedObject>,
    ) {
        let mut content = input(revision_id);
        content.attachments = vec![DraftAttachment::New {
            object,
            filename: "one.txt".to_owned(),
            media_type: "text/plain".to_owned(),
        }];
        let id = draft_id.to_owned();
        block_on(res.write(WritePriority::User, move |tx| {
            save_draft(
                tx,
                &id,
                None,
                content,
                Timestamp::from_millis(1_735_689_600_000),
            )
        }))
        .unwrap();
    }

    /// The source revision's reference and the fresh reservation both
    /// protect the object from GC until the following save commits, and
    /// the save's own reference keeps it afterwards.
    #[test]
    fn a_republished_copy_is_protected_until_its_save_commits() {
        let scratch = ScratchDir::new("draft-republish-gc");
        let res = Resources::open(&scratch.path().join("store")).unwrap();
        let now = Timestamp::from_millis(1_735_689_600_000);
        block_on(res.write(WritePriority::User, move |tx| {
            create_draft(tx, "d1", &DraftSource::New, now)
        }))
        .unwrap();
        let object = res
            .publish_draft_attachment(b"attachment one".as_slice())
            .unwrap();
        save_with_attachment(&res, "d1", "r1", Arc::new(object));
        let reference = res
            .read(|tx| crate::draft(tx, "d1"))
            .unwrap()
            .unwrap()
            .revision
            .unwrap()
            .attachments[0]
            .clone();

        let copy = res
            .republish_draft_attachment("d1", "r1", &reference)
            .unwrap();
        let report = res.store.collect_garbage(Duration::ZERO).unwrap();
        assert_eq!((report.scanned, report.removed, report.kept), (1, 0, 1));
        assert!(res.store.objects().exists(copy.id()).unwrap());

        block_on(res.write(WritePriority::User, move |tx| {
            create_draft(tx, "d2", &DraftSource::New, now)
        }))
        .unwrap();
        save_with_attachment(&res, "d2", "r2", Arc::new(copy));
        let report = res.store.collect_garbage(Duration::ZERO).unwrap();
        assert_eq!((report.scanned, report.removed, report.kept), (1, 0, 1));
        res.close().unwrap();
    }
}
