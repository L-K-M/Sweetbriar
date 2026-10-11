# Mailsmith (Bare Bones Software → Stickshift Software, 1998–2018)

Synthesis of [raw/mailsmith/devin.md](../raw/mailsmith/devin.md) and
[raw/mailsmith/codex.md](../raw/mailsmith/codex.md). Both are built on the
*Mailsmith User Manual* (2nd ed., 2009, 217 pages; extracted from the 2.3.1
disk image on macintoshgarden.org; not redistributed here) plus the app's
help book, release notes, reviews and reminiscences.

## In one paragraph

Mailsmith was the BBEdit company's "extra-strength" plain-text email client
for the Mac. It was POP3/SMTP only and was built around three pillars, which
Matt Neuburg called the "Iron Triangle": BBEdit-class text editing, deep
AppleScript (scriptable, recordable and attachable), and **distributed
filtering**, where the mailbox tree *is* the filter flowchart. Reviewers
called the filtering "revolutionary" (Tsai, 1999) and said "no program gives
you more or better ways to create and use filters" (Porter, 2002). It won a
Macworld Eddy in 2003. It died of competing with the free Apple Mail and of
never shipping the IMAP support it had promised; the last build (2.4.3,
2018) is 32-bit and has been dead since Catalina. People still miss it in
2026: "The last email program I truly loved was Bare Bones' Mailsmith"
(Watts Martin, via Michael Tsai, August 2026).

## Timeline

| Year | Event |
|---|---|
| 1997 | Announced; code name "Bluto" |
| 1998-05-04 | 1.0, $79; POP3/SMTP; BBEdit text engine; NeoAccess object database |
| 2002 | 1.5: Carbon (OS 9 + OS X), PCRE grep engine, easier filters; 1.5.3 adds relevance "Find Messages About" |
| 2003 | 2.0: OS X only, faster, SpamSieve/PGP/Address Book integration; Macworld Eddy |
| 2004 | 2.1: keyboard command to move messages with type-ahead destination |
| 2009-08-18 | Ownership moves to Stickshift Software (Rich Siegel); 2.2 freeware, closed source. Storage switches to "outboard data files, one per message" plus a rebuildable database, for Spotlight, performance and disaster recovery |
| 2010 | 2.3: Rebuild/Reconstruct Mailboxes, Reply to List, Dock badge |
| 2018-05-14 | 2.4.3, last release; never made 64-bit |

## Feature inventory (highlights; full detail in the raw reports)

**Distributed filtering.**
- Filters are named objects that do nothing until attached to mailboxes.
- Arriving mail is offered to the top-level mailboxes' filters in
  alphabetical order. A **Deposit** drops the message into that mailbox and
  offers it to the mailbox's children.
- **Transfer** ends filtering; **Copy** duplicates. One filter can be
  attached to many mailboxes. Outgoing mail runs through the same system.
- Make a Filter from a model message; auto-filters when creating a mailbox;
  **Re-Apply Filters** for "cradle to grave" flows; filter logging; XML
  import and export.

**Criteria and actions.**
- Criteria cover any header, body, label, priority, status, Sender is
  Known, Sender's group membership, server account, List-ID, notes, spam
  score, size, partial, and on server.
- Match methods include **PCRE grep** and **Approximately Matches** (fuzzy,
  20–100%).
- About 21 actions, including Run Script (`on FilterMessage`), autoreply
  from file, Report to SpamCop, Print, Notify, Extract Enclosures and Set
  Subject.
- Criteria are linear only, with no nested groups.

**Search.** Advanced Query uses the same criteria engine as filters,
including regex. Spotlight importer (`kind:mailsmith`). The manual includes a
roughly 30-page regex tutorial. Weaknesses: no incremental results, and
queries can't be saved.

**Editor (BBEdit engine).**
- Six-slot clipboard history, Copy/Cut & Append, Paste Quoted, unlimited
  undo.
- Split panes, rectangular selection, Emacs bindings, invisibles.
- **Rewrap Quoted Text**, quote level up and down, Strip Quotes, Zap
  Gremlins, Change Case, Balance.
