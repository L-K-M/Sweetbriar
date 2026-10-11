<!-- provenance: helper=devin model=swe-2-max started=2026-10-11T18:42:53+00:00 exit=0 brief=research/briefs/ -->

# Mailsmith — Research Report for Sweetbriar

*Compiled 2026-10-11. Primary sources: `sources/mailsmith-manual.txt` (the *Mailsmith User Manual*, 2nd ed., © 2009 Stickshift Software, covering Mailsmith 2.2 — distributed with the 2.3.1 disk image on macintoshgarden.org; cited below as "Manual p. N" using the `=== Page N ===` markers) and `sources/help/` (the app's HTML help book, a condensed subset of the manual). Web sources are linked inline; every factual claim that is not drawn from the manual/help is linked. Quotes from reviews and blogs are verbatim unless marked as paraphrase; later recollections are labeled as such.*

## 1. TL;DR

- **Mailsmith was Bare Bones Software's "extra-strength" plain-text email client**: POP3/SMTP only, built around the BBEdit text engine, a NeoAccess object database, distributed filtering, and deep AppleScript scripting. It shipped 1998-05-04 at US$79 and was actively developed through 2005 ([launch press release](https://web.archive.org/web/20040319155749/http://www.barebones.com:80/company/press.php?news_id=63), [Bare Bones company history](https://www.barebones.com/company/history.html)).
- **Its three pillars** — called the "Iron Triangle" by reviewer Matt Neuburg — were BBEdit-class text handling, recordable/attachable AppleScript, and *distributed filtering*, where the mailbox hierarchy itself is the filter flowchart ([TidBITS, 2003-07-28](https://tidbits.com/2003/07/28/true-confessions-of-a-mailsmith-switcher/)).
- **It died of two causes**: commercially, competing with Apple's free bundled Mail ("competing with 'free'… really does make it difficult to justify investing," per founder Rich Siegel, [MacObserver interview](https://macobserver.com/tmo/article/digging_deep_into_bare_bones_with_rich_siegel)); technically, the missing IMAP support that drove users (including [John Gruber](https://daringfireball.net/2007/08/rethinking_email)) away — and finally the end of 32-bit app support in macOS Catalina, since the last build (2.4.3, 2018-05-14) was never recompiled for 64-bit ([mailsmith.org updates](http://www.mailsmith.org/support/mailsmith/updates.html)).
- **In 2009 ownership moved to Stickshift Software LLC** — a company Siegel founded in Rhode Island in June 2009 expressly to continue Mailsmith — and 2.2 shipped as freeware, staying "closed source" but a "labor of love" ([TidBITS, 2009-08-18](https://tidbits.com/2009/08/18/mailsmith-2-2-released-as-freeware-from-new-firm/), [Stickshift about page](http://www.mailsmith.org/company/)).
- **What still isn't replaced**: PCRE regex search/filter of messages, distributed/contextual filtering, per-message Notes, menu-item scripting/attachability, clippings with placeholders, multiple clipboards/undo/split panes/rewrap-quoted-text — MailMate is the closest modern analog but lacks the scripting, editor depth, and filter actions. In August 2026, [Michael Tsai's blog](https://mjtsai.com/blog/2026/08/21/the-best-worst-e-mail-client/) still quotes Watts Martin: "The last email program I truly loved was Bare Bones' Mailsmith," and commenter Riccardo Mori writes he'd "instantly leave Mail for Mailsmith, if it were still actively developed."
- **For Sweetbriar**: the storage design Mailsmith converged on in 2.2 — one data file per message plus a rebuildable index database — is exactly right for "never lose mail"; the filter engine is worth reviving nearly whole; the AppleScript layer should be a Swift/automation-layer feature, not a Rust-core one; and unlike 1998, *server-side* sync makes some Mailsmith features better (permanent message metadata can sync via IMAP keywords/JMAP) while others (client-side filtering) must be a local overlay on top of server rules (Sieve/Gmail filters/Exchange rules).

## 2. Findings

### 2.1 History: who made it, when, what happened

**Origins.** Bare Bones Software — maker of BBEdit — announced on 1997-07-31 that its new email client, developed under the code name **"Bluto"**, would ship as "Mailsmith" ([company history](https://www.barebones.com/company/history.html)). Mailsmith 1.0 shipped 1998-05-04 at US$79 list (US$59 cross-upgrade for registered BBEdit customers) ([1998 press release](https://web.archive.org/web/20040319155749/http://www.barebones.com:80/company/press.php?news_id=63)). The release announcement already names its signature technologies: "Distributed Filters™", approximate ("fuzzy") searching at 20–100% accuracy, a "high-tech database (powered by NeoAccess™)", OSA scripting that is both scriptable and *recordable*, and BBEdit's text engine including BBEdit plug-in support (ibid.).

**Version timeline** (dates from the [Bare Bones history page](https://www.barebones.com/company/history.html) and [mailsmith.org](http://www.mailsmith.org/support/mailsmith/updates.html)):

| Version | Date | Highlights |
|---|---|---|
| 1.0 | 1998-05-04 | Debut; POP3/SMTP; Classic Mac OS (Mac OS 7.5+, MacTCP or Open Transport per [ATPM](https://www.atpm.com/5.01/page8.shtml)) |
| 1.0.1 | 1998-05-27 | Maintenance |
| 1.1 | 1998-09-01 | "Improves database performance significantly" |
| 1.1.1 | 1998-10-26 | Quoted-text coloring |
| 1.1.2 | 1998-11-25 | Performance improvements |
| 1.1.3 | 1999-04-02 | POP Monitor window, improved Rewrap tool ([MacTech](https://www.mactech.com/1999/04/02/npl-mailsmith-1-1-3/)) |
| 1.1.4 | 1999-07-28 | Composition/viewing enhancements, OSA scripting growth, custom menu keys, Open Transport/PPP support ([MacTech](https://www.mactech.com/1999/07/29/npl-mailsmith-1-1-4/)) |
| 1.1.5 | 1999-10-15 | Maintenance |
| 1.1.6 | 2000-10-31 | SMTP authentication |
| 1.1.7 | 2001-07-09 | Improved SMTP auth |
| 1.1.8 | 2001-08-24 | Export entire mailboxes to mbox |
| 1.5 | 2002-03-25 | Carbonized: native Mac OS X + OS 9 in one binary; PCRE grep engine; easier ("basic") filter creation ([history](https://www.barebones.com/company/history.html), [ATPM review](https://atpm.com/8.04/mailsmith.shtml)) |
| 1.5.1 | 2002-04-15 | "Smart" context-sensitive columns |
| 1.5.3 | 2002-06-06 | Relevance ("Find Messages About") searching |
| 2.0 | 2003-06-18 | Mac OS X only; big performance rework; integrated SpamSieve, PGP 8, Apple Address Book ([MacTech PR](https://www.mactech.com/2003/06/18/mailsmith-2-0/amp/)); free update for 1.5 owners |
| 2.0.1 | 2003-08-20 | SSL support; SpamSieve bundled with purchases ([history](https://www.barebones.com/company/history.html)) |
| 2.0.2 | 2003-09-23 | Maintenance |
| — | 2003-12 | Mailsmith 2.0 wins Macworld 2003 Editors' Choice Award ([history](https://www.barebones.com/company/history.html)) |
| 2.1 | 2004-01-06 | Announced at Macworld SF: automatic attachment encoding by file type, keyboard command for quick mailbox moves with type-selection, enhanced SMTP, improved random signatures, Panther support ([Macworld](https://www.macworld.com/article/169367/mailsmith-10.html), [Tsai](https://mjtsai.com/blog/2004/01/06/mailsmith_21/)) |
| 2.1.1–2.1.5 | 2004-02-03 → 2005-03-09 | Refinements incl. Unix Tools piping and server-side spam-header honoring (2.1.2, 2004-07-21) ([history](https://www.barebones.com/company/history.html), [MacTech 2.1.5](https://www.mactech.com/2005/03/09/bare-bones-software-releases-mailsmith-2-1-5-update/)) |
| 2.2 (beta) | 2007-04 | "Semi-public beta" — Universal binary, Spotlight, Unicode, BBEdit 8.6-style clippings, format=flowed, inline spell-check, spam scores ([Tsai](https://mjtsai.com/blog/2007/04/02/mailsmith-22-semi-public-beta/)) |
| 2.2 | 2009-08-18 | Released as **freeware** by new owner **Stickshift Software**; new storage model (one data file per message + DB), Spotlight importer, Unicode throughout, flagged status, "Do Not Disturb", menu-hiding prefs ([2.2 release notes](http://www.mailsmith.org/support/mailsmith/arch_notes_221.html), [TidBITS](https://tidbits.com/2009/08/18/mailsmith-2-2-released-as-freeware-from-new-firm/)) |
| 2.3 | 2010-12-26 | 10.5+; Rebuild/Reconstruct Mailboxes commands, Reply to Sender / Reply to List, dock badge ([2.3 notes](http://www.mailsmith.org/support/mailsmith/arch_notes_23.html)) |
| 2.3.1 | 2011-04-11 | 10.5+ maintenance ([updates page](http://www.mailsmith.org/support/mailsmith/updates.html), [TidBITS watchlist](https://tidbits.com/watchlist/mailsmith-2-3-1/)) |
| 2.4 | 2015-08-31 | Intel-only, 10.9.5+, Developer ID code-signing, QuickTime dependencies removed ([2.4 notes](http://www.mailsmith.org/support/mailsmith/current_notes.html)) |
| 2.4.3 (build 480) | 2018-05-14 | Last release; collected fixes from "unofficial" 2.4.1/2.4.2 (ibid.) |

**Business story and decline.** Mailsmith was a commercial product at $79 (1.x) then $99 (1.5/2.x, with academic/cross-upgrade discounts; 2.x boxed SpamSieve licenses) ([ATPM 1.5 review](https://atpm.com/8.04/mailsmith.shtml), [Neuburg](https://tidbits.com/2003/07/28/true-confessions-of-a-mailsmith-switcher/)). Two structural problems killed it as a business:

1. **Apple's free Mail.app.** In a [2009 MacObserver interview](https://macobserver.com/tmo/article/digging_deep_into_bare_bones_with_rich_siegel), Siegel said Bare Bones wasn't in financial trouble but "wanted to focus development on the company's flagship products," adding: *"Competing with 'free,' be it in the form of services or bundled applications, really does make it difficult to justify investing the kind of resources that would have been necessary to implement and market a significant competitive feature set."* He declined to reveal user numbers ([TidBITS](https://tidbits.com/2009/08/18/mailsmith-2-2-released-as-freeware-from-new-firm/)).
2. **IMAP never shipped — despite being promised.** Siegel: *"We made an exception to this [never announcing unshipped features] once, and it didn't go well — ironically, it was for IMAP support in Mailsmith. We intended to support it, and said so publicly, but then when it came time to start planning and scheduling the details, we realized that it was just not going to happen."* ([MacObserver](https://macobserver.com/tmo/article/digging_deep_into_bare_bones_with_rich_siegel)). Every reviewer and obituary names no-IMAP as the deal-killer: Gruber switched to Mail in June 2007 "so as to start using IMAP with my iPhone" after using Mailsmith since 1.0 ([Daring Fireball](https://daringfireball.net/2007/08/rethinking_email)).

**What happened to the code and the team.** On 2009-08-18, Bare Bones transferred ownership to **Stickshift Software, LLC** (incorporated in Rhode Island, June 2009; [about page](http://www.mailsmith.org/company/)), a company Siegel created solely to keep Mailsmith alive; Siegel confirmed it would remain *"closed source"* but a *"labor of love"* ([TidBITS](https://tidbits.com/2009/08/18/mailsmith-2-2-released-as-freeware-from-new-firm/), [Bare Bones PR](https://www.barebones.com/company/press/mailsmith-transfer_pr.html)). Stickshift shipped 2.2 (2009), 2.3 (2010), 2.3.1 (2011), 2.4 (2015, Intel-only) and 2.4.3 (2018). mailsmith.org today states plainly: *"Mailsmith is no longer under active development. It will not be upgraded for 64-bit macOS compatibility. Future features or bug fixes are not in any way guaranteed to happen."* ([mailsmith.org](https://www.mailsmith.org/)). Because the app is 32-bit, it cannot run on macOS 10.15 Catalina (2019) or later — Apple dropped 32-bit app support there ([Apple 32-bit support note](https://support.apple.com/en-us/HT208436)) — and on Apple-silicon-only macOS 27 "Golden Gate" (released 2026-09-14, [Apple](https://support.apple.com/en-us/149035)) it is unambiguously dead outside virtualization or vintage hardware.

Credits in the manual (p. 2–3): product design — Rich Siegel, Patrick Woolsey, Jim Correia, Steve Kalkwarf, Dave Hill; engineering — Correia, Hill, Jon Hueras, Kalkwarf, Siegel, Steve Sisak; documentation — John Gruber, Steve Kalkwarf, Jerry Kindall, Jeff Mattson, William Porter, Siegel, Woolsey; additional engineering — Gavin Eadie, Polaschek Computing, Keith Stattenfield; icon design — Ultra Maroon Design. Seth Dillingham is credited for 2.2-era engineering (Manual p. 2). Notable: John Gruber wrote part of the documentation, and William Porter went on to write the TidBITS feature on distributed filtering. Third-party licenses (Manual p. 3): NeoAccess (NeoLogic Systems, the embedded OODB also used by AOL 3.0 and Netscape Communicator per [MacTech](https://www.mactech.com/1997/04/30/md1-neoaccess-used-by-aol/)), PCRE by Philip Hazel, plus University of Washington and Stanford University code (likely POP-related; the manual does not specify which components — the exact mapping is unverified).

The community hub was the **Mailsmith-Talk** list — originally a Bare Bones–hosted discussion list (referenced in the [2003–04 product pages](https://web.archive.org/web/20040211074233/http://www.barebones.com/products/mailsmith/features.shtml)), later a Google Group ([mailsmith.org/discuss](http://www.mailsmith.org/discuss/), still linked and nominally live as of 2026-10-11).

### 2.2 Feature inventory

The backbone here is the supplied 217-page manual (Mailsmith 2.2), cross-checked against the help book and the [2.2](http://www.mailsmith.org/support/mailsmith/arch_notes_221.html)/[2.3](http://www.mailsmith.org/support/mailsmith/arch_notes_23.html)/[2.4](http://www.mailsmith.org/support/mailsmith/current_notes.html) release notes.

**Protocols and accounts** (Manual ch. 3)

- POP3 + SMTP only; **no IMAP**, no NNTP, no LDAP (Manual p. 1; also flagged by every review).
- Per-account Checking/Sending/Advanced/Headers panels: POP server/credentials (Keychain-stored), connect at startup, arbitrary per-account poll interval, include/exclude from global check, leave-on-server, delete-after-N-days, don't-download-larger-than-N (partial download; "Get from Server" later fetches the whole message) (Manual p. 15–18).
- SMTP AUTH (CRAM-MD5, LOGIN, PLAIN in that order — added 1.1.6/1.1.7), SSL/TLS, APOP, custom ports, poppwd password change (Manual p. 17); SSL for server connections shipped in 2.0.1 ([history](https://www.barebones.com/company/history.html)).
- Per-account signature (fixed or random), per-account extra RFC 822 headers, spoofable X-Mailer/User-Agent "Mailer ID" (removed in 2.3 — Mailsmith then always emits `X-Mailer: Mailsmith x.y (zzz)`; [2.3 notes](http://www.mailsmith.org/support/mailsmith/arch_notes_23.html)).
- **POP Monitor**: inspect the POP mailbox *on the server* — list, preview, selectively download, save to mbox (optionally gzip in 2.2+), mark for delete (deletions commit on disconnect; Option-click = undelete, Command-click = delete immediately) (Manual p. 69–70). This gave POP users server control IMAP users take for granted.

**Storage** (Manual p. 8; [2.2 notes](http://www.mailsmith.org/support/mailsmith/arch_notes_221.html))

- All user data in `~/Mail/Mailsmith User Data/`: a `Mail` package, `Mail Account Settings.plist`, `Mail Filters`, `Mail Signatures.xml`, `These Are Not Saved Queries`, plus the `Mailsmith Support` folder (Action Scripts, Clippings, Menu Scripts, Plug-Ins, Scripts, Shutdown/Startup Items, Sounds, Stationery, Logs).
- Pre-2.2, each mailbox was a NeoAccess object database file. From 2.2: *"Message data is now stored in outboard data files, one per message. Among other things, this allows for Spotlight indexing of message data, significantly improved performance in all aspects of database operation, and much easier disaster recovery."* ([2.2 notes](http://www.mailsmith.org/support/mailsmith/arch_notes_221.html)). Databases were excluded from Time Machine while running; 2.3 stopped excluding the "Post Office" core DB to prevent restore-time data loss ([2.3 notes](http://www.mailsmith.org/support/mailsmith/arch_notes_23.html)). A 2.4 note confirms Spotlight shipped a `kind:mailsmith` content type via importer.
- Recovery: Rebuild Mailboxes (Cmd-Opt at launch; menu command in 2.3) and the last-resort Reconstruct Mailboxes (2.3) that rebuilds indexes from stored message files — at the cost of metadata not in the message data (notes, labels, read/replied status) ([2.3 notes](http://www.mailsmith.org/support/mailsmith/arch_notes_23.html)).
- Special fixed mailboxes: `(incoming mail)`, `(outgoing mail)`, `(drafts)`, `(errors)`, `(spam)`, `(trash)` — parentheses prevent deletion/renaming (Manual p. 58).
- Import: mbox (gzip too, 2.2+), Pegasus PMM, Eudora mailboxes + Eudora filters, Apple Mail `.mbox` packages; first-run importer for Eudora 4+, Claris Emailer 2.0v2+, Apple Mail (Manual p. 6–7; help book). Export: mbox (gzip optional), drag-and-drop to Finder. Addresses: Apple Address Book, built-in address book (removed in 2.2 → migrated to system AB), Eudora Nicknames, vCard 2.1.

**Composition** (Manual ch. 4)

- Recipient panel with Address Book autocomplete, drag/drop addresses and clippings; Return = To, Shift-Return = CC, Shift-Option-Return = BCC (Manual p. 23–24); system "people picker" from 2.2.
- Priorities Urgent/Important/Normal/Low/Bulk; Close After Sending; Copy Sender (self-Bcc); Suppress Recipient List (Manual p. 25).
- Enclosures: per-attachment encoding Automatic/AppleDouble/Base64/BinHex; StuffIt or Zip compression (2.2); PGP encrypt/sign (Manual p. 26–27).
- Private per-message **Notes** — a free-text field never transmitted, searchable via Advanced Query (Manual p. 27).
- Reply/Reply to All/Forward/Redirect/Re-Send; Redirect preserves original sender (bounce-style) (Manual p. 28).
- Configurable attribution string with `#date#`, `#time#`, `#sender#` plus ICU `#date-time fmt#`/`#local-date-time fmt#`; configurable quote string; cursor placement before/after quote; forward as quoted text or MIME attachment (Manual p. 28–29).
- Netiquette **warnings**: excessive-quote ratio, sigdash checking, "Me Too" detection, URLs lacking `<…>` (Manual p. 29–30, 162).
- Signatures: central Signatures window, per-account default, **random signatures** drawn from a flagged subset, stored in `Mail Signatures.xml` (Manual p. 30–31).
- Send Now (Cmd-E) vs Queue for Sending (Cmd-Opt-E) with Option-toggle of the button default; drafts in `(drafts)` re-openable (Manual p. 32). (In 1.x drafts were saved as Finder files; the `(drafts)` mailbox arrived later — the 1999 ATPM review notes its absence in 1.1.2: [ATPM](https://www.atpm.com/5.01/page8.shtml).)
- 2.3 added "Reply to Sender" and "Reply to List" (uses `List-Post` header) ([2.3 notes](http://www.mailsmith.org/support/mailsmith/arch_notes_23.html)).
- 2.2 added **format=flowed** (RFC 3676) sending and display, retiring the hard-wrap preference ([2.2 notes](http://www.mailsmith.org/support/mailsmith/arch_notes_221.html)).

**Editing — the BBEdit engine** (Manual ch. 5, App. A–B)

- **Multiple clipboards**: six-slot rotating clipboard history; Cut/Copy & Append; Paste Quoted (Cmd-Opt-V); Paste Previous (Manual p. 35–36).
- **Multiple undo/redo** limited only by memory (Manual p. 36).
- **Split panes**: any window splits into two independently scrolling views (Manual p. 36–37).
- **Rectangular selection** (Option-drag); numeric-keypad cursor mode; F1–F4 = Undo/Cut/Copy/Paste; **Emacs bindings** mode ("a comfort blanket", not full Emacs) (Manual p. 39–41, 149, 197–198).
- Go to Line; Show Invisibles (tab Δ, space ◊, return ¬); Balance While Typing; Smart Quotes; Auto-Indent; Auto-Expand Tabs; soft wrap to window/margin/N chars (Manual p. 42–43, 149–150).
- Text transforms on the Text menu: Balance (Cmd-B), Exchange Characters/Words, Change Case, Shift Left/Right, Add/Remove Line Breaks, **Rewrap Quoted Text** (unwraps each quote-depth run and reflows preserving `>` levels — the signature mail-editing command), Increase/Decrease Quote Level, Strip Quotes, Hard Wrap (with paragraph-fill styles), **Zap Gremlins** (delete/escape/replace non-ASCII, control chars, nulls), Entab/Detab (Manual p. 45–48, 50–51).
- Spell check: as-you-type underlining (2.2+), Find Next/All Misspelled Word, Spelling panel (Manual p. 52–54).
- Font/tabs per window; **Save Default Window per screen configuration** (Manual p. 44).

**Mail browsing** (Manual ch. 7)

- Mail Browser = mailbox outline | message list | preview, panes resizable; alternatively Mailbox List + one two-pane window per mailbox (Eudora-style) or one window per message (Manual p. 57–61; [ATPM 1.1.2](https://www.atpm.com/5.01/page8.shtml) describes the three-pane browser in 1999).
- Per-mailbox and global column sets, reorderable columns, remembered sort orders; smart "From/To" and "Date Sent/Received" columns flip meaning in incoming vs outgoing mailboxes (Manual p. 62–63).
- Status column glyphs: • unread, D redirected, F forwarded, R replied, ✓ sent; attachment paperclip; label color dots; 2.2 added Flagged column and Spam Score column ([2.2 notes](http://www.mailsmith.org/support/mailsmith/arch_notes_221.html)).
- Space bar pages through a message then advances to the next (Manual p. 64); Show Internet Headers (Cmd-D); Show Message Source; Display HTML in Browser (Mailsmith never renders HTML inline; HTML-only messages get a synthesized plain-text rendering) (Manual p. 64–65).
- PGP integration: decrypt on view, signature-verification icon in the status bar (Manual p. 65–66).
- Open Digest: explode a MIME digest into a message list (Manual p. 66; also marketed as "digest bursting" in the [2.0 PR](https://www.mactech.com/2003/06/18/mailsmith-2-0/amp/)).
- **Copy Item Link**: a `mailsmith:` URL per message usable system-wide (Manual p. 68).
- 32 labels (8 Finder labels + 24 custom name/color/order) (Manual p. 153); 5-level priorities.
- Per-mailbox "Notification Enabled" toggle — mute new-mail alerts for low-priority boxes (Manual p. 68–69).
- 2.1 added keyboard-driven filing: "a new keyboard command that quickly moves messages between mailboxes" with type-ahead selection of the destination ([Macworld](https://www.macworld.com/article/169367/mailsmith-10.html), [Tsai](https://mjtsai.com/blog/2004/01/06/mailsmith_21/)).

**Filters** (Manual ch. 9) — the crown jewel

- **Distributed filtering**: filters are first-class named objects (Filters window) but do nothing until *attached to mailboxes*. On arrival, a message is offered to the filters of each top-level mailbox (alphabetical order — users rename "1 Work", "2 Personal" to control traversal); when a mailbox's filter fires a **Deposit** action the message drops into that mailbox and is then offered to that mailbox's *children*; mailboxes later in the same level never see it. The mailbox hierarchy *is* the flowchart (Manual p. 82–84; [Porter, TidBITS part 1](https://tidbits.com/2002/09/23/mailsmith-and-distributed-filtering-part-1/)).
- `(outgoing mail)` is processed through the same system after sending — outgoing filtering is just filters attached at the top level; no separate rule class (Manual p. 99–100). 2.2 added send-disposition options: leave in outgoing, move to original's mailbox, move to chosen mailbox ([2.2 notes](http://www.mailsmith.org/support/mailsmith/arch_notes_221.html)).
- Filter creation: Make a Filter from a selected model message; File ▸ New ▸ Filter; or quick "auto filters" when creating a mailbox (`•auto•`-prefixed) (Manual p. 87–90).
- Criteria = logic row (`if`/`and`/`or`/`or else` = XOR) + message part + match method + settings. Parts include all header fields, Body, Header Text, Label, Priority, Read/Answered/Sent flags, **Sender is Known** (Address Book membership), **Sender's Group Membership** (Address Book group), **Server Account** (synthetic `user@popserver`), Primary Recipient, (any recipient), (any address), On Server, Partial, Is Spam/Is Not Spam, Spam Score, Size, List-ID, Notes, Flagged (Manual p. 91–92; List-ID/Flagged/Size per [2.2 notes](http://www.mailsmith.org/support/mailsmith/arch_notes_221.html)).
- Match methods: equal/less/greater; contains/starts/ends; **Matches Grep Pattern** (full PCRE); **Approximately Matches** (fuzzy, 20–100% slider) (Manual p. 92).
- Fine-grained address matching: full address, name, user ID, or server component; case and whole-word toggles (Manual p. 93).
- Criteria evaluate linearly — no nested grouping; "(A and B) or (C and D)" needs two filters or a grep alternation (Manual p. 93–94).
- **Twenty-one actions** (Manual p. 97): the named ones are Deposit, Transfer (named destination; ends all filtering — must be last action), Copy, Set Label, Set Priority, Set Status/Clear Status (incl. Flagged in 2.2+), Mark Read/Unread, Set Subject, Play Sound, Notify (custom alert text), Print, Reply to Sender / Reply to All Recipients (autorespond from a saved text file), Report to SpamCop, Run Script (AppleScript via `on FilterMessage(theMessage)`), Open Message's Mailbox, Extract Enclosures, Stop Filtering This Message, plus delete/move-to-trash class actions (Manual p. 94–98).
- One filter can be attached to many mailboxes — the same "from me" filter deposits into *whichever* mailbox is evaluating it (Manual p. 84).
- **Re-Apply Filters** re-runs selected messages through the pipeline later — enabling "cradle to grave" flows (read-but-unmarked list mail auto-trashes on re-apply) (Manual p. 101–102, example p. 80–81; [TidBITS part 1](https://tidbits.com/2002/09/23/mailsmith-and-distributed-filtering-part-1/)).
- Filters enable/disable per mailbox; drag to reorder; import/export as XML ("human-readable, not human-writable"); full filter logging for troubleshooting (Manual p. 103–104). `Skip Remaining Filters` exists (fixed in 2.3).

**Search** (Manual ch. 10–11)

- Find in Window: literal or grep; replace/replace-all with `&`, `\1`–`\99`, named subpatterns, `\U\L\l\E` case transforms; wrap-around, backwards, entire-word (Manual p. 105–107).
- Simple Query (mailbox picker + term + parts); reworked in 2.2 with Exact/Any/All multi-term modes and multi-mailbox selection with Option/Command-click semantics ([2.2 notes](http://www.mailsmith.org/support/mailsmith/arch_notes_221.html)).
- **Advanced Query**: the same criteria engine as filters, unlimited rows, over all mail or a mailbox + subtree; results in a Query Results browser (Manual p. 108–110).
- AppleScript `whose` clauses query the store directly and can materialize a sortable mail list window (Manual p. 178–179).
- **Grep = PCRE** — the manual devotes ~30 pages (pp. 112–141) to a regex tutorial: classes, quantifiers, lazy matching, numbered + named backrefs, lookahead/lookbehind, `(?>…)` once-only, `(?R)` recursion, `(?imsx)` modifiers, comments. A regex *user's guide* inside an email manual.
- 1.5.3 added "Find Messages About" — Sherlock-style relevance-ranked fuzzy search ([history](https://www.barebones.com/company/history.html)); 2.2 added system-wide **Spotlight** indexing (`kind:mailsmith`) (ibid.).

**Clippings / Glossary** (Manual ch. 13)

- Named text snippets in `Mailsmith Support/Clippings/`, organized in sets (subfolders, aliases, `NN)` ordering prefixes, `-***` divider folders); a set matching an account name auto-activates for that account (Manual p. 165–167).
- Insert Clipping: type partial name + invoke, or pick from filterable panel; menu, palette, or key equivalents; Save Selection as Clipping (Manual p. 168–169; 2.2 collapsed to a single "Insert Clipping" panel with fuzzy matching, [2.2 notes](http://www.mailsmith.org/support/mailsmith/arch_notes_221.html)).
- Placeholders: `#CLIPBOARD#`, `#DATE#`, `#DATETIME fmt#`/`#DATETIME_GMT fmt#` (ICU), `#FILE#`, `#INDENT#`, `#INLINE#`, `#INSERTION#` (caret; extra become `#•#` jump placeholders), `#LOCALTIME#`, `#NAME#`, `#SCRIPT name#` (run an AppleScript, insert result), `#SELECT#`, `#SELSTART#`/`#SELEND#`, **`#SYSTEM shell_script#`** (run a Unix command, insert output), `#TIME#`, `#UUID#` (Manual p. 170–172).

**Scripting & plug-ins** (Manual ch. 14–15)

- AppleScript: fully scriptable, **recordable** (drive the UI, get a draft script), **attachable** — Menu Scripts named `Menu•Item` intercept menu commands (`menuselect` returning true suppresses the built-in command; `postmenuselect` runs after) (Manual p. 173–174, 180–182). The architecture was deliberately "factored" so the UI talks to the core via scripting (Neuburg's analysis, [TidBITS](https://tidbits.com/2003/07/28/true-confessions-of-a-mailsmith-switcher/)).
- Startup Items / Shutdown Items folders run scripts at launch/quit; Action Scripts folder feeds filter Run-Script actions and notification scripts (Manual p. 10–11, 98).
- Scripts menu + palette with keyboard assignments; Bare Bones hosted a user-contributed Mailsmith Scripts library ([2003 site nav](https://web.archive.org/web/20040211074233/http://www.barebones.com/products/mailsmith/features.shtml)).
- **BBEdit plug-in compatibility**: the BBEdit 8.5+ plug-in API, Tools menu + floating palette, per-plug-in keys (Manual p. 183); SmartWrap (Selznick Scientific) named in the manual (p. 66).
- "Run Unix Command on Each Received Message": pipe each message through an arbitrary Unix filter, stdout becomes the message (bogofilter integration documented) (Manual p. 152).

**Notification & misc** (Manual ch. 12)

- New mail: alert, Dock bounce/badging (2.3), PowerMate flash, open mailboxes, custom sounds, run script; **Growl** integration (2009 era, C→ObjC API in 2.3); "Quiet Time" schedule suppresses sounds (Manual p. 156–158).
- `(errors)` mailbox + `ErrorLogged` script hook (Manual p. 157–158).
- Spam: SpamSieve deeply integrated (Mark as Spam/Non-Spam train it; Is Spam criteria; spam score 0–100 column + Recalculate Spam Score in 2.2; honors SpamAssassin/Bogofilter headers; "run unix command per message") (Manual p. 71–73, 152, 160–161).
- Menus prefs: reassign any menu key, **hide any menu item or entire menus**, configure contextual menu contents, key autorepeat toggle (Manual p. 155–156; [2.2 notes](http://www.mailsmith.org/support/mailsmith/arch_notes_221.html)).
- Preferences are `defaults write`-able (`com.barebones.mailsmith`); dozens of documented expert prefs.
- Printing with page headers, line numbers, gutter, frame; "Send PDF with Mailsmith" print integration (2.2).

### 2.3 What users loved — contemporary reception and later reminiscence

**Contemporary reviews.**

- Michael Tsai, [ATPM review of 1.1.2](https://www.atpm.com/5.01/page8.shtml) (Jan 1999, rated "Good" 3/5): *"Mailsmith breaks out of the evolutionary rut that other e-mail clients confine themselves to… Mailsmith's distributed filtering is revolutionary and extremely elegant. Likewise, there is no better mail composition environment than Mailsmith (except perhaps BBEdit)."* He docked it for performance — "I find it unusable on anything slower than a PowerPC 604" — and storage bloat (~3× Outlook Express for the same 1000 messages).
- Tsai again, [ATPM review of 1.5](https://atpm.com/8.04/mailsmith.shtml) (Apr 2002, "Very Nice"): *"Mailsmith is arguably the best e-mail client for Mac OS X… the database still leaves a lot to be desired"* — 559 messages took 11+ minutes to download; modal operations; but browsing was "snappy… you can arrow-key from message to message without any delay" unlike Eudora.
- William Porter, [TidBITS review of 1.5](https://tidbits.com/2002/07/15/mailsmith-1-5-lean-mean-email-machine/) (Jul 2002): *"no program gives you more or better ways to create and use filters than Mailsmith"*; praised regex utility ("run this on my main mailbox and found 90 messages; only four… not marketing messages"), recordable AppleScript, custom keystrokes for every command; griped about no multi-sibling-mailbox search and unsavable queries.
- Porter's [distributed-filtering two-parter](https://tidbits.com/2002/09/23/mailsmith-and-distributed-filtering-part-1/) (Sep 2002) remains the best explanation of why the feature mattered: *"my incoming mailbox has no filters attached to it at all… If mail does not end up where it is supposed to, I just observe where it does end up and climb back up the mailbox tree."*
- Matt Neuburg, ["True Confessions of a Mailsmith Switcher"](https://tidbits.com/2003/07/28/true-confessions-of-a-mailsmith-switcher/) (Jul 2003): the "Iron Triangle" of text handling, scriptability, filters; candid that 1.5 was "dog-slow… crashed remarkably often – about once a day," while 2.0 "feels peppy, clean, crisp, and totally reliable."
- Andy Ihnatko, [Macworld review of 2.0](https://www.macworld.com/article/168357/mailsmith2.html) (Oct 2003): pros — "good integration with PGP, SpamSieve, and OS X's Address Book; powerful scripting, mail filtering, and text-manipulation features"; cons — "no integrated search filter [for the toolbar]; user interface needs a makeover; no as-you-type spelling checker; no support for IMAP, LDAP, or nonroman text." He credited 2.0 with jettisoning "the profound wonkiness of its database engine."
- Mailsmith 2.0 won the **Macworld 2003 Editors' Choice (Eddy) Award** ([history](https://www.barebones.com/company/history.html)).

**Why text-centric users loved it** (synthesis of the above + recollections):

- *Mail is text; treat it like text.* The value proposition was that composition used "the essential capabilities of BBEdit" ([1998 PR](https://web.archive.org/web/20040319155749/http://www.barebones.com:80/company/press.php?news_id=63)) — real undo, real find/replace with regex, Rewrap Quoted Text to fix mangled threads, Change Case for the all-caps sender, Zap Gremlins for MSWord smart-quote junk.
- *No HTML in your face.* Porter: "Mailsmith has been designed by people who believe that email should be plain text." Fleishman used it partly because he was "suspicious of HTML-formatted email, because it can embed tracking images" ([TidBITS 2009](https://tidbits.com/2009/08/18/mailsmith-2-2-released-as-freeware-from-new-firm/)) — an early articulation of the tracking-pixel objection.
- *Script everything.* Gruber's [menu-script "Send Now" defense](https://daringfireball.net/2004/02/send_now_defense) (Feb 2004) is the canonical anecdote: he attached an AppleScript to Message ▸ Send Now to catch muscle-memory misfires of Cmd-E — "it has completely solved a rare but highly annoying problem I've suffered for over five years."
- *SpamSieve.* SpamSieve 1.0 (2002-09-10, $10, [C-Command](https://c-command.com/blog/2002/09/10/spamsieve-1-0/)) originally drove Mailsmith/Entourage via scripts; Mailsmith 2.0 baked it in — "no scripts required, no configuration necessary" (Siegel, [Macworld](https://www.macworld.com/article/166699/mailsmith-7.html)); Tsai noted details like suppressing Dock bounce when a whole batch was spam ([his blog, 2003-06-18](https://mjtsai.com/blog/2003/06/18/)).
- *Brent Simmons switched from Apple Mail to Mailsmith+SpamSieve in Jan 2004* because Mail was slow and Mailsmith "was faster than Mail. It's very scriptable and customizable — for instance, I wanted to give some of the menu commands the same keystrokes that I was used to in Mail, and I could" ([inessential.com](https://inessential.com/2004/01/06/why_im_switching_to_mailsmith_and_spa.html)).

**Later reminiscences** (recollection, not contemporaneous fact):

- Watts Martin (via [Tsai, Aug 2026](https://mjtsai.com/blog/2026/08/21/the-best-worst-e-mail-client/)): "The last email program I truly loved was Bare Bones' Mailsmith"; calls MailMate "the most obvious spiritual descendant of Mailsmith."
- Riccardo Mori used it daily into the 2010s for secondary/list accounts: "it deals with thousands of messages without a problem… very fine-grained searches" ([morrick.me](https://morrick.me/archives/6487), also [2012](https://morrick.me/archives/5613), [2014](https://morrick.me/archives/7205)); in Aug 2026 he comments he'd "instantly leave Mail for [Mailsmith]" if actively developed ([Tsai comments](https://mjtsai.com/blog/2026/08/21/the-best-worst-e-mail-client/)).
- "Boy, do I miss Mailsmith and Claris Emailer" — Retrophisch, Oct 2017 ([micro.blog](https://retrophisch.micro.blog/2017/10/04/boy-do-i.html)), quoting Tsai.
- Hacker News: in ["BBEdit is 30 years old as a commercial product"](https://news.ycombinator.com/item?id=35904320) (May 2023), a commenter writes "I really miss BareBones Mailsmith, which was like BBEdit for mail… man alive it was good"; in ["BBEdit: Where Respect Is Due"](https://news.ycombinator.com/item?id=36303371) (June 2023), deltarholamda: "a POP mail client with BBEdit crammed in… if you needed to do clever things with it, it was fantastic. It never made the jump to IMAP AFAIK, so it kind of died out." Separately, in a [2012 Markdown discussion](https://news.ycombinator.com/item?id=4701282), chipotle_coyote credits Mailsmith's and BBEdit's ">"-quote-wrapping commands as the cultural ancestor of Markdown's blockquote convention.
- "Farewell, Old Friend" (RaisedWeird, Feb 2008 — a switcher *to* Apple Mail listing what he lost): "Mailsmith's text editing environment… the 'Rewrap' command"; "Mailsmith, which is the king of scriptability"; "Annotation. Mailsmith supports things like notes on emails and coloration under script control" ([byrneweb](https://byrneweb.com/sunburn/blog/2008/02/farewell-old-friend/)).

### 2.4 What users disliked / the real limits

1. **No IMAP** — the big one, documented everywhere ([Siegel's own admission](https://macobserver.com/tmo/article/digging_deep_into_bare_bones_with_rich_siegel); Gruber's 2007 departure; "the lack of IMAP may still prove a deal-killer," [Fleishman 2009](https://tidbits.com/2009/08/18/mailsmith-2-2-released-as-freeware-from-new-firm/)).
2. **Database performance and bulk.** Tsai measured mailboxes ~3–7× larger than competitors' (9.8 MB vs 1.4 MB mbox for the same mailbox), slow message ingestion (559 msgs > 11 min), modal operations blocking the UI ([ATPM 8.04](https://atpm.com/8.04/mailsmith.shtml)); Neuburg: 1.5 "dog-slow… crashed about once a day" ([TidBITS](https://tidbits.com/2003/07/28/true-confessions-of-a-mailsmith-switcher/)). 2.0 and 2.2 substantially fixed this, but the reputation persisted (Macworld's Ihnatko still dinged the UI).
3. **No HTML display or composition** — a feature to its audience, a dealbreaker to the mainstream; so was absence of inline image/movie display except via separate QuickTime windows (and removed with QuickTime in 2.4, [2.4 notes](http://www.mailsmith.org/support/mailsmith/current_notes.html)).
4. **Weak address book** (1.x–2.1's internal book: no multi-address contacts, clumsy groups) — "Nothing in Mailsmith better deserves to be described as 'bare bones' than the address book" (Porter); mitigated in 2.0+ by Apple Address Book integration and finally removed in 2.2 (migrated to system AB).
5. **Search UX gaps**: no incremental/type-as-you-find (Apple Mail/Entourage had it), couldn't show results as found, couldn't search sibling mailboxes at once (fixed in 2.2's Simple Query rework), unsavable queries (Porter's complaint; still true in the 2.2 manual).
6. **No non-Roman text** until Unicode arrived in 2.2/2009 (Ihnatko, ATPM reader Q&A on Japanese).
7. **POP's single-store assumptions**: one local store, no multi-device story, leave-on-server crude vs IMAP's server truth (the "stub" message UX annoyed Neuburg: downloading the stub didn't remind you the original remained on the server).
8. Small irritants documented by Neuburg: no Undo for message deletion, no inline spell check (until 2.2), awkward glossary placeholder discovery, Option-click inconsistency on Delete button.
9. **Filter criteria couldn't test Address Book membership until 2.0.1** (Aug 2003) — Tsai's peeve ([his blog](https://mjtsai.com/blog/2003/08/20/mailsmith_201_and_spamsie/)).

### 2.5 Lost features — where Mailsmith still beats 2026's mainstream

Reality-checked against current clients (Apple Mail rules per [Apple Mail User Guide](https://support.apple.com/guide/mail/use-rules-to-manage-emails-you-receive-mlhlp1017/mac), Mimestream per [mimestream.com](https://mimestream.com/) and its [filters help](https://mimestream.com/help/user-guide/filters), MailMate per [its manual](https://manual.mailmate-app.com/), Gmail filters per Google's filter model as surfaced in Mimestream's server-side filter UI; Thunderbird verified broadly — items marked where documentation is thin):

| Mailsmith feature | Apple Mail (macOS 26/27) | Outlook | Gmail (web) | Spark | Thunderbird | Mimestream | MailMate |
|---|---|---|---|---|---|---|---|
| Regex (grep) in search & filters | No | No | No | No | Add-on only (unverified for current) | No | Partial — header-criteria matching incl. regex-style operators ([manual](https://manual.mailmate-app.com/)) |
| Fuzzy/approximate search | No | No | No | No | No | No | No |
| Filters attach to mailbox hierarchy (distributed) | No — flat ordered rule list, client-side only ([Apple](https://support.apple.com/guide/mail/use-rules-to-manage-emails-you-receive-mlhlp1017/mac)) | Flat server/client rules | Flat server-side filter list | Smart folders (search, not actions) | Flat filter list per account | Server-side Gmail filters ([help](https://mimestream.com/help/user-guide/filters)) | Rules attachable per mailbox incl. smart mailboxes — closest analog |
| Filter criteria = notes/labels/spam-score/groups | Labels via rules partial; no notes; no group membership criteria | Categories | Some (label/has-attachment) | No | Tags yes; notes no | Gmail criteria only | Extensive incl. tags = IMAP keywords |
| Filter actions: run script / print / autoreply-from-file / SpamCop | AppleScript action exists ([Apple guide](https://support.apple.com/guide/mail/use-rules-to-manage-emails-you-receive-mlhlp1017/mac)); others no | Some (server rules) | Skip Inbox/label/fwd/delete only | No | Partial | Gmail actions only | Rules: mostly local actions |
| Re-apply rules manually to arbitrary selection | Yes (Apply Rules) | Yes | No | No | Yes | No (server-side runs at delivery) | applyRules: selector exists |
| Recordable/attachable scripting (menu-intercept scripts) | AppleScript exists but "left to rot" ([Carmo via Tsai, Aug 2026](https://mjtsai.com/blog/2026/08/21/the-best-worst-e-mail-client/)); no attachability | VBA desktop / Office Scripts | No | No | WebExtensions | No | Extensible via custom command "bundles" (TextMate-style; depth unverified) |
| BBEdit-grade editor (multi-clipboard, rewrap-quote, rect selection, unlimited undo) | No | No | No | No | No (HTML composer) | No | Plain-text composer w/ Markdown; closest |
| Multiple clipboards | No | No | No | No | No | No | No |
| Per-message private Notes (searchable) | No | Categories only | No | No | No | No | No (tags via IMAP keywords, not free text) |
| Clippings/text factories with placeholders (#SYSTEM#, #SCRIPT#) | No (sigs only) | Quick Parts/Templates | Templates | Templates | Templates | No | Bundles can generate text — partial |
| Custom key equivalents for every menu command + hide menus | No | No | Web shortcuts only | No | Partial (add-ons) | Fixed set | Custom keybinding plist files ([manual](https://manual.mailmate-app.com/custom_key_bindings)) — yes |
| POP Monitor (server-side triage without download) | N/A | N/A | N/A | N/A | N/A | N/A | N/A — concept lives on as "server rules" |
| Random signatures | No | No | No | No | Add-on | No | No |
| Plain-text-only philosophy w/ synth text view of HTML | Plain-text pref weak | Plain-text option | Plain-text compose only | No | Plain-text option | Plain text compose yes | Yes |
| Digest bursting | No | No | No | No | No | No | No |
| `kind:` Spotlight search of the mail store | Mail messages are in Spotlight | Partial | N/A | ? | No | Spotlight integration yes | Messages are files → yes via Spotlight |

Table caveats: cells marked ?/unverified mean I could not find authoritative current docs; Gmail/Outlook rows describe the *filter model*, not exhaustive capability matrices. Spark Desktop moved to Electron + subscriptions ([Tsai link roundup](https://mjtsai.com/blog/2026/08/21/the-best-worst-e-mail-client/)) — which eliminates its claim to "native" anyway.

The deep pattern: modern clients converged on *flat, delivery-time, server-synced rules* (Exchange rules, Gmail filters, Sieve). Nobody ships *contextual* filtering tied to a user-arranged mailbox tree, nobody ships regex-filter in the client, nobody ships a scriptable/attachable message-processing pipeline, and nobody ships a real text-editor engine in the composer. MailMate alone preserves the power-user substrate (smart mailboxes, per-header matching, keybinding files, tags as IMAP keywords) but stops short of editor depth and attachable scripting.

### 2.6 Re-imagining for 2026 — what to revive, and where it must live

Sweetbriar's architecture (Rust core: sync/storage/MIME/search/backends + native macOS UI) maps well onto these revivals. Verdict per feature: **C** = client-side only; **S** = needs server support; **H** = hybrid.

| Revived feature | C/S/H | Implementation sketch for Sweetbriar |
|---|---|---|
| Regex + fuzzy search of all mail | C | Rust `regex` crate (or `pcre2` for PCRE fidelity) over a local index — SQLite FTS5 for terms + a regex scan fallback, or a trigram-accelerated regex engine (à la `ripgrep`/Tantivy). All local, works on every backend. Fuzzy ≈ Levenshtein/automaton index (Tantivy fuzzy queries). |
| Advanced Query (multi-criteria structured search) | H | Client-side against the Rust store for everything; *also* push down to server search where available (IMAP `SEARCH`, JMAP `Email/query`, Gmail `users.messages.list q=`). Keep local truth: mixed accounts mean client evaluation is authoritative. |
| Distributed filtering | C (rules engine), H (results visible server-side) | The mailbox-tree flowchart is pure client logic in the core. Effects (move/label/flag) map to IMAP `COPY/STORE`/`MOVE` (RFC 6851), JMAP `Email/set` mailboxIds/keywords, Gmail `modify`, Exchange Graph `move`. *Counterpoint*: server-side Sieve ([RFC 5228](https://www.rfc-editor.org/rfc/rfc5228.html); manageable via [RFC 9661 JMAP-for-Sieve](https://www.rfc-editor.org/rfc/rfc9661.html)), Gmail filters, or Exchange Inbox rules run when the Mac is asleep — Sweetbriar should **also** expose a server-rule editor (like Mimestream's Gmail filter UI) rather than pretend client rules are portable. |
| Re-apply filters on demand ("cradle to grave") | C | Cheap: re-evaluate predicates against stored metadata. |
| Filter actions incl. scripts | C for local actions; S for anything delivery-time | Local actions: move/label/flag/notify/run-script/print/save-attachments are fine. Reply/autorespond should be offered server-side where the backend supports it (Sieve `vacation`, Exchange automatic replies via Graph) because a laptop reply-bot is fragile. |
| Message notes / private annotations | H | Store in the local DB keyed by Message-ID; *sync* via IMAP `$Label`-style keywords or `X-*` headers on a per-account opt-in (Eudora/Thunderbird precedent of writing headers); JMAP has no notes field — a keyword like `$SweetbriarNote` + sidecar is pragmatic. |
| Clippings/stationery/placeholders | C | Pure client. Placeholder system (caret, clipboard, selection, shell-command output, script) is straightforward; keep `#SYSTEM#` sandboxed/flagged. |
| Random signatures | C | Trivial. |
| BBEdit-grade text editing | C | Swift text stack (TextKit 2) — must implement rewrap-quoted-text, rect selection, multi-clipboard, multi-undo in the composer/viewer. This is the differentiator users still cite. |
| Menu-script attachability / intercept built-in commands | C | AppleScript support (NSScriptingRegistry) on macOS gives attachability analog: scripts named `Menu•Command` can intercept or wrap menu items, exactly Gruber's use case. On Linux, equivalent via the native toolkit's scripting (DBus/scripting layer later). |
| Recordable scripting | C (macOS) | AppleScript recording requires `OSASetRecordingDelegates` plumbing; expensive to build — deprioritize vs. a good automation API. |
| Menu/keyboard customization incl. hiding menus | C | `keyEquivalents` driven by prefs file; MailMate's plist keybinding files ([manual](https://manual.mailmate-app.com/custom_key_bindings)) prove demand. |
| Spotlight exposure of mail | C | On macOS 26/27: still via `.mdimporter` plugins or Core Spotlight/`CSImportExtension`; Objective-See documents that mdimporters still work on Tahoe 26 ([blog](https://objective-see.org/blog/blog_0x81.html)); App Intents actions surfaced in Spotlight are the new avenue ([Apple newsroom, macOS 26](https://www.apple.com/kw/newsroom/2025/06/macos-tahoe-26-makes-the-mac-more-capable-productive-and-intelligent-than-ever/)). Keep message bodies as real files (maildir-like) to make this nearly free. |
| POP Monitor | S-ish | With IMAP/JMAP the concept generalizes to "inspect server state without mutating local": headers-only browse, server quota, sieve state. Simplest: IMAP `FETCH … BODY.PEEK[HEADER]`. |
| Digest bursting | C | Split `multipart/digest` on ingest (display-level, don't rewrite store). |
| Per-account poll/check intervals + quiet hours + DND | C | Core scheduler; cheap. |
| Spam via external engine (SpamSieve-style) | C/H | Modern analog: server-side provider spam is now good; local bayesian is a niche. Integrate SpamSieve if it still exists (it does — C-Command sells it; current per c-command.com) or offer a generic "pipe message to command" = the Unix-tool filter revived, which also enables `bogofilter`/`rspamd`-as-a-service. |
| Notifications w/ scripts + per-mailbox mute | C | UNUserNotificationCenter + script hooks + per-mailbox mute flag. |
| Copy Item Link | C | `sweetbriar:` URL scheme + drag-out `.eml`; Spotlight jump-back. |
| Save-as-mbox / import mbox, Eudora, Apple Mail | C | Core importer: mbox is trivial; Apple Mail `.mbox` packages documented-ish; Eudora/PMM legacy importers optional. |
| Leave-on-server/quota triage | S | IMAP makes "leave on server" meaningless — the server *is* the store; replace with "local retention policy per mailbox" (keep headers vs full body vs attachments stripped) — the honest 2026 equivalent. |

## 3. Options compared

**Filter architecture options for Sweetbriar:**

| Model | Examples | Pros | Cons |
|---|---|---|---|
| Flat ordered rules (client) | Apple Mail rules, Thunderbird | Familiar, easy mental model | Ordering bugs; no reuse/context; doesn't scale to hundreds of rules |
| Flat server rules | Gmail filters, Exchange Inbox rules, Sieve | Runs offline/server-side, uniform across devices | Server-controlled expressiveness; no scripts; provider-specific editors |
| **Distributed (mailbox-attached)** | Mailsmith | Hierarchy = flowchart; context-sensitive reuse (one filter many mailboxes); local reasoning ("climb the tree to debug") | Novel concept = learning curve; meaningless if folders are remote-flat; doesn't run while client offline |
| *Recommendation:* layered — smart-mailbox queries (read path) + flat-to-tree hybrid filters where actions may deposit/transfer (write path), plus a separate server-rule editor per backend. | | | |

**Storage options:**

| Model | Examples | Pros | Cons |
|---|---|---|---|
| Single monolithic DB | Entourage, pre-2.2 Mailsmith (per-mailbox OODB) | Simple | Corruption blast radius; backup-unfriendly (1.x flaw noted by Tsai/Porter) |
| mbox folders | Eudora, Thunderbird | Interop | Rewrite-on-change; weak concurrent access |
| **Per-message files + rebuildable index** | maildir, Mailsmith 2.2+ ("outboard data files, one per message"), Apple Mail `.emlx` | Spotlight nearly free; per-file backup; index is disposable (Rebuild/Reconstruct) | Index rebuild cost; metadata (notes/labels) must live in sidecar/DB with journaling |
| Cloud-only | Mimestream (cache) | Always consistent w/ server | No durable local guarantee — contradicts Sweetbriar's data-safety goal |

**Scripting options:**

| Approach | Pros | Cons |
|---|---|---|
| AppleScript (NSScriptingRegistry) | Mailsmith-proven; attachable; users exist; Shortcuts/App Intents bridge | macOS-only; verbose; recording is deep work |
| Modern automation API (App Intents + URL scheme + embedded JS/Lua) | Cross-platform core possible; testable | Less discoverable; no recording |
| Both: thin AppKit AppleScript façade over a core automation API | Mailsmith's "factored" design reborn; Linux UI can drive same API | Engineering cost; keep scope tight (messages/mailboxes/compose/search) |

## 4. Recommendations for Sweetbriar

1. **Adopt the 2.2 storage model wholesale**: per-message files + disposable index (SQLite FTS5 + structured metadata). Mailsmith's own evolution (NeoAccess monolith → outboard files) is the strongest possible endorsement — they rebuilt for data safety and Spotlight, and the manual's Rebuild/Reconstruct commands are exactly the recovery story Sweetbriar wants. *Strongest counter-argument:* per-message files + index is more code than "just Postgres/SQLite blob," and maildir fsync discipline is subtle (JMAP servers like Stalwart prove it's doable in Rust); but for a client whose pitch is "never lose mail," a database blob is a single point of catastrophic corruption. Verdict: do it.
2. **Ship the regex+fuzzy Advanced Query and the same criteria engine for filters and search** — Mailsmith's best idea was one criteria language shared by query, filters, and scripting. `regex` crate + Tantivy/SQLite FTS5; `Email/query` pushdown for JMAP. *Counter-argument:* regex-over-body doesn't scale past ~100k messages without a trigram index — mitigate with FTS5 first-pass narrowing.
3. **Implement filters-attached-to-mailboxes (the deposit semantics) as Sweetbriar's flagship differentiator** — it's the single most-praised unique feature (three separate reviewers call it revolutionary) and *nobody* ships it today. *Counter-argument:* it confuses users raised on Gmail filters and adds a second-order model to support; mitigate by also offering flat rules UI mapping onto the same engine (auto-filters did exactly this in 1.5).
4. **Editing**: rewrap-quoted-text, multi-clipboard, real undo, strip-quotes, rectangular selection in the composer; plain-text display with synthesized text view of HTML. *Counter-argument:* HTML composition is expected in 2026 (inline images for screenshots are legitimate); answer = compose plain text by default, attach/view images, never remote-load remote content.
5. **Per-message Notes**: searchable, syncable via opt-in IMAP keyword/JMAP. *Counter-argument:* clashes with server truth when notes sync partially; make sync opt-in per account.
6. **Scripting**: prioritize a stable automation API + AppleScript façade + menu-intercept scripts on macOS; defer recording. *Counter-argument:* AppleScript ROI looks poor in 2026 — but the Mailsmith evidence (Gruber, Tsai, Simmons, the Menu Scripts ecosystem) shows a tiny attachable surface yields disproportionate loyalty among exactly the users Sweetbriar targets.
7. **Do not resurrect**: POP Monitor (reframe as server-state inspection), BinHex/AppleDouble encoders, StuffIt, PGP-7/8-era integration (use `sequoia-pgp`/`rpgp` if PGP at all — but S/MIME via Security.framework is the saner 2026 default), PowerMate/Growl hooks, internal address book (Contacts framework + optional built-in groups are enough), menu-hiding (niche even for power users — keep keybinding customization only).

## 5. Risks and open questions

- **IMAP/server truth vs distributed filtering**: how do deposit/transfer semantics present when the folder tree is remote and shared? Needs a defined "evaluation scope" per account (client rules may only reorder *within* synced folders). Open design question.
- **Metadata durability**: Mailsmith's own Reconstruct command lost labels/notes/status because they lived only in the DB. Sweetbriar must either journal metadata or mirror it into keywords/headers — decide the durability contract early.
- **AppleScript maintenance burden**: AppleScript support is a long-tail maintenance cost on an AppKit app and impossible on the Linux UI; budget for a façade, not a model.
- **SpamSieve integration**: SpamSieve is still sold (C-Command); integration contract is AppleScript-based — verify current API before promising parity.
- **Unverified/unchecked**: exact 21-item filter-action list (manual p. 97 states the count and names most; full popup list not transcribed); whether the UW/Stanford credits are c-client-derived; Mailsmith-Talk list archive health on Google Groups; whether 2.4.x builds ever ran on Catalina via tricks (unlikely — 32-bit); Macworld's star rating for the Ihnatko 2.0 review (page shows pros/cons but rating not captured); sales figures (Siegel declined to disclose).
- **Manual version nuance**: the supplied manual is internally labeled 2.2 (© 2009) although it shipped on the 2.3.1 disk image — feature claims cited from it may slightly predate 2.3.x additions (Reply to List, Reconstruct Mailboxes, Dock badging), which I've sourced to release notes instead.

## 6. Sources (URLs actually consulted)

**Primary (supplied):** `sources/mailsmith-manual.txt` (Mailsmith User Manual, 2nd ed., 2009 — 217 pp.); `sources/help/` (HTML help book).

**Vendor/official:**
- [Bare Bones company history](https://www.barebones.com/company/history.html) — version dates 1997–2009
- [Mailsmith 1.0 press release (Wayback, 2004 capture)](https://web.archive.org/web/20040319155749/http://www.barebones.com:80/company/press.php?news_id=63)
- [Mailsmith 2.1 features page (Wayback, 2004 capture)](https://web.archive.org/web/20040211074233/http://www.barebones.com/products/mailsmith/features.shtml)
- [Mailsmith 2.0 press release (MacTech)](https://www.mactech.com/2003/06/18/mailsmith-2-0/amp/)
- [Mailsmith 2.1 announcement (Macworld)](https://www.macworld.com/article/169367/mailsmith-10.html); [2.0 SpamSieve news (Macworld)](https://www.macworld.com/article/166699/mailsmith-7.html); [2.1.1 news (Macworld)](https://www.macworld.com/article/169670/mailsmith-11.html); [2.1.5 (MacTech)](https://www.mactech.com/2005/03/09/bare-bones-software-releases-mailsmith-2-1-5-update/); [1.1.3](https://www.mactech.com/1999/04/02/npl-mailsmith-1-1-3/), [1.1.4](https://www.mactech.com/1999/07/29/npl-mailsmith-1-1-4/) (MacTech)
- [Ownership transfer PR](https://www.barebones.com/company/press/mailsmith-transfer_pr.html)
- mailsmith.org: [home](https://www.mailsmith.org/), [download](http://www.mailsmith.org/download/), [updates](http://www.mailsmith.org/support/mailsmith/updates.html), [2.2 notes](http://www.mailsmith.org/support/mailsmith/arch_notes_221.html), [2.3 notes](http://www.mailsmith.org/support/mailsmith/arch_notes_23.html), [2.4/2.4.3 notes](http://www.mailsmith.org/support/mailsmith/current_notes.html), [discuss](http://www.mailsmith.org/discuss/), [company](http://www.mailsmith.org/company/), [support](http://www.mailsmith.org/support/)

**Reviews:**
- [ATPM 5.01 — Mailsmith 1.1.2 (Tsai, Jan 1999)](https://www.atpm.com/5.01/page8.shtml)
- [ATPM 8.04 — Mailsmith 1.5 (Tsai, Apr 2002)](https://atpm.com/8.04/mailsmith.shtml)
- [TidBITS — Mailsmith 1.5: Lean, Mean Email Machine (Porter, Jul 2002)](https://tidbits.com/2002/07/15/mailsmith-1-5-lean-mean-email-machine/)
- [TidBITS — Distributed Filtering part 1 (Porter, Sep 2002)](https://tidbits.com/2002/09/23/mailsmith-and-distributed-filtering-part-1/)
- [TidBITS — True Confessions of a Mailsmith Switcher (Neuburg, Jul 2003)](https://tidbits.com/2003/07/28/true-confessions-of-a-mailsmith-switcher/)
- [Macworld — Mailsmith 2.0 review (Ihnatko, Oct 2003)](https://www.macworld.com/article/168357/mailsmith2.html)
- [TidBITS — Mailsmith 2.2 freeware (Fleishman, Aug 2009)](https://tidbits.com/2009/08/18/mailsmith-2-2-released-as-freeware-from-new-firm/)
- [TidBITS watchlist — 2.3.1](https://tidbits.com/watchlist/mailsmith-2-3-1/)

**Reminiscences and contemporaneous user accounts:**
- [MacObserver interview w/ Rich Siegel (2009)](https://macobserver.com/tmo/article/digging_deep_into_bare_bones_with_rich_siegel)
- Daring Fireball: [Send Now menu-script defense (2004)](https://daringfireball.net/2004/02/send_now_defense), [You Have New Mail (2003)](https://daringfireball.net/2003/06/you_have_new_mail), [Rethinking Email (2007)](https://daringfireball.net/2007/08/rethinking_email), [Mailsmith/iBook/Glider Pro (2003)](https://daringfireball.net/2003/08/mailsmith_an_ibook_and_glider_pro_walk_into_a_bar), [2.2 beta linked (2007)](https://daringfireball.net/linked/2007/04/01/mailsmith-22-beta)
- Michael Tsai's blog: [Mailsmith 2.0 & SpamSieve (2003)](https://mjtsai.com/blog/2003/06/18/), [2.0.1 (2003)](https://mjtsai.com/blog/2003/08/20/mailsmith_201_and_spamsie/), [2.1 (2004)](https://mjtsai.com/blog/2004/01/06/mailsmith_21/), [2.2 semi-public beta (2007)](https://mjtsai.com/blog/2007/04/02/mailsmith-22-semi-public-beta/), [The Best Worst E-mail Client (Aug 2026)](https://mjtsai.com/blog/2026/08/21/the-best-worst-e-mail-client/)
- [Brent Simmons — switching to Mailsmith+SpamSieve (2004)](https://inessential.com/2004/01/06/why_im_switching_to_mailsmith_and_spa.html)
- [RaisedWeird — Farewell, Old Friend (2008)](https://byrneweb.com/sunburn/blog/2008/02/farewell-old-friend/)
- Riccardo Mori: [favorite apps](https://morrick.me/archives/6487), ['Fixing' email (2012)](https://morrick.me/archives/5613), [Mail's hegemony (2014)](https://morrick.me/archives/7205)
- [Retrophisch — "Boy, do I miss Mailsmith" (2017)](https://retrophisch.micro.blog/2017/10/04/boy-do-i.html)
- [HN: BBEdit respect thread w/ Mailsmith comments (2023)](https://news.ycombinator.com/item?id=35904320); [HN comment crediting BBEdit/Mailsmith quote-wrapping for Markdown blockquotes (2012)](https://news.ycombinator.com/item?id=4701282)
- [C-Command — SpamSieve 1.0 (Sep 2002)](https://c-command.com/blog/2002/09/10/spamsieve-1-0/); [Mailsmith 2.2 (2009)](https://c-command.com/blog/2009/08/18/mailsmith-22/)
- [NeoAccess/AOL adoption (MacTech, 1997)](https://www.mactech.com/1997/04/30/md1-neoaccess-used-by-aol/); [NeoAccess 5.0 (MacTech, 1997)](https://www.mactech.com/1997/03/05/md1-neoaccess-5-0/)

**Modern-context verification:**
- [Apple Mail User Guide — rules](https://support.apple.com/guide/mail/use-rules-to-manage-emails-you-receive-mlhlp1017/mac)
- [Mimestream](https://mimestream.com/), [server-side Gmail filters help](https://mimestream.com/help/user-guide/filters), [labels](https://mimestream.com/help/user-guide/managing-labels), [releases](https://mimestream.com/releases)
- MailMate manual: [introduction](https://manual.mailmate-app.com/), [organize/smart mailboxes](https://manual.mailmate-app.com/organize), [custom key bindings](https://manual.mailmate-app.com/custom_key_bindings), [key binding selectors](https://manual.mailmate-app.com/key_binding_selectors)
- [RFC 9661 — JMAP for Sieve Scripts (Sep 2024)](https://www.rfc-editor.org/rfc/rfc9661.html); [RFC 5228 Sieve](https://www.rfc-editor.org/rfc/rfc5228.html)
- macOS 27 "Golden Gate": [Apple security note (released 2026-09-14)](https://support.apple.com/en-us/149035), [Apple OS page](https://www.apple.com/os/macos/), [MacRumors release coverage](https://www.macrumors.com/2026/09/14/apple-releases-macos-golden-gate/), [Wikipedia](https://en.wikipedia.org/wiki/MacOS_Golden_Gate) (27.0.1 out 2026-09-28; Apple-silicon-only; last macOS with full Rosetta 2)
- [Apple 32-bit app support note (Catalina cutoff context)](https://support.apple.com/en-us/HT208436); Spotlight importer status on macOS 26: [Objective-See](https://objective-see.org/blog/blog_0x81.html), [Apple macOS 26 newsroom](https://www.apple.com/kw/newsroom/2025/06/macos-tahoe-26-makes-the-mac-more-capable-productive-and-intelligent-than-ever/)
- [Macworld Pro File Q&A w/ Siegel](https://www.macworld.com/article/166152/siegel.html); [Karelia indie-marketing interview](http://www.karelia.com/mac_indie_marketing/rich-siegel-bare-bones-soft.html)
