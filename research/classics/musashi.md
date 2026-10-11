# Musashi (Sonosoft, Japan, 1997–2008)

Synthesis of [raw/musashi/codex.md](../raw/musashi/codex.md),
[raw/musashi-artifacts/codex.md](../raw/musashi-artifacts/codex.md) and
[raw/musashi/devin.md](../raw/musashi/devin.md). Primary sources: the
original distribution archives from
[macintoshgarden.org](http://macintoshgarden.org/apps/musashi) (1.2.4J,
3.0.6, 3.4.1 classic EN and JP, 3.4.1 Mac OS X, 3.5). Codex extracted them
with a StuffIt 5 decoder it wrote for the purpose (all checksums verified)
and read the English and Japanese HTML manuals, release notes and
resource forks. The archive contents are copyrighted and not redistributed.
Japanese quotations are translations.

## In one paragraph

Musashi was a compact, fast POP3/SMTP client for classic Mac OS and Mac OS
X, written almost single-handedly by **Mamoru Misono (弥園 護)** of Sono
Software / Sonosoft in Osaka. It was shareware via Kagi and Vector (¥3,000,
later US$25–33) from 1997, became freeware with 3.5 in 2008, and was never
open-sourced. Misono's stated goal (Vector, 1998, translated) was "a
responsive, compact style" while other applications grew heavy. Its
signature ideas:

- **multiple local users** with private mailboxes on one shared Mac
- a **Virtual Message Viewer** that gathers new arrivals or search results
  across folders and shows where each was filed
- **serious Japanese and multilingual text handling**
- a spatial, icon-based folder workspace with customizable "Modules"

Macworld (Engst, 2002) found it "fast and easy to use" but thin on search
and filtering. A reader replied that it was "the easiest multiple-user
environment to set up".

## Timeline

| Date | Event |
|---|---|
| ≤1997 | Early Japanese-only versions; 1.2.3 adds quoted-printable |
| 1997-10 | 2.0.5 circulating: 68K/PPC, English and Japanese |
| 1998 | 2.2.x in English, French, Italian and Japanese; Vector review of J-2.1 |
| 1998-12 / 1999 | 3.0: hierarchical folders, multilingual fonts, Remote Mail |
| 2000-10 → 2001-01 | "Musashi-V" rewrite beta: labels, named saved searches, thread sorting, mail-merge "repeated send", unlimited folder depth. **Never shipped** |
| 2001-12 | 3.3/3.3.1: SMTP AUTH (CRAM-MD5, then LOGIN/PLAIN), POP-before-SMTP |
| 2002 | 3.4 for Mac OS X and Classic; 3.4.1 |
| 2006 | 3.4.2 |
| 2008 | 3.5 freeware (package dated 03-14, announced 04-24) |

## Feature inventory (from the manuals and the full menu/dialog inventory)

- **Users:** single or multi-user mode with a manager password; add,
  rename or delete users; per-user home directories; switch user without
  quitting.
  - 1.2.4 had **permission-controlled shared mailboxes**: others may add
    into and/or move out of the owner's mailbox, but not rename or delete
    it.
  - 3.4.1 replaced this with **Local Forward**, which delivers selected
    mail to another local user with no server involved.
- **Accounts:** per-account POP/SMTP, Reply-To, default signature, output
  charset, attachment encoding; send-only accounts; explicit ports.
  Long-pressing Check Mail or New Message picks an account.
- **Receiving:** a process window; account checks queue behind each other
  (no parallel downloads).
  - New arrivals open a **Virtual Message Viewer** with a folder-name
    column and every normal action.
  - **Remote Mail:** list the POP server's messages (subject, sender, date,
    size), mark them to retrieve and/or delete, then execute.
- **Folders:** large icons, small icons or a list; free icon placement;
  three grid spacings; custom folder icons; up to three levels. A folder
  holds either messages or subfolders, not both. Inbox, Outbox and Trash
  are fixed at the root.
- **Reading:**
  - Space pages forward and then advances to the next message.
  - **Control-1…9 scrolls at nine speeds**; Shift reverses direction.
  - Headers: none, some or all.
  - Three quote-depth colours, URL highlighting, per-language fonts.
