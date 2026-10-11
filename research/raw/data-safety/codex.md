<!-- provenance: helper=codex model=gpt-6.1-sol started=2026-10-11T18:52:49+00:00 exit=0 brief=research/briefs/ -->

# Sweetbriar: guaranteeing data safety

**Research date: 2026-10-11.** This report distinguishes documented behavior and incidents from proposed Sweetbriar policy. Recommendations, thresholds and implementation choices below are design judgments, not claims that existing software already provides these guarantees. Live documentation and dependency status were checked on the research date.

The platform correction is confirmed: Apple lists **macOS 27 Golden Gate, released 2026-09-14**, and **27.0.1, released 2026-09-28**. macOS 27 supports Apple silicon; macOS 26 Tahoe remains relevant, with **26.7.1 released 2026-09-28**. [Apple security releases](https://support.apple.com/en-us/100100), [macOS 27 compatibility](https://support.apple.com/en-ca/127255).

## 1. TL;DR

- **Make safety a contract about acknowledged operations.** “Saved” must mean durable; “submitted” must mean accepted by the submission service. Generic SMTP cannot guarantee exactly-once delivery after a lost acceptance reply. [RFC 5321](https://datatracker.ietf.org/doc/html/rfc5321).
- **Keep immutable original messages and attachments, with SQLite for authoritative mutable state.** Search, previews and threading are rebuildable projections. Rebuilding them must never rewrite or delete originals.
- **Persist intent before network activity.** Track intermediate steps and server receipts. Retry only operations whose effects are known to be absent or safely idempotent.
- **Make deletion a distinct, authorized operation.** Prohibit client-issued bare EXPUNGE and implicit expunging through CLOSE; retain local recovery copies when remote messages disappear. [UIDPLUS](https://www.rfc-editor.org/rfc/rfc4315), [IMAP4rev2](https://www.rfc-editor.org/rfc/rfc9051).
- **Use WAL with FULL synchronization for irreplaceable state**, plus macOS fullfsync, checked error handling, consistent backups and verified migrations. Qualify the actual linked SQLite build, including its 2026 WAL-reset fix. [SQLite WAL](https://sqlite.org/wal.html).
- **Treat drafts and the outbox as primary user data.** Preserve revisions, attachment bytes, recipients and uncertain send outcomes across crashes and restores.
- **Render hostile mail with multiple independent controls.** Disable content JavaScript, sanitize HTML/CSS, deny network requests by default and isolate attachment access. JavaScript blocking alone does not stop CSS exfiltration or EFAIL-style attacks. [CSS exfiltration research](https://portswigger.net/research/blind-css-exfiltration), [EFAIL](https://efail.de/).
- **Prove invariants with simulation and real systems.** Test every crash boundary, provider-specific semantics, disk exhaustion, corrupt inputs and recovery from backups. A successful kill -9 test alone does not establish power-loss durability.

## 2. Findings

### 2.1 Real incidents and what they establish

Evidence quality matters. A vendor repair article documents a failure class; a bug with a patch can establish a mechanism; a user's missing-message report usually cannot establish the root cause.

| Documented case | Observed failure and root-cause evidence | Design that would prevent or contain that mechanism |
|---|---|---|
| Thunderbird's 122+ IMAP corruption cluster; [tracker 1890230](https://bugzilla.mozilla.org/show_bug.cgi?id=1890230), [bug 1923747](https://bugzilla.mozilla.org/show_bug.cgi?id=1923747) | The tracker covers multiple bugs. Bug 1923747 identifies downloads accepted into an IMAP folder while its store was locked; a caller also ignored an error return. Corruption could become more obvious after compaction. This is one identified mechanism, not an explanation for every report in the cluster. | Enforce store ownership at the lowest write layer; reject writes during replacement; propagate errors. Avoid rewriting a whole mailbox to reclaim one message. |
| Thunderbird interrupted compaction and damaged mbox; [Mozilla's explanation](https://support.mozilla.org/en-US/kb/compacting-folders-potential-complications) | An interrupted compaction can leave an empty .msf index, making intact mail appear missing. Separately, compacting an already damaged mbox can truncate the rewritten mailbox and lose subsequent messages. | Separate indexes from originals; validate replacement output before publication; preserve the previous generation. Distinguish “index unavailable” from “message absent.” |
| Antivirus quarantine of Thunderbird stores; [same vendor article](https://support.mozilla.org/en-US/kb/compacting-folders-potential-complications) | Mozilla describes historical antivirus removal or quarantine of an entire mailbox file containing a suspect message. Cached indexes could temporarily conceal the missing store. | Store messages independently; detect missing bytes immediately; retain verified backups. Per-message files reduce the failure's scope but do not make antivirus harmless. |
| Apple Mail migration and moves; [Michael Tsai's incident log](https://mjtsai.com/blog/2019/10/11/mail-data-loss-in-macos-10-15/) | The 2019-10-11 report records missing/incomplete mail after migration and blank messages after moves, with changes propagating through sync. A 2023-06-28 update describes Tsai's own reproduction: reapplying a rule to move messages into their existing IMAP mailbox deleted them. The underlying Apple implementation defect is **(unverified)**; this is not evidence of a current macOS 27 bug. | Make same-destination moves no-ops; compare migration inventories and bytes; preserve source data until destination verification; never propagate a local reconstruction's omissions as deletions. |
| Proton Bridge's Recovered Messages; [Proton documentation](https://proton.me/support/what-is-the-recovered-messages-folder-in-bridge) | Failed APPEND operations are preserved in a device-local recovery folder. Proton explicitly says some clients had already deleted the source despite Bridge returning an error. The recovery folder is not available on other devices. | Copy, verify and only then delete. Preserve recovery data in backups; never assume all Bridge-local data can safely be discarded and reconstructed from Proton. |
| Outlook PST/OST corruption; [Microsoft repair guidance](https://support.microsoft.com/en-us/outlook/repair-outlook-data-files-pst-and-ost) | SCANPST makes a backup, may recover items into Lost and Found and cannot recover every damaged item. Microsoft documents repair limitations, not one universal corruption root cause. | Preserve the original before repair, compare recovered inventories and keep unsynchronized edits independently. A server copy cannot replace local-only data. |
| Gmail IMAP deletion; [ImapSettings API](https://developers.google.com/workspace/gmail/api/reference/rest/v1/ImapSettings) | This is documented configuration behavior, not a proven client bug: autoExpunge can act when \Deleted is set, and expungeBehavior can archive, trash or permanently destroy mail when its last visible IMAP membership is expunged. | Model label removal, archive, Trash and permanent destruction separately. Do not assume the final expunge command is the only destructive boundary. |

Apple's current missing-mail support page also lists view, account, rule and filtering issues, and recommends rebuilding for certain display/search problems. Apparent disappearance therefore needs diagnosis before any destructive “repair.” [Apple Mail troubleshooting](https://support.apple.com/en-gb/117379).

Specific root-cause allegations about Eudora, Airmail, Spark and Mailspring were not verified in this research. They are omitted rather than presented as established incidents. The verified examples already expose distinct hazards: shared-file replacement, ignored errors, migration omissions, premature source deletion and provider semantics.

### 2.2 Sync correctness

**A durable intent queue is necessary, but it does not turn every command into an idempotent command.** IMAP COPY and APPEND can create additional messages if repeated after an unobserved success. RFC 4549 discusses disconnected operation and reconciliation; UIDPLUS supplies receipts that improve identification. [Disconnected IMAP guidance](https://www.rfc-editor.org/rfc/rfc4549), [UIDPLUS](https://www.rfc-editor.org/rfc/rfc4315).

Recommended operation record:

~~~text
operation UUID; account; logical message UUID
source/destination mailbox identities and epochs
desired change; base server version; prerequisite object hashes
phase; attempts; server receipts; last error; originating user action
~~~

Commit this record and the optimistic local view in one transaction before sending anything. Use an explicit state machine: prepared → in flight → confirmed, rejected or outcome unknown. A crash can occur between any network effect and recording its result. An “in flight” record after restart must therefore trigger reconciliation, not unconditional replay.

Setting a desired flag state is safer than replaying a toggle. Even repeated flag changes require conflict policy when another client changes that flag. With CONDSTORE, use UNCHANGEDSINCE and inspect MODIFIED responses; with QRESYNC, process VANISHED and modification sequences. These mechanisms assist concurrency control but do not provide a transaction across an arbitrary batch of messages. [RFC 7162](https://www.rfc-editor.org/rfc/rfc7162).

**Identity and epoch changes.** Persist IMAP references as account + mailbox + UIDVALIDITY + UID. Sequence numbers are transient. On UIDVALIDITY change, invalidate mappings, suspend dependent operations and build a new inventory. Never apply an old queued deletion to the same numeric UID in the new epoch. A Message-ID or content hash can help reconcile; neither should merge logically distinct messages automatically. [IMAP4rev2](https://www.rfc-editor.org/rfc/rfc9051), [Internet Message Format](https://datatracker.ietf.org/doc/html/rfc5322).

**Moves and deletion.** Prefer UID MOVE when supported. MOVE's per-message preservation requirements do not make an entire multi-message request atomic: a failed request can leave some messages moved. Reconcile each affected message. [RFC 6851](https://www.rfc-editor.org/rfc/rfc6851).

For servers requiring COPY followed by deletion, recommend:

1. Persist the move plan; obtain complete source bytes before any operation that could remove the only complete copy.
2. Issue UID COPY and record COPYUID, when supplied; verify destination membership and investigate ambiguous completion.
3. Verify that the provider’s deletion behavior matches the intended action; only then mark confirmed source UIDs \Deleted.
4. Issue UID EXPUNGE only for those UIDs, after applying provider-specific safeguards.
5. Commit final mappings and retain a recovery copy.

UID EXPUNGE limits removal to matching UIDs already marked deleted; bare EXPUNGE can remove messages marked by another client. UIDPLUS receipts are not available in every circumstance. When proof is missing, fetch and reconcile rather than guessing. [RFC 4315](https://www.rfc-editor.org/rfc/rfc4315).

Sweetbriar should prohibit client-issued bare EXPUNGE and CLOSE's implicit expunge; use UNSELECT to leave a mailbox without expunging when supported. Without appropriate capabilities, leave removal pending or offer a non-destructive alternative. This is deliberately stricter than older protocol fallback procedures. Never perform cross-account deletion until a complete destination copy is verified; this includes moving server mail into a local folder.

**Tombstones and conflicts.** Recommended tombstones distinguish explicit local deletion, remotely observed disappearance, folder-membership removal and object destruction. Record provenance and epoch. A remote observation must not become a new outbound delete intent. Keep deleted local bytes in a recovery area; expire them through a separate, reviewable retention policy.

For drafts, preserve both versions on concurrent edits. For memberships, reconcile the particular add/remove intent against current provider state. For flags, use conditional updates where available and record unresolved conflicts; do not choose “last writer wins” from unsynchronized device clocks. Do not resurrect intentional server deletions automatically merely because a retained recovery file exists.

**Incomplete inventories and circuit breakers.** Stage a full inventory and publish it only after every page succeeds. Authentication failure, a timeout, a filtered view or an expired cursor must never mean “zero messages.” Gmail documents HTTP 404 for an obsolete historyId and requires a full sync. [Gmail synchronization](https://developers.google.com/workspace/gmail/api/guides/sync).

Recommended initial breaker: investigate disappearance of an entire previously nonempty mailbox, or an unexplained account-wide loss of at least 20% and 100 messages in a completed scan. These numbers are proposed defaults, not established standards. Include smaller-account and folder-deletion safeguards. Freeze automated destructive work and local recovery garbage collection, retain the old inventory, independently recheck and show affected counts. Explicit batch-deletion intent can explain expected changes; unexplained changes cannot.

**Provider models must remain distinct.**

- **Gmail:** labels are exposed as IMAP folders; X-GM-MSGID identifies a message across memberships and X-GM-LABELS exposes labels. Maintain one logical message with multiple memberships, while retaining provider identity. Inspect deletion settings and visibility limits where accessible; do not silently change account settings. [Gmail extensions](https://developers.google.com/workspace/gmail/imap/imap-extensions), [Gmail IMAP settings](https://developers.google.com/workspace/gmail/api/reference/rest/v1/ImapSettings).
- **Microsoft 365:** Graph's ordinary message IDs can change after moves. Request ImmutableId consistently; it remains stable within a mailbox but not across every archive or export/import operation. Its scope must be part of identity. [Graph immutable IDs](https://learn.microsoft.com/en-us/graph/outlook-immutable-id).
- **JMAP:** state tokens and ifInState permit conditional mutation; EmailSubmission provides a submission object to track. Request creation IDs are not a blanket promise that replaying a lost HTTP response is harmless. Implement each method's actual semantics. [JMAP Core](https://www.rfc-editor.org/rfc/rfc8620), [JMAP Mail](https://datatracker.ietf.org/doc/html/rfc8621).
- **Proton Bridge:** it is a local intermediary with recovery state as well as a remote mailbox. Keep that distinction in error handling and account removal. [Recovered Messages](https://proton.me/support/what-is-the-recovered-messages-folder-in-bridge).

Offline work should remain visible as pending, with cancel, retry and recovery actions. Removing an account must not generate server-deletion commands.

### 2.3 Local durability on macOS and Linux

**SQLite configuration.** WAL with synchronous=FULL synchronizes each committed transaction; NORMAL can lose recent commits on power failure while retaining database consistency. The WAL is part of database state: copying only the main file while it is live can omit committed changes. [SQLite WAL](https://sqlite.org/wal.html).

Recommended authoritative database configuration:

~~~sql
PRAGMA journal_mode=WAL;
PRAGMA synchronous=FULL;
PRAGMA foreign_keys=ON;
-- macOS connections:
PRAGMA fullfsync=ON;
~~~

Read back required settings on every relevant connection. SQLite silently ignores unknown pragmas. fullfsync requests F_FULLFSYNC where supported, currently macOS. checkpoint_fullfsync alone affects checkpoints; when fullfsync is on it adds nothing. Neither changes NORMAL into per-commit durability. [SQLite pragmas](https://sqlite.org/pragma.html).

As checked **2026-10-11**, SQLite documents a concurrent writer/checkpointer WAL-reset corruption bug fixed in **3.51.3, released 2026-03-13**, with backports **3.44.6 and 3.50.7**. Release news lists **3.54.0, released 2026-10-09**. Qualify the actual linked SQLite version/source ID and compile options; a Rust wrapper's version is insufficient. Pin a validated, patched build rather than taking an untested upgrade merely because it is newest. [WAL-reset advisory](https://sqlite.org/wal.html#walresetbug), [SQLite releases](https://sqlite.org/news.html).

Use one authoritative writer actor in the Rust core. Readers receive snapshots; UI code never opens a second mutation path. Keep transactions short and monitor checkpoint progress because long readers can prevent WAL reclamation. Database and rebuildable search state should not require atomic transactions across separate WAL databases. [SQLite WAL](https://sqlite.org/wal.html).

**File durability.** Atomic visibility and durable persistence are different properties. On Linux, rename can atomically replace a destination, but synchronizing a file does not synchronize its directory entry. Cross-filesystem rename is not the same operation. [rename(2)](https://man7.org/linux/man-pages/man2/rename.2.html), [fsync(2)](https://man7.org/linux/man-pages/man2/fsync.2.html).

For a new immutable object, recommend creating a uniquely named temporary file in the destination filesystem, streaming bytes while calculating length/checksum, flushing the file, publishing without overwriting an existing object, then synchronizing the containing directory. Only afterward may a durable database transaction reference it. If newly created directories are involved, persist their entries too. A crash before the database commit produces an orphan that can be recovered; the reverse ordering can produce a committed reference to absent bytes.

On macOS, distinguish fsync from the stronger F_FULLFSYNC request; Apple's archived manual explains why a drive's volatile cache matters. Implement and test the publication sequence separately on APFS, including directory synchronization and errors, rather than assuming Linux behavior proves the Mac implementation. [Apple fsync manual](https://developer.apple.com/library/archive/documentation/System/Conceptual/ManPages_iPhoneOS/man2/fsync.2.html).

APFS's metadata copy-on-write and crash protection do not prove that an application's multi-file transaction is durable or that message contents have end-to-end integrity. That is the engineering implication of Apple's filesystem description, not an additional guarantee from APFS. Preserve application checksums and write ordering. [APFS guide](https://developer.apple.com/library/archive/documentation/FileManagement/Conceptual/APFS_Guide/FAQ/FAQ.html).

**macOS 27 changes verified on 2026-10-11.** Its release notes fix a beta false-positive metadata-corruption report on high-RAM Macs; tighten access to other developers' app containers; describe additional XProtect restrictions on other apps' data; and deprecate encrypted HFS+ backups in favor of encrypted APFS. These affect diagnosis, import permissions and backup planning. They do not document a replacement for application persistence barriers. [macOS 27 release notes](https://developer.apple.com/documentation/macos-release-notes/macos-27-release-notes).

**Backup and migration.** Use SQLite's backup API for a consistent database copy, not an uncontrolled live-file copy. Before a schema migration, publish a verified backup generation, pause sync/send workers and keep the previous schema available for rollback. Transactional migration protects database changes; filesystem changes need their own crash-safe ordering. Never delete the previous generation simply because migration returned success. [SQLite backup API](https://sqlite.org/backup.html), [corruption hazards](https://sqlite.org/howtocorrupt.html).

A complete backup needs the consistent database and every immutable object it references. Pin those objects against garbage collection while producing its manifest. Test restore before calling a backup usable. Restore into a disconnected workspace first: historical pending sends and deletes must not run automatically against today's server.

Back up raw mail, drafts, outbox, settings, operation history and recovery data. Exclude only demonstrably rebuildable caches from Time Machine, and verify actual exclusions and restores on supported Macs. Do not assume a generic backup-exclusion resource flag establishes all Time Machine behavior. [Time Machine exclusions](https://support.apple.com/en-au/guide/mac-help/-mh15622/mac).

Keep the live store outside iCloud Drive, Dropbox, OneDrive, network mounts and similar replicated folders. SQLite's locking and WAL assumptions do not coordinate independent file replication; Mozilla also identifies cloud synchronization as a source of compaction interference. Synchronize an immutable backup package instead. [SQLite corruption guidance](https://sqlite.org/howtocorrupt.html), [Mozilla complications](https://support.mozilla.org/en-US/kb/compacting-folders-potential-complications).

**Detection and recovery.** Store original-byte lengths and SHA-256 checksums; scrub incrementally and verify before export or destructive moves. Run database integrity_check and foreign_key_check as distinct checks. Checksums detect discrepancies, not reconstruct missing bytes. Rebuild search and previews from verified originals; treat authoritative metadata corruption as a recovery event, preserving damaged files and restoring a verified copy. Raw messages alone cannot reconstruct settings, Bcc envelopes or whether a send was accepted. [SQLite integrity checks](https://sqlite.org/pragma.html).

The guarantee must specify its fault model. SQLite itself documents reliance on filesystem and device behavior, including storage that may misreport flush completion. No app can preserve the only copy after arbitrary hardware destruction. Independent backups and explicit recovery points are part of safety, not substitutes for durable commits. [SQLite atomic commit assumptions](https://sqlite.org/atomiccommit.html).

### 2.4 A storage format the user owns

**Recommendation:** store complete original messages as immutable RFC 5322/MIME .eml files, with portable metadata manifests and a documented directory layout. Assign a local UUID independently of Message-ID. Keep flags, memberships, provider IDs and revision history in metadata; preserve original bytes even when parsing fails. RFC 5322 describes message syntax, not an email client's complete account archive. [Internet Message Format](https://datatracker.ietf.org/doc/html/rfc5322).

Maildir provides a useful exchange format: tmp/new/cur directories, one message per file, with flags in filenames. However, provider labels, account configuration and outgoing envelopes require additional metadata. Dovecot documents its Maildir conventions and extensions. [Dovecot Maildir](https://doc.dovecot.org/2.3/admin_manual/mailbox_formats/maildir/).

Do not call every per-message store “Maildir-compatible.” As checked **2026-10-11**, Thunderbird explicitly describes its implementation as not full Maildir, experimental and disabled by default. File-per-message storage reduces certain risks; implementation correctness still decides safety. [Thunderbird Maildir](https://support.mozilla.org/en-US/kb/maildir-thunderbird).

| Format | Sweetbriar's recommended role | Safety and fidelity considerations |
|---|---|---|
| EML | Original message objects; simple import/export | Preserve bytes and include a metadata manifest; an EML alone does not describe all account state. |
| Maildir | Portable bulk import/export | Preserve timestamps, flags and filename conventions; export labels and identities separately. |
| mbox | Compatibility import/export, not the live mutable store | Handle delimiter and “From ” escaping variants explicitly; preserve input on parsing errors. [RFC 4155](https://www.rfc-editor.org/rfc/rfc4155). |
| Apple Mail .emlx / .mbox | Import through user-selected copies or Mail export | Apple exports .mbox packages. EMLX contains a byte-count framing and may carry plist metadata; use a tested importer, not suffix renaming. [Apple import/export](https://support.apple.com/en-lamr/guide/mail/mlhlp1030/mac), [EMLX implementation documentation](https://pkg.go.dev/github.com/wesm/msgvault@v0.14.1/internal/emlx). |
| Thunderbird stores | Read-only importer for mbox and its per-message layout | Treat .msf as an index, preserve source files, reconstruct and report inventories. [Mozilla storage notes](https://support.mozilla.org/en-US/kb/maildir-thunderbird). |
| PST / OST | Isolated, read-only legacy import | Use the published format and qualify parser coverage. libpff describes itself as alpha as checked **2026-10-11**; do not promise complete recovery. [MS-PST specification](https://learn.microsoft.com/en-us/openspecs/office_file_formats/ms-pst/141923d5-15ab-4ef1-a524-6dce75aae546), [libpff](https://github.com/libyal/libpff/). |

The EMLX description above is evidence from an implementation, not an Apple commitment to a stable public interchange format. Missing externally stored attachment bytes must be reported as incomplete, never silently converted into a supposedly complete message.

Every import/export should produce counts, byte totals, checksums, rejected-item paths and explicit metadata-loss warnings. Verify round trips against fixtures with duplicate Message-IDs, malformed MIME, unusual flags, large attachments and nested mailboxes. Never remove the source automatically after import.

### 2.5 Drafts, undo send and a durable outbox

**Drafts.** Recommend monotonic revision IDs, durable incremental saves, periodic complete snapshots and retained conflict versions. A practical initial coalescing window is 100–250 ms, with explicit Save, Close and Send waiting for the latest revision's commit. These are proposed product parameters to measure.

Do not claim that this saves every keystroke under sudden power loss: uncommitted input remains vulnerable. “Saved” must identify the committed revision; a pending indicator must remain until durability succeeds. A stricter per-edit durability mode is possible at additional latency and energy cost. Disk-full or flush failure must preserve the last committed revision and prominently report that new edits are not yet saved.

Copy attachment bytes into owned immutable storage before confirming attachment completion. Preserve names, media types and hashes independently of the original file path. Stream large files with bounded memory; do not retain a reference that breaks when the user deletes or edits the source file.

When Send is pressed, freeze a specific draft revision, complete MIME bytes, stable Message-ID, envelope sender and recipient set—including Bcc—in durable outbox state before dispatch. Later editing must create a new draft, not mutate the bytes of an in-flight attempt.

**Undo send.** Recommend a configurable 10–30-second local hold, implemented as a durable not-before deadline. Cancellation and worker claim must serialize through the same state transition. The UI must distinguish an available local cancellation from a submission already underway. On restart or uncertain clock changes, extend the remaining hold conservatively rather than accidentally accelerating it.

**SMTP's unavoidable ambiguity.** After the final DATA terminator, the submission server may accept a message while its reply is lost. Retrying can duplicate it; not retrying can leave it unsent. RFC 5321 recognizes the duplication hazard around the final response. A stable Message-ID assists investigation but does not require arbitrary SMTP servers to deduplicate. A final 250 means the receiving SMTP service accepted responsibility, not that all final recipients received the message. [RFC 5321](https://datatracker.ietf.org/doc/html/rfc5321).

Recommend these durable states:

~~~text
held → ready → attempt started → accepted
                          ↘ known rejection
                          ↘ outcome unknown
accepted → Sent copy pending → Sent copy confirmed
~~~

Record a conservative submission phase before transmitting the final DATA terminator or equivalent final chunk. Record acceptance and recipient outcomes durably before reporting success. A crash after acceptance but before recording it must recover as unknown. Never silently map unknown to failed and automatically retry.

Retry definite transient failures with backoff and jitter; show permanent rejection. Track individual RCPT results: an accepted subset must not cause a later retry to resend indiscriminately to that subset. For unknown outcomes, show “This message may have been sent; automatic retry is paused.” Reconcile provider submission IDs, Sent copies and receipts where available; absence from Sent alone does not prove non-delivery. Offer an informed user retry when proof remains unavailable.

**Sent-copy failure is not send failure.** Retrying an IMAP APPEND after a lost reply can duplicate the Sent copy. Use APPENDUID when available and reconcile independently; never resend the message merely because filing it failed. Gmail documents that SMTP submissions are automatically copied to Sent. Microsoft Graph sendMail normally saves to Sent and returns 202 before all subsequent processing is complete. These require provider-specific filing behavior and honest status labels. [Gmail client guidance](https://support.google.com/mail/answer/78892), [Graph sendMail](https://learn.microsoft.com/en-us/graph/api/user-sendmail?view=graph-rest-1.0), [Graph submission processing](https://learn.microsoft.com/en-us/graph/outlook-things-to-know-about-send-mail).

Backup restoration is another duplicate-send risk. Restore outbox entries in a paused recovery state and reconcile previously attempted submissions before enabling dispatch.

### 2.6 Credentials, encryption and hostile mail

**Credentials and OAuth tokens.** On macOS, recommend SecItem with the data-protection Keychain selected explicitly. Apple's TN3137 explains that SecItem otherwise defaults to the older file-based implementation on macOS; it recommends the data-protection implementation for new applications. Selecting it must not inadvertently enable iCloud synchronization. Keep passwords, refresh tokens and encryption keys out of SQLite, logs and portable metadata exports. [Keychain services](https://developer.apple.com/documentation/security/keychain-services), [TN3137](https://developer.apple.com/documentation/technotes/tn3137-on-mac-keychains).

On Linux, use a tested Secret Service adapter and distinguish locked/unavailable from missing. The specification explicitly treats lookup attributes as non-secret, so use opaque account identifiers there. **As checked 2026-10-11, the published 0.2 specification is a draft dated 2026-10-10**; this does not imply that Ubuntu's deployed services implement every new detail. Qualify actual GNOME/KDE environments. [Secret Service specification](https://specifications.freedesktop.org/secret-service/latest/), [lookup attributes](https://specifications.freedesktop.org/secret-service/latest/lookup-attributes.html), [unlocking](https://specifications.freedesktop.org/secret-service/latest/unlocking.html).

A current Rust integration caveat: **keyring 4.2.0**, checked **2026-10-11**, recommends keyring-core and explicit store crates for applications needing backend control; its v1 and cli modes are not that architecture. Prefer a narrow native adapter or evaluate that explicit-store approach, rather than copying an outdated feature recipe. [keyring documentation](https://docs.rs/keyring/latest/keyring/).

Recommend storing a new secret successfully before committing its database reference, retaining the previous credential during replacement where possible, and reporting failures without erasing account data. Secret stores and SQLite do not share one atomic transaction: test each ordering boundary. Server token revocation or an interrupted refresh-token rotation can still require reauthorization; that is an account-access failure, not permission to discard local mail.

Require authenticated TLS and certificate validation for remote access/submission; never silently downgrade after connection failure. For a loopback Bridge, define an explicit adapter-specific trust policy rather than weakening validation globally. [RFC 8314](https://www.rfc-editor.org/rfc/rfc8314).

**Encryption at rest.** FileVault protects volume data at rest and ties access to credentials on supported Macs. As an engineering consequence, it does not isolate an unlocked email session from software already able to act as the user, nor protect an unencrypted exported copy elsewhere. [Apple FileVault security](https://support.apple.com/guide/security/volume-encryption-with-filevault-sec4c6dc1b6e/web).

SQLCipher encrypts database pages and associated journal/WAL data with integrity protection. It does not automatically encrypt separately stored EML files or attachment exports. [SQLCipher design](https://www.zetetic.net/sqlcipher/design/).

Recommend FileVault and encrypted backups as the initial macOS baseline. An optional app-locked vault requires encryption of raw objects, attachments, drafts, previews and search indexes, plus key recovery and deliberate plaintext export. SQLCipher can cover its database component; it cannot supply the whole feature. Avoid designing a new cryptographic format casually. The strongest objection to postponing app-level encryption is the weaker confidentiality boundary while the operating-system session is unlocked.

**HTML rendering.** WKWebView is useful, but its defaults are not a mail security policy. allowsContentJavaScript defaults to true; content rule lists must be installed on the web view to take effect. A nonpersistent website data store avoids persistent web state, and custom scheme handlers can serve controlled local resources. [JavaScript preference](https://developer.apple.com/documentation/webkit/wkwebpagepreferences/allowscontentjavascript), [content rule lists](https://developer.apple.com/documentation/webkit/wkcontentruleliststore), [nonpersistent store](https://developer.apple.com/documentation/webkit/wkwebsitedatastore/nonpersistent()), [scheme handler](https://developer.apple.com/documentation/webkit/wkurlschemehandler).

Recommended defense layers:

1. Sanitize parsed HTML before rendering; remove scripts, forms, frames, embedded objects, event handlers and dangerous URL schemes.
2. Set allowsContentJavaScript=false before loading. Expose no privileged script bridge to message content.
3. Install a tested deny-by-default network content policy before loading. If compilation fails, show plain text.
4. Allow only capability-scoped local CID/attachment resources for that message; never give message HTML general file:// access.
5. Strip external CSS, CSS imports, fonts and URL-bearing CSS. Start with a small formatting allowlist rather than trying to enumerate every exfiltration pattern.
6. Use isolated, nonpersistent web state; intercept all navigation, new windows and downloads. Open approved links through the application, never execute mail-provided navigation automatically.

**ammonia 4.2.3**, checked **2026-10-11**, is a candidate Rust HTML sanitizer using an allowlist. It remains only one layer: Sweetbriar must define and test its own CSS, URL and network policy. [ammonia](https://docs.rs/ammonia/latest/ammonia/).

PortSwigger's research demonstrates exfiltration using CSS selectors and network requests without JavaScript. It is browser research, not proof of a specific current mail-client vulnerability. The implication is that JavaScript-off is insufficient whenever hostile CSS can initiate requests. [Blind CSS exfiltration](https://portswigger.net/research/blind-css-exfiltration).

EFAIL, published in 2018, showed decrypted email plaintext leaking through HTML resource requests, including attacks exploiting MIME composition and encryption malleability. Recommend authenticating encrypted-message integrity before displaying plaintext, refusing unsafe compositions and preventing decrypted content from initiating remote requests. Merely disabling scripts does not address that mechanism. [EFAIL authors' findings](https://efail.de/).

Block remote images and other content by default. If an image proxy is offered, document the inference clearly: hiding the user's network address does not remove a tracking identifier already embedded in a unique URL; fetching it can still signal that the message was opened. Never prefetch on hover or preview. Loading remote content must be a deliberate user choice. Google also acknowledges that image-containing mail can still reveal an opening to a sender. [Gmail image privacy limits](https://support.google.com/mail/answer/145919?co=GENIE.Platform%3DAndroid&hl=en).

**Attachments and links.** Set or preserve quarantine metadata, including com.apple.quarantine, on exported attachments, and verify the resulting Gatekeeper behavior on macOS 27 and 26. Apple's APIs expose quarantine properties and an app-wide default-quarantine setting. Do not clear quarantine to make opening attachments easier. Quarantine is a system handoff mechanism, not proof that an attachment is safe. [Quarantine properties](https://developer.apple.com/documentation/foundation/urlresourcevalues/quarantineproperties), [Launch Services quarantine keys](https://developer.apple.com/library/archive/documentation/General/Reference/InfoPlistKeyReference/Articles/LaunchServicesKeys.html).

Recommend filename/path validation, no automatic execution or archive extraction, bounded previews in isolated workers and explicit external opening. Display the actual destination domain for links, including a readable Unicode/punycode comparison where useful; warn on deceptive visible-text/target and From/Reply-To mismatches without claiming perfect phishing detection.

**Authentication signals.** DKIM authenticates a signing domain, not a benevolent sender. **RFC 9989, published May 2026**, now specifies DMARC and obsoletes RFCs 7489 and 9091. Authentication-Results is trustworthy only within a defined receiving-system trust boundary; arbitrary message headers can be forged. BIMI's own organization says it is not a security solution. [DKIM](https://www.rfc-editor.org/rfc/rfc6376), [current DMARC](https://www.rfc-editor.org/rfc/rfc9989), [Authentication-Results](https://www.rfc-editor.org/rfc/rfc8601), [BIMI explanation](https://bimigroup.org/all-about-bimi/).

Present these as qualified evidence, never a “safe email” badge. A downloaded message alone generally lacks the original SMTP context needed to independently reproduce SPF. Prefer trusted provider results; otherwise show unknown. Do not auto-delete solely because authentication fails.

**Containment and Rust.** Recommend App Sandbox, minimal entitlements, hardened runtime and isolated parsing/preview workers with no credentials and only narrowly scoped file access. Sandbox restrictions and hardened-runtime protections address different attack surfaces. Apple's macOS 27 security release includes APFS and WebKit fixes; a native WebKit renderer still requires operating-system security updates. [App Sandbox](https://developer.apple.com/documentation/security/app-sandbox), [hardened runtime](https://developer.apple.com/documentation/security/hardened-runtime), [macOS 27 security content](https://support.apple.com/en-us/149035).

Safe Rust reduces memory-corruption risk, but unsafe code and FFI remain important boundaries; memory safety does not establish synchronization, MIME semantics or crash correctness. **mail-parser 0.11.9**, checked **2026-10-11**, is a concrete parser candidate whose documentation describes malformed-input handling and fuzzing. Evaluate it with Sweetbriar's corpus and bounded depth, part counts, decoding expansion, header size, CPU and memory budgets. Preserve uninterpretable raw mail while declining an unsafe preview. [Rust unsafe boundaries](https://doc.rust-lang.org/nomicon/what-unsafe-does.html), [mail-parser](https://docs.rs/mail-parser/latest/mail_parser/).

### 2.7 Testing correctness

**Deterministic simulation.** FoundationDB's published design runs real database logic in a deterministic simulation with modeled networking, disks and faults. TigerBeetle's VOPR similarly makes failures reproducible through seeds; its liveness testing distinguishes surviving faults from making progress once conditions improve. These are useful patterns, not proof that their existing simulators can be dropped into an email client. [FoundationDB paper](https://www.foundationdb.org/files/fdb-paper.pdf), [TigerBeetle VOPR](https://github.com/tigerbeetle/tigerbeetle/blob/main/docs/internals/vopr.md), [simulation for liveness](https://tigerbeetle.com/blog/2023-07-06-simulation-testing-for-liveness/).

Build the Rust sync reducer around injected clocks, randomness, transport and storage. Model multiple clients and a server independently. Run the actual operation planner and recovery logic against controlled completion, failure and persistence schedules; keep protocol serialization and native-storage integration tests as additional layers.

Antithesis can add controlled system faults around containerized services. Its documented environment is Linux-based; it does not replace physical macOS/APFS and WebKit tests. Evaluate it as an optional service rather than a prerequisite or a promise about unverified pricing. [Antithesis environment](https://antithesis.com/docs/configuration/the_antithesis_environment/), [fault types](https://antithesis.com/docs/product/writing_tests/controlling_faults/fault_types/).

**Model-based and property tests.** proptest-state-machine supports comparison of generated transitions against a reference model, with shrinking. Use it for accounts, UID epochs, messages, memberships, operation receipts, draft revisions and outbox attempts. Keep the reference implementation intentionally simple and independent of production algorithms. Add loom for bounded Rust concurrency interleavings; property-generated sequential operations alone do not test every race. [proptest state machines](https://proptest-rs.github.io/proptest/proptest/state-machine.html), [loom](https://docs.rs/loom/latest/loom/).

Generators should include:

- UID reuse after UIDVALIDITY change; folder deletion/recreation; Message-ID collisions; identical bytes in distinct logical messages.
- Another client's move, flag update or expunge during an operation; duplicate/out-of-order notifications; partial scans and invalid cursors.
- APPEND/COPY completing just before the connection drops; partial MOVE completion; quota and malformed-message rejection.
- Partial recipient acceptance; a lost final SMTP reply; worker restart; cancel racing with dispatch; backup restoration of an already attempted send.
- Disk-full, read-only storage, short writes, EIO and failed flush/rename; crashes between object publication, database commit and receipt recording.

Assert safety after every transition. Then stop injecting faults and assert eventual convergence or a visible unresolved state. “Never deletes anything” can pass safety while being an unusable sync engine; liveness needs its own tests.

**Real-server matrix.** Recommend automated fixtures for Dovecot, Cyrus and Stalwart, plus controlled test accounts on Gmail, Microsoft 365, iCloud and Fastmail. Record server build, configured capabilities and account settings with each run. Exercise native APIs as well as IMAP where supported; do not infer provider behavior from Dovecot alone.

Dovecot's ImapTest is useful for exercising servers and constructing scenarios, but it is a server tester, not a substitute for running Sweetbriar's client against those scenarios. [Dovecot testing tools](https://doc.dovecot.org/main/core/admin/testing.html).

**Native storage and hostile-input testing.** SQLite's own test strategy includes simulated crashes, I/O errors and disk-full behavior. Use a fault-injecting VFS or equivalent test adapter against Sweetbriar's exact SQLite build, alongside real filesystem tests. kill -9 leaves the operating system running; it does not necessarily discard the kernel's dirty pages. Test power-loss semantics with a model that discards unpersisted writes and a sacrificial hardware/VM program appropriate to each storage stack. [SQLite testing](https://sqlite.org/testing.html).

Use cargo-fuzz/libFuzzer for MIME, HTML sanitization, IMAP/JMAP response decoding, EMLX/mbox/PST import and decompression boundaries; use suitable sanitizers for native/FFI parsers. Preserve every minimized failing input and seed. [Rust fuzzing guide](https://rust-fuzz.github.io/book/cargo-fuzz.html).

Recommended release gates are coverage-based: every durable boundary tested, every invariant checked, all known failures retained as regression cases, and restore tested on macOS 27, macOS 26 and the supported Linux filesystems. Add measurable seed budgets later; an arbitrary million passing traces cannot prove the absence of bugs.

## 3. Options compared

These comparisons are engineering judgments based on the mechanisms above.

| Storage architecture | Advantages | Strongest objection | Recommended use |
|---|---|---|---|
| Mutable mbox plus index | Familiar exchange format; fewer files | Compaction rewrites shared data and enlarges the scope of damage | Import/export only |
| Maildir as the entire live store | Per-message files; recognizable layout | Renaming flags is not a transaction with settings, drafts and operation state; provider metadata exceeds the format | Exchange format, or carefully specified subset |
| All raw bytes and metadata in SQLite BLOBs | One transactional persistence domain; simpler crash ordering | Large shared artifact, less convenient direct access, backup/write amplification | Strong alternative if file-store complexity proves excessive |
| Immutable EML/blob files plus SQLite metadata | Original-byte ownership; small damage scope; independent index rebuilding | Correctness spans filesystem publication, database references, backup and garbage collection | **Preferred**, with the ordering protocol in §2.3 |
| Server-authoritative cache only | Lower disk use | Cannot protect local-only drafts, unsent work or mail deleted upstream | Optional limited-download mode, explicitly labeled |

| Policy choice | Preferred policy | Cost / strongest counterargument |
|---|---|---|
| Critical commits | WAL + FULL; fullfsync on macOS | Flush latency and energy cost; keep work off the UI thread and acknowledge grouped commits honestly |
| Unsupported safe deletion | Leave pending or use a non-destructive operation | Some old servers cannot complete the requested workflow |
| Ambiguous send | Preserve and reconcile; no silent automatic retry | A real unsent message can remain delayed until the user acts |
| Remote disappearance | Retain recovery bytes; break on suspicious bulk loss | Disk usage and possible retention of mail the user intended to erase |
| Rendering | Restricted HTML with remote content blocked | Reduced fidelity; newsletters may require deliberate image loading |
| Initial encryption | OS volume encryption and encrypted backups | Weaker isolation in an unlocked session than a properly implemented app vault |

## 4. Concrete recommendations for Sweetbriar

### 4.1 Architecture and recovery boundaries

Put safety policy in the shared Rust core. Swift and later Linux UIs submit commands and observe revisioned snapshots; they cannot bypass the operation planner or write storage directly. Native adapters own Keychain/Secret Service, file durability and rendering containment.

Recommended layout:

~~~text
store/
  state.sqlite                 authoritative mutable state
  objects/<account>/<shard>/   immutable original EML and attachment blobs
  revisions/                   draft/outbox snapshots and envelope manifests
  backups/<generation>/        consistent DB + referenced objects + manifest
  recovery/                    preserved originals and disputed versions
cache/
  search.sqlite                rebuildable full-text index
  previews/                    rebuildable decoded/rendered content
~~~

Use a small schema for accounts, logical messages, memberships, remote identities/epochs, intent records, receipts, draft revisions, outbox attempts, recipient results, settings and backup generations. Give every destructive effect a durable originating intent. Settings migrations must preserve unknown fields or retain their original representation, rather than silently replacing invalid input with defaults.

**rusqlite 0.40.2**, checked **2026-10-11**, is a concrete SQLite binding candidate with a backup API module. Its wrapper release does not certify the bundled/system SQLite build. SQLite FTS5 is a reasonable first rebuildable search projection; deciding “best-in-class search” needs separate relevance and performance research. [rusqlite](https://docs.rs/rusqlite/latest/rusqlite/), [FTS5](https://sqlite.org/fts5.html).

Seal all referenced object files before committing metadata. Acquire exclusive process ownership of the live store; a second app instance must use the existing core or remain read-only. Serialize workers, deduplicate notifications and enforce one active submission attempt per outbox item. Keep progress, failures and uncertain results discoverable without requiring users to understand protocol transcripts.

Garbage collection must respect live references, pending operations, recovery retention and backup pins; commit reference removal before unlinking, and never erase disputed data to clear an error.

Maintain portable draft/outbox snapshots for recovery, but distinguish orphaned preparation files from committed actions: finding an outbox file must never automatically send it. Periodic metadata exports and backups aid recovery; they are not equivalent to a second durably committed copy of every mutation.

**The strongest architectural counterargument:** SQLite BLOB storage removes a substantial amount of custom multi-file crash logic. Sweetbriar should choose the immutable-file design only after its publication, backup and garbage-collection protocol passes fault injection. If that proof is too costly, begin with BLOBs and verified EML export; user ownership is better served by a reliable export than by an unreliable nominally open store.

### 4.2 Invariants, enforcement and tests

These are proposed acceptance conditions under the documented storage/server fault model, not claims of implementation already completed.

| Invariant | Enforcement | Required test |
|---|---|---|
| An acknowledged local revision survives process crash and modeled power loss | FULL commits, platform barriers, no premature “Saved” acknowledgement | Crash before/after commit; discard volatile writes; verify every acknowledged revision |
| A committed object reference identifies complete, unchanged bytes | Publish and flush immutable objects before referencing; length/checksum checks | Short write, flush failure, rename failure, torn object and orphan recovery |
| Index rebuilding cannot alter originals or authoritative user state | Separate cache ownership and read-only input | Delete/corrupt every cache; rebuild; compare original and metadata inventories |
| A remote destructive action has a specific durable intent | Planner-only writes, provenance, UID scoping, capability policy | Rogue response, other-client \Deleted, same-folder move, account removal |
| Old mailbox identities cannot target new messages | Epoch-bound mappings and suspended stale intents | UIDVALIDITY change with numeric UID reuse during an offline delete |
| Incomplete observations cannot establish absence | Staged inventory and atomic cursor publication | Fail each pagination step; expired cursor; limited Gmail visibility |
| A move cannot deliberately delete the sole complete copy | Destination verification and retained recovery source | Drop connection after COPY/APPEND; destination quota failure; incomplete local source |
| Draft conflicts preserve both authors' work | Immutable revisions and explicit conflict branches | Two clients edit offline, reconnect in both orders, crash during merge |
| Send uncertainty is preserved and visible | Durable phases/receipts; unknown state blocks blind replay | Lose final acceptance reply; crash before receipt commit; restore an old backup |
| Filing a Sent copy cannot resubmit the email | Independent submission and filing state machines | APPEND failure after known acceptance; repeat filing with a lost receipt |
| Undo send cannot race into an unreported dispatch | Serialized cancellation/claim; explicit cutoff | Concurrent cancel/claim, restart during hold, wall-clock jump |
| Credential errors cannot erase mail or leak secrets | Native secret adapters, staged replacement, redacted logs | Locked/missing service; token rotation failure; inspect logs and exports |
| Untrusted content cannot obtain unrelated files, secrets or automatic network access | Restricted renderer, resource capabilities, sandboxed workers | CSS/EFAIL/MIME fixtures; canary HTTP/DNS endpoints; CID/path traversal |
| A restore or migration cannot silently lose data or replay historical effects | Verified generations, inventory comparison, disconnected recovery mode | Fail every migration boundary; restore old pending sends/deletes; compare settings and attachment bytes |

Checksums establish byte consistency, not that a malicious sender's bytes are benign. A detector finding corruption must pause dangerous work and preserve evidence; it must not “repair” by deleting discrepancies.

### 4.3 Build order and release gate

Recommend four milestones: local storage/draft/outbox durability; read-only sync and complete export/restore; reversible mutations with reconciliation; then narrowly authorized destructive operations and submission. Each milestone adds failure traces to the permanent regression corpus.

Before release, require: verified backups restored on clean machines; loss-of-network and disk-full exercises with real accounts; complete-versus-header-only status visible; no automatic dispatch after backup restore; no secret leakage in diagnostics; and no unaccounted destructive command in recorded protocol traces.

Measure durability acknowledgement latency, attachment import throughput, checkpoint stalls and recovery time on macOS 27 and 26. Optimize scheduling, batching and cache policy while retaining the stated acknowledgement contract.

## 5. Risks and open questions

1. **The word “never” needs a precise boundary.** A full download can be protected locally; mail never fetched cannot be recovered from its nonexistent local copy. Offer a complete-archive mode and clearly identify incomplete messages. Arbitrary media destruction requires independent copies; backups have a recovery point unless every acknowledgement also waits for replication.
2. **Retaining deletions conflicts with erasure expectations.** Decide recovery retention, explicit permanent purge, encrypted-backup retention and storage-full behavior. Suspicious disappearance must not silently trigger garbage collection.
3. **Provider evidence may be incomplete.** IMAP does not supply universal operation IDs; server settings may be inaccessible; Sent lookup may lag. Define which outcomes require human resolution for each adapter.
4. **Single-writer design has scaling costs.** Large imports, scrubbing and checkpoints must not block draft commits. Prove scheduling under load before adding multiple mutation owners.
5. **Models can faithfully reproduce the wrong assumptions.** Keep the reference model independent, audit it against RFCs, and maintain real-provider tests. Simulation does not establish actual APFS/firmware persistence.
6. **Credential and encryption recovery need product decisions.** Reauthorization should preserve mail. Optional vault keys need a recoverable backup story; losing the only decryption key defeats data availability even when every file survives.
7. **Native import permissions changed in macOS 27.** Validate user-selected import/export workflows against the new app-data restrictions; do not base import on unrestricted scanning of another application's live container.
8. **Parser and renderer limits can affect legitimate mail.** Preserve raw bytes and explain unavailable previews. App-level OpenPGP/S/MIME support should ship only with maintained cryptography, explicit integrity policy and EFAIL regression tests.
9. **Incident evidence has limits.** The Apple defect's underlying mechanism remains (unverified); Microsoft repair guidance does not identify a universal cause; the other named clients lack verified root-cause cases here. None should be used as a claim that current competitors inevitably lose mail.
10. **Corrupt metadata cannot be fully reconstructed from EML.** Raw objects permit message recovery, but settings, envelopes and submission history need their own backups. State the recovery point rather than promising automatic reconstruction of everything.

## 6. Sources actually consulted

All sources below were consulted on **2026-10-11**. Archived documents and historical incident reports are used for mechanisms or dated events; current versions and platform changes are dated in the report. The Linux filesystem manuals were also consulted through search-cached excerpts when direct retrieval failed.

1. [Apple security releases](https://support.apple.com/en-us/100100)
2. [macOS 27 compatibility](https://support.apple.com/en-ca/127255)
3. [RFC 5321](https://datatracker.ietf.org/doc/html/rfc5321)
4. [UIDPLUS](https://www.rfc-editor.org/rfc/rfc4315)
5. [IMAP4rev2](https://www.rfc-editor.org/rfc/rfc9051)
6. [SQLite WAL](https://sqlite.org/wal.html)
7. [CSS exfiltration research](https://portswigger.net/research/blind-css-exfiltration)
8. [EFAIL](https://efail.de/)
9. [tracker 1890230](https://bugzilla.mozilla.org/show_bug.cgi?id=1890230)
10. [bug 1923747](https://bugzilla.mozilla.org/show_bug.cgi?id=1923747)
11. [Mozilla's explanation](https://support.mozilla.org/en-US/kb/compacting-folders-potential-complications)
12. [Michael Tsai's incident log](https://mjtsai.com/blog/2019/10/11/mail-data-loss-in-macos-10-15/)
13. [Proton documentation](https://proton.me/support/what-is-the-recovered-messages-folder-in-bridge)
14. [Microsoft repair guidance](https://support.microsoft.com/en-us/outlook/repair-outlook-data-files-pst-and-ost)
15. [ImapSettings API](https://developers.google.com/workspace/gmail/api/reference/rest/v1/ImapSettings)
16. [Apple Mail troubleshooting](https://support.apple.com/en-gb/117379)
17. [Disconnected IMAP guidance](https://www.rfc-editor.org/rfc/rfc4549)
18. [RFC 7162](https://www.rfc-editor.org/rfc/rfc7162)
19. [Internet Message Format](https://datatracker.ietf.org/doc/html/rfc5322)
20. [RFC 6851](https://www.rfc-editor.org/rfc/rfc6851)
21. [Gmail synchronization](https://developers.google.com/workspace/gmail/api/guides/sync)
22. [Gmail extensions](https://developers.google.com/workspace/gmail/imap/imap-extensions)
23. [Graph immutable IDs](https://learn.microsoft.com/en-us/graph/outlook-immutable-id)
24. [JMAP Core](https://www.rfc-editor.org/rfc/rfc8620)
25. [JMAP Mail](https://datatracker.ietf.org/doc/html/rfc8621)
26. [SQLite pragmas](https://sqlite.org/pragma.html)
27. [WAL-reset advisory](https://sqlite.org/wal.html#walresetbug)
28. [SQLite releases](https://sqlite.org/news.html)
29. [rename(2)](https://man7.org/linux/man-pages/man2/rename.2.html)
30. [fsync(2)](https://man7.org/linux/man-pages/man2/fsync.2.html)
31. [Apple fsync manual](https://developer.apple.com/library/archive/documentation/System/Conceptual/ManPages_iPhoneOS/man2/fsync.2.html)
32. [APFS guide](https://developer.apple.com/library/archive/documentation/FileManagement/Conceptual/APFS_Guide/FAQ/FAQ.html)
33. [macOS 27 release notes](https://developer.apple.com/documentation/macos-release-notes/macos-27-release-notes)
34. [SQLite backup API](https://sqlite.org/backup.html)
35. [corruption hazards](https://sqlite.org/howtocorrupt.html)
36. [Time Machine exclusions](https://support.apple.com/en-au/guide/mac-help/-mh15622/mac)
37. [SQLite atomic commit assumptions](https://sqlite.org/atomiccommit.html)
38. [Dovecot Maildir](https://doc.dovecot.org/2.3/admin_manual/mailbox_formats/maildir/)
39. [Thunderbird Maildir](https://support.mozilla.org/en-US/kb/maildir-thunderbird)
40. [RFC 4155](https://www.rfc-editor.org/rfc/rfc4155)
41. [Apple import/export](https://support.apple.com/en-lamr/guide/mail/mlhlp1030/mac)
42. [EMLX implementation documentation](https://pkg.go.dev/github.com/wesm/msgvault@v0.14.1/internal/emlx)
43. [MS-PST specification](https://learn.microsoft.com/en-us/openspecs/office_file_formats/ms-pst/141923d5-15ab-4ef1-a524-6dce75aae546)
44. [libpff](https://github.com/libyal/libpff/)
45. [Gmail client guidance](https://support.google.com/mail/answer/78892)
46. [Graph sendMail](https://learn.microsoft.com/en-us/graph/api/user-sendmail?view=graph-rest-1.0)
47. [Graph submission processing](https://learn.microsoft.com/en-us/graph/outlook-things-to-know-about-send-mail)
48. [Keychain services](https://developer.apple.com/documentation/security/keychain-services)
49. [TN3137](https://developer.apple.com/documentation/technotes/tn3137-on-mac-keychains)
50. [Secret Service specification](https://specifications.freedesktop.org/secret-service/latest/)
51. [lookup attributes](https://specifications.freedesktop.org/secret-service/latest/lookup-attributes.html)
52. [unlocking](https://specifications.freedesktop.org/secret-service/latest/unlocking.html)
53. [keyring documentation](https://docs.rs/keyring/latest/keyring/)
54. [RFC 8314](https://www.rfc-editor.org/rfc/rfc8314)
55. [Apple FileVault security](https://support.apple.com/guide/security/volume-encryption-with-filevault-sec4c6dc1b6e/web)
56. [SQLCipher design](https://www.zetetic.net/sqlcipher/design/)
57. [JavaScript preference](https://developer.apple.com/documentation/webkit/wkwebpagepreferences/allowscontentjavascript)
58. [content rule lists](https://developer.apple.com/documentation/webkit/wkcontentruleliststore)
59. [nonpersistent store](https://developer.apple.com/documentation/webkit/wkwebsitedatastore/nonpersistent())
60. [scheme handler](https://developer.apple.com/documentation/webkit/wkurlschemehandler)
61. [ammonia](https://docs.rs/ammonia/latest/ammonia/)
62. [Gmail image privacy limits](https://support.google.com/mail/answer/145919?co=GENIE.Platform%3DAndroid&hl=en)
63. [Quarantine properties](https://developer.apple.com/documentation/foundation/urlresourcevalues/quarantineproperties)
64. [Launch Services quarantine keys](https://developer.apple.com/library/archive/documentation/General/Reference/InfoPlistKeyReference/Articles/LaunchServicesKeys.html)
65. [DKIM](https://www.rfc-editor.org/rfc/rfc6376)
66. [current DMARC](https://www.rfc-editor.org/rfc/rfc9989)
67. [Authentication-Results](https://www.rfc-editor.org/rfc/rfc8601)
68. [BIMI explanation](https://bimigroup.org/all-about-bimi/)
69. [App Sandbox](https://developer.apple.com/documentation/security/app-sandbox)
70. [hardened runtime](https://developer.apple.com/documentation/security/hardened-runtime)
71. [macOS 27 security content](https://support.apple.com/en-us/149035)
72. [Rust unsafe boundaries](https://doc.rust-lang.org/nomicon/what-unsafe-does.html)
73. [mail-parser](https://docs.rs/mail-parser/latest/mail_parser/)
74. [FoundationDB paper](https://www.foundationdb.org/files/fdb-paper.pdf)
75. [TigerBeetle VOPR](https://github.com/tigerbeetle/tigerbeetle/blob/main/docs/internals/vopr.md)
76. [simulation for liveness](https://tigerbeetle.com/blog/2023-07-06-simulation-testing-for-liveness/)
77. [Antithesis environment](https://antithesis.com/docs/configuration/the_antithesis_environment/)
78. [fault types](https://antithesis.com/docs/product/writing_tests/controlling_faults/fault_types/)
79. [proptest state machines](https://proptest-rs.github.io/proptest/proptest/state-machine.html)
80. [loom](https://docs.rs/loom/latest/loom/)
81. [Dovecot testing tools](https://doc.dovecot.org/main/core/admin/testing.html)
82. [SQLite testing](https://sqlite.org/testing.html)
83. [Rust fuzzing guide](https://rust-fuzz.github.io/book/cargo-fuzz.html)
84. [rusqlite](https://docs.rs/rusqlite/latest/rusqlite/)
85. [FTS5](https://sqlite.org/fts5.html)
