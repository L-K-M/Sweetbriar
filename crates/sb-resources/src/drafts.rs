//! The draft attachment's publication path (spec/02, spec/03
//! "Publication protocol"): bytes become a durable object plus its
//! one-shot reservation before the revision that references them is
//! written. The draft row work itself is `sb_store::drafts`, used
//! through [`Resources::write`]/[`Resources::read`].

use std::io::{self, Read, Seek};
use std::path::{Component, Path};

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

    /// Publish an explicitly selected file without buffering it in memory.
    /// The final component must be a regular file, not a symbolic link;
    /// `open_no_follow` validates the opened handle and does not block on a
    /// pipe or device. A later rename cannot redirect the held reader.
    pub fn publish_draft_attachment_file(
        &self,
        path: &Path,
    ) -> Result<PublishedObject, StoreError> {
        if !path.is_absolute()
            || path.as_os_str().as_encoded_bytes().contains(&0)
            || path
                .components()
                .any(|component| component == Component::ParentDir)
        {
            return Err(StoreError::InvalidInput {
                message: "attachment path must be absolute without traversal".into(),
            });
        }
        let mut file = self.fs.open_no_follow(path).map_err(|error| {
            if error.kind() == io::ErrorKind::InvalidInput {
                StoreError::InvalidInput {
                    message: "attachment must be a regular file".into(),
                }
            } else {
                StoreError::Unavailable {
                    reason: format!("cannot open selected attachment: {error}"),
                    io_kind: Some(error.kind()),
                }
            }
        })?;
        self.publish_selected(&mut *file)
    }

    /// Stream an opened selection. The held descriptor pins the inode, so
    /// its size before and after the stream proves whether the file's
    /// length changed while it was read — a torn copy from a concurrent
    /// shrink or growth fails instead of publishing. An in-place rewrite
    /// that keeps the length can't be seen this way. Blocking file I/O;
    /// run on a blocking thread, never the writer's.
    fn publish_selected<R: Read + Seek + ?Sized>(
        &self,
        file: &mut R,
    ) -> Result<PublishedObject, StoreError> {
        let io_failure = |error: io::Error| StoreError::Unavailable {
            reason: format!("cannot read selected attachment: {error}"),
            io_kind: Some(error.kind()),
        };
        let opened = file
            .seek(io::SeekFrom::End(0))
            .and_then(|length| file.seek(io::SeekFrom::Start(0)).map(|_| length))
            .map_err(io_failure)?;
        let published = self.store.objects().publish(&mut *file)?;
        let settled = file.seek(io::SeekFrom::End(0)).map_err(io_failure)?;
        if opened != settled || published.id().byte_length() != opened {
            return Err(StoreError::Unavailable {
                reason: "selected attachment changed while it was read".into(),
                io_kind: Some(io::ErrorKind::InvalidData),
            });
        }
        Ok(published)
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
    /// blocking thread, never the writer's.
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
    use sb_test_support::{FaultFs, FaultKind, FsOp, ScratchDir, block_on, real_fs};

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
            content_id: None,
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

    #[test]
    fn selected_file_publication_streams_and_survives_a_saved_revision() {
        let scratch = ScratchDir::new("draft-selected-file");
        let fs = Arc::new(FaultFs::new(
            real_fs(),
            FsOp::Read,
            u64::MAX,
            FaultKind::Eio,
        ));
        let res = Resources::open_on(
            &scratch.path().join("store"),
            fs.clone(),
            Arc::new(sb_platform::SystemClock),
        )
        .unwrap();
        let bytes = vec![0x5a; 2 * 1024 * 1024 + 17];
        let path = scratch.path().join("input.bin");
        std::fs::write(&path, &bytes).unwrap();
        let reads = fs.calls(FsOp::Read);
        let object = res.publish_draft_attachment_file(&path).unwrap();
        let object_id = *object.id();
        assert!(
            fs.calls(FsOp::Read) - reads > 1,
            "input must be read in chunks"
        );
        let mut actual = Vec::new();
        res.store
            .objects()
            .open(object.id())
            .unwrap()
            .read_to_end(&mut actual)
            .unwrap();
        assert_eq!(actual, bytes);
        let now = Timestamp::from_millis(1_735_689_600_000);
        block_on(res.write(WritePriority::User, move |tx| {
            create_draft(tx, "d1", &DraftSource::New, now)
        }))
        .unwrap();
        save_with_attachment(&res, "d1", "r1", Arc::new(object));
        let saved = res.read(|tx| crate::draft(tx, "d1")).unwrap().unwrap();
        assert_eq!(
            saved.revision.unwrap().attachments[0].size,
            bytes.len() as u64
        );
        res.close().unwrap();
        let reopened = Resources::open(&scratch.path().join("store")).unwrap();
        let restored = reopened.read(|tx| crate::draft(tx, "d1")).unwrap().unwrap();
        assert_eq!(
            restored.revision.unwrap().attachments[0].size,
            bytes.len() as u64
        );
        let mut restored_bytes = Vec::new();
        reopened
            .store
            .objects()
            .open(&object_id)
            .unwrap()
            .read_to_end(&mut restored_bytes)
            .unwrap();
        assert_eq!(restored_bytes, bytes);
        reopened.close().unwrap();
    }

    #[test]
    fn selected_file_read_failure_returns_no_handle_and_a_later_retry_succeeds() {
        let probe_dir = ScratchDir::new("draft-selected-read-probe");
        let probe = Arc::new(FaultFs::new(
            real_fs(),
            FsOp::Read,
            u64::MAX,
            FaultKind::Eio,
        ));
        let opened = Resources::open_on(
            &probe_dir.path().join("store"),
            probe.clone(),
            Arc::new(sb_platform::SystemClock),
        )
        .unwrap();
        let startup_reads = probe.calls(FsOp::Read);
        opened.close().unwrap();

        let scratch = ScratchDir::new("draft-selected-read-fault");
        let fs = Arc::new(FaultFs::new(
            real_fs(),
            FsOp::Read,
            startup_reads + 2,
            FaultKind::Eio,
        ));
        let res = Resources::open_on(
            &scratch.path().join("store"),
            fs.clone(),
            Arc::new(sb_platform::SystemClock),
        )
        .unwrap();
        assert_eq!(fs.calls(FsOp::Read), startup_reads);
        let path = scratch.path().join("input.bin");
        std::fs::write(&path, vec![0x7f; 2 * 1024 * 1024]).unwrap();
        assert!(matches!(
            res.publish_draft_attachment_file(&path),
            Err(StoreError::Unavailable { .. })
        ));
        assert_eq!(fs.calls(FsOp::Read), startup_reads + 2);
        let object = res.publish_draft_attachment_file(&path).unwrap();
        assert!(res.store.objects().exists(object.id()).unwrap());
        res.close().unwrap();
    }

    #[test]
    fn selected_file_publication_rejects_invalid_paths_and_nonfiles() {
        let scratch = ScratchDir::new("draft-selected-invalid");
        let res = Resources::open(&scratch.path().join("store")).unwrap();
        for path in [
            Path::new("relative"),
            Path::new("/tmp/../file"),
            Path::new("/tmp/\0bad"),
        ] {
            assert!(matches!(
                res.publish_draft_attachment_file(path),
                Err(StoreError::InvalidInput { .. })
            ));
        }
        assert!(matches!(
            res.publish_draft_attachment_file(scratch.path()),
            Err(StoreError::InvalidInput { .. })
        ));
        assert!(matches!(
            res.publish_draft_attachment_file(&scratch.path().join("missing")),
            Err(StoreError::Unavailable { .. })
        ));
        res.close().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn selected_file_publication_refuses_links_and_fifos() {
        use std::os::unix::fs::symlink;
        let scratch = ScratchDir::new("draft-selected-links");
        let res = Resources::open(&scratch.path().join("store")).unwrap();
        let file = scratch.path().join("real");
        std::fs::write(&file, b"contents").unwrap();
        let link = scratch.path().join("link");
        symlink(&file, &link).unwrap();
        assert!(matches!(
            res.publish_draft_attachment_file(&link),
            Err(StoreError::InvalidInput { .. })
        ));
        let fifo = scratch.path().join("fifo");
        assert!(
            std::process::Command::new("mkfifo")
                .arg(&fifo)
                .status()
                .unwrap()
                .success()
        );
        assert!(matches!(
            res.publish_draft_attachment_file(&fifo),
            Err(StoreError::InvalidInput { .. })
        ));
        res.close().unwrap();
    }

    /// Reads `inner` but writes `writer` once, the way an editor saving
    /// over the picked file mid-stream would change its length. Both
    /// handles share one inode, so the pinned descriptor's post-stream
    /// size exposes the change.
    struct MutateOnRead {
        inner: std::fs::File,
        writer: std::fs::File,
        size: u64,
        fired: bool,
    }

    impl io::Read for MutateOnRead {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if !self.fired {
                self.fired = true;
                self.writer.set_len(self.size).unwrap();
            }
            self.inner.read(buf)
        }
    }

    impl io::Seek for MutateOnRead {
        fn seek(&mut self, position: io::SeekFrom) -> io::Result<u64> {
            self.inner.seek(position)
        }
    }

    #[test]
    fn selected_file_changed_while_streaming_is_an_explicit_failure() {
        for size in [8u64, 65_536] {
            let scratch = ScratchDir::new("draft-selected-mutated");
            let res = Resources::open(&scratch.path().join("store")).unwrap();
            let path = scratch.path().join("picked");
            std::fs::write(&path, vec![7u8; 512]).unwrap();
            let inner = std::fs::File::open(&path).unwrap();
            let writer = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
            let mut reader = MutateOnRead {
                inner,
                writer,
                size,
                fired: false,
            };
            assert!(
                matches!(
                    res.publish_selected(&mut reader),
                    Err(StoreError::Unavailable { .. })
                ),
                "size {size}"
            );
            res.close().unwrap();
        }
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
