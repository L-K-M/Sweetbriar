# 00 — Overview

## The pilot

The first version Sweetbriar builds toward is a **pilot for one user**,
the owner, on macOS. It must be good enough to replace the owner's daily
mail client for Proton Mail.

**In the pilot:**
- **macOS app** (macOS 26+, Apple silicon): an AppKit shell with SwiftUI
  islands.
- **Local archive:** import Apple Mail `.mbox` exports and EML files into
  immutable storage, with a reconciliation report.
- **Proton Mail through Proton Bridge:** IMAP and SMTP to Bridge on the
  same Mac; read, search, compose, send, flag and file.
- **Search:** exact local full-text search (FTS5) with field queries and
  visible coverage.
- **Reader:** sanitized sender layout for HTML and literal plain text.
  The Reading view arrives later, category by category, as a fidelity
  corpus proves it (research D16).
- **Composer:** plain text or Markdown, sent as `multipart/alternative`.
- **Classic workflows:** recoverable drafts and an inspectable Out Box;
  saved views; keyboard filing with undo; private notes and tags (D14).
- **Account colors:** the compose identity strip and account markers
  (D31).
- **Durability:**
  - originals are immutable;
  - SQLite is the only authority for mutable state;
  - backups are coordinated generations, and restores are quarantined.
- **Account health surface:** sync state, paused accounts, parked
  intents.

**Not in the pilot:**
- Linux UI: the Rust core builds and passes its tests on Linux from day
  one, but the GTK app comes later.
- Gmail, Microsoft Graph and JMAP backends, and OAuth.
- Rules beyond manual filing; server-rule editing.
- Semantic search and AI.
- A background agent: Sweetbriar syncs while it runs.
- Calendars and contacts sync. Address completion comes from local
  history, and optionally from macOS Contacts.
- Distribution to other users: signing for others, notarization and
  Sparkle updates come before any second user.

## Principles

These rule every decision the spec leaves open.

1. **Never lose what the user acquired or wrote.** Once "Saved" shows,
   it survives crashes and power loss. Archived mail is never deleted or
   overwritten except by an undoable action or a purge or retention
   policy the user chose. A restore never sends, moves or deletes
   anything by itself.
2. **Uncertainty stays visible.** An unknown send outcome, an ambiguous
   match or incomplete coverage is shown, never guessed away.
3. **The core owns meaning; shells own presentation.** Rust holds state,
   rules and durability. Swift and GTK hold windows, editing and
   accessibility.
4. **Strict layering.** Each layer talks only to the one directly below
   it, enforced by a check in CI.
5. **Measure, don't assume.** Performance budgets are tested on the
   owner's Mac. Engine and design choices have registered switch
   criteria.
6. **Small, testable steps.** Every change lands as a reviewed PR with
   acceptance checks.

## Glossary

| Term | Meaning |
|---|---|
| **Account** | A configured mail source in Sweetbriar (the local archive, a Proton Bridge account). It has a stable UUID that never depends on credentials or remote ids |
| **Identity** | An address the user can send as, belonging to an account (the primary address, aliases) |
| **Acquisition** | Immutable bytes of one message as acquired from one source, stored once per content hash, with provenance |
| **Occurrence** | One remote copy of a message: (account, mailbox, UIDVALIDITY, UID) for IMAP. It carries its own flags. The same message in two Bridge label folders is two occurrences |
| **Logical message** | Sweetbriar's grouping of occurrences known to be the same provider message. It never merges on Message-ID alone |
| **Membership** | An occurrence's presence in a mailbox (folder or label) |
| **Intent** | A durable record of a remote effect the user requested (flag, move, delete, send), written in the same transaction as the local effect |
| **Send attempt** | A frozen record of one submission: bytes, envelope, identity, per-recipient results |
| **Generation** | A version counter used to invalidate stale query windows, credentials or backups |
| **Coverage** | Which parts of an account are locally available (headers, bodies, attachments, index) |
| **Breaker** | The safety stop that pauses unexplained mass disappearance |
| **Bridge** | Proton Mail Bridge, Proton's local IMAP/SMTP gateway |