- **Message operations:** move, copy, trash, highlight (seven colours),
  Get Info (**edit the subject and status**), add the sender to the
  address book, apply filters, remove attachments (with a warning), mark
  read or unread.
- **Filters:**
  - named and reorderable
  - **at most two conditions** with AND/OR and per-condition case
    sensitivity
  - fields: subject, to, cc, from, reply-to, headers, body, whole message,
    account
  - predicates: contains, doesn't contain, begins with, ends with
  - actions: move and highlight
  - triggers: incoming or outgoing; the first match wins
  - manual application of selected rules to stored mail
- **Search:**
  - status shortcuts (Unread, Read, Unsent, Sent)
  - two condition regions, dates, highlight
  - scope: all, the current viewer, or a folder
  - **"Call filter"** loads a rule's conditions into search
  - **searching while a result viewer is in front refines the results**
- **Composer:**
  - tabs for recipients, account/signature and encodings
  - Reply, Reply All, Reply to Sender
  - **Direct Forward** keeps the original sender and date
  - Send Now / Send Later queue
  - **attachments as icons inline in the text**: movable, deletable like a
    character
  - Option-click shows the caret position and character count
- **Templates:** plain-text files with optional To, Cc, Bcc, Subject,
  Account, Signature (and later Reply-To) headers, then the body. A
  documented, portable format. The reply introduction uses `%N %S %D`.
- **Japanese text:** JIS output, inline input conversion, per-language
  fonts (avoiding half-width-kana mojibake); Cyrillic, Turkish and Greek
  converters (3.2).
- **Import/export plug-ins:** Eudora mail and address book, UNIX aliases.
  Drag messages to the Finder as text.
- **Modules:** a published resource format for icons, colours and sounds,
  with a third-party module ecosystem on Vector.
- **Storage:** 2.x used mbox-like per-mailbox files. 3.x used per-user
  single-file Incoming/Outgoing Archives plus a Message Index; one 2002
  user who switched clients cited it as a backup worry.

## Limits and warnings

- Release notes record fixed mail-loss bugs:
  - mail lost after a forced quit
  - mail lost when cancelling a user switch
  - deletes bypassing Trash
  - multiline Bcc exposed
- A preference **automatically answers "Yes" to warning dialogs after a
  timer.** That is a data-safety anti-pattern.
- No IMAP, SSL or threading; two-condition filters; three folder levels;
  HTML only via the browser. The English documentation was thin and partly
  wrong (it swapped POP3 and SMTP; its authentication section was stale).

## What survives today

- **Apple Mail** Smart Mailboxes gather mail across folders.
- **Gmail** multiple-inbox sections.
- **Thunderbird** runs filters after sending, so outgoing filters are not
  extinct.
- Templates are common (Spark, Mimestream, Gmail).
- **Not established anywhere:** a reading session that stays stable while
  rules file the mail, a folder-context column in an arrival view,
  searching within results, and portable plain-text templates that carry
  the account and signature.

## What Sweetbriar should take

1. **Reading sessions.** A stable, ordered list of messages (new arrivals,
   unread across accounts, any search) that doesn't reshuffle while you
   read. It shows each message's actual folder or labels and account.
   Refresh is explicit, and Read, Done and File are separate actions. It's
   persisted so a restart restores your place. The same idea as Engst's
   Eudora "attention queue".
2. **Search inside results** and "load a rule into search" (and the
   reverse): one predicate language for both.
3. **Multilingual correctness as a test corpus:** a decoding override with
   preview that never rewrites originals, and CJK fixtures from day one
   (mixed headers, wrong charsets, half-width kana).
4. **Portable templates:** human-readable files with a versioned header
   schema (account, identity, signature, recipients, subject,
   placeholders), with Musashi import as a nicety.
5. **Inline attachment placement** in the composer (an attachment as a
   movable object in the text) and an explicit **Direct Forward**.
6. **Personality:** optional folder symbols and colours, message-state
   badges, colour plus text (never colour alone).
7. **Local profiles** only if a real workflow needs them. macOS user
   accounts cover most of this today. **Local transfer and export** of raw
   mail without a server is the useful remnant.
8. **Avoid:** auto-confirming dialogs, monolithic per-user archives,
   undocumented mailbox formats.