- As-you-type spelling (2.2).

**Composition.**
- Private, searchable per-message **Notes**, never sent.
- Configurable attribution string with ICU date formats.
- Netiquette warnings: excessive quoting, sigdash, "me too",
  unbracketed URLs.
- Random signatures. Send Now vs Queue. Redirect and Re-Send. Reply to List
  (`List-Post`). format=flowed.

**Clippings** with placeholders such as `#INSERTION#` (plus jump markers),
`#SELECT#`, `#CLIPBOARD#`, `#DATETIME fmt#`, `#SCRIPT name#`,
`#SYSTEM shell#` and `#UUID#`. Clipping sets switch on automatically per
account.

**Scripting.**
- **Menu Scripts** named `Menu•Item` intercept or wrap menu commands.
  Gruber used one to stop Command-E sending mail when text was selected.
- Startup and Shutdown script items.
- BBEdit plug-in API.
- "Run Unix command on each received message", a pipe filter.

**Misc.**
- POP Monitor: triage the server mailbox before downloading.
- 32 labels; per-mailbox notification mute; Quiet Time.
- Copy Item Link (`mailsmith:` URLs); digest bursting.
- Any menu key can be reassigned and any menu item hidden.
- HTML-only mail is shown as synthesized plain text.

## What users loved, and what they didn't

- **Loved:**
  - mail treated as text
  - script everything; a small attachable surface earned outsized loyalty
    (Gruber, Tsai, Simmons, Porter)
  - filters that matched how people actually organize
  - keyboard control (Porter barely used the mouse)
  - suspicion of HTML and tracking pixels long before it was mainstream
- **Disliked:**
  - no IMAP (the killer)
  - slow, bloated storage before 2.0 (Tsai measured mailboxes 3–7× larger
    than competitors' and 559 messages taking over 11 minutes); crashes in
    1.5
  - no HTML display
  - weak early address book
  - no saved or incremental searches
  - alphabetical filter order as a hidden control
  - send failures easy to miss in the `(errors)` mailbox
  - uneditable queued messages
- **Recovery lesson:** 2.3's Reconstruct rebuilt databases from message
  files but **lost notes, labels and read/replied state**. Metadata that
  lives in only one place gets lost.

## What survives today

- **MailMate** is the closest living descendant: plain text and Markdown,
  an external editor, rules on any mailbox, keyboard binding files.
- **Apple Mail** has rules with AppleScript actions, but no menu-veto hooks.
- **Thunderbird** has local filters and saved searches.
- **Outlook for Mac** documents server rules only.
- **Mimestream and Gmail** edit server filters.

Nobody ships mailbox-attached contextual rules, regex filters in the client,
a BBEdit-grade composer, or attachable menu hooks.

## What Sweetbriar should take

1. **Storage:** Mailsmith 2.2's design (one file per message plus a
   disposable index), with **metadata journaled separately** so a rebuild
   keeps notes and labels. See [04-data-safety](../04-data-safety.md).
2. **One criteria engine** shared by search, saved views, rules and
   scripting, with regex and fuzzy match modes over an index (not a scan),
   protected so a pathological regex can't freeze the UI.
3. **Mailbox-attached rules as an advanced mode** over the same engine,
   with explicit order instead of alphabetical, a preview and a trace.
4. **Text workbench in the composer:** rewrap quoted text, Paste Quoted,
   clipboard history with append, strip quotes, change case, split view,
   rectangular selection; Notes; clippings with typed placeholders
   (`#SYSTEM#`-style shell insertion behind an explicit permission).
5. **Attachable automation:** pre-send checks and menu pre/post hooks over
   the command layer (AppleScript and App Intents on macOS, CLI and D-Bus
   later on Linux). Skip AppleScript *recording* (expensive).
6. **Received-message overlays:** Set Subject becomes a local display
   overlay (Graph only allows subject edits on drafts).
7. **Skip:** POP Monitor as such (reframe it as a read-only server-state
   inspector), BinHex/AppleDouble encoding, menu hiding.
