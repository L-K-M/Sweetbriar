# Eudora (Steve Dorner / Qualcomm, 1988–2006)

Synthesis of [raw/eudora-manual/devin.md](../raw/eudora-manual/devin.md)
(the *Eudora 6.2 for Macintosh* manual,
[archive.org](https://archive.org/details/bitsavers_qualcommeual_8147561)),
[raw/eudora-source/codex.md](../raw/eudora-source/codex.md) (an audit of the
Mac source released by the Computer History Museum under BSD, mirrored at
[pmachapman/eudora-mac](https://github.com/pmachapman/eudora-mac); derived
tables in [codex-artifacts/audit](../raw/eudora-source/codex-artifacts/audit/)),
[raw/eudora/codex.md](../raw/eudora/codex.md) (history and reception) and the
partial [raw/eudora/mimo.md](../raw/eudora/mimo.md).

## In one paragraph

Steve Dorner started Eudora in 1988 at the University of Illinois. It was
named after Eudora Welty's story "Why I Live at the P.O.". Qualcomm
licensed it in 1991 and sold it from 1993 as free Light, paid Pro, and
later an ad-supported Sponsored mode. By 1996 it claimed 18 million users.
Qualcomm ended it on 2006-10-11 (final releases: Mac 6.2.4, Windows
7.1.0.9). It was profitable, but didn't fit a wireless company. The
Thunderbird-based OSE successor ("Penelope") was panned for not feeling
like Eudora. Jeff Beckley: "Users come to depend on those subtle
interactions, and, when they aren't there, it just feels different". It is
now unsupported. The source became BSD in 2018. In 2026 people still run
it, and the HERMES project keeps a revival going.

Eudora's power came from **composable operations**:

- personalities
- filters that run on incoming, outgoing and manual triggers
- nicknames
- stationery
- Redirect and Send Again
- server dispositions per message
- an inspectable Out queue
- keyboard modifiers everywhere

## Feature inventory (highlights)

**Storage and state.**
- Local mbox-family mailboxes; In, Out, Trash and Junk.
- **Out is a real, editable queue** with lifecycle statuses: Unsent,
  Sendable, Queued, Timed Queue, Sent, Replied, Forwarded, Redirected,
  Recovered.
- Five priorities; 15 named, coloured labels.
- **Editable summaries:** Make Subject and the "Pencil" to edit a received
  message's subject or body.
- Per-message POP server actions (Fetch, Delete, Fetch & Delete).
- "Skip messages over N KB" for partial download.

**Personalities.**
- Full identities: server, SMTP, address, signature, stationery, checking
  options.
- Replies come from the personality that received the message.
- Personality is both a filter criterion and an action.
- Drag a message onto a personality to reply as it.

**Filters.**
- Incoming, outgoing and manual triggers.
- Two terms with and/or/unless.
- Any header including arbitrary names, body, Junk Score, Personality.
- Operators include **intersects nickname / address book**: 15 operators
  in all.
- About 19 actions:
  - status, priority, label, personality, subject
  - sound, speak, open, print, notify
  - forward, redirect, Reply With stationery
  - server options, copy, transfer, move attachments, skip rest
- **Make Filter** from selected messages; first match wins.
- A **dinosaur icon for filters unused for 30 days**; a filter report.
- Refuses auto-replies to `Precedence: bulk/list`.

**Message list.**
- Toggleable columns, including Junk Score, Mood and Server Status.
- **Shift-click multi-key sort**, sticky per mailbox.
- Group Subjects.
- **Option-click "select similar"**: same correspondent, subject and so on.

**Reading.**
- **Content Concentrator:** read a thread or sender as one document, with
  repeated headers and quotes trimmed (Compact and Terse levels).
- Multiple-message preview and reply.
- **ScamWatch:** hover shows the real URL; deceptive links get a warning
  and a confirmation.
- Blah Blah Blah (full headers); Speak with speech rules; Link History.
- Remote images off by default, with a "Get HTML Graphics" button.

**Composing.**
- A per-message icon bar overrides signature, encoding, wrap, keep copy and
  return receipt.
- **Stationery** captures headers, body and icon-bar state.
  **Reply With stationery** drives auto-replies.
- **MoodWatch:** chili-pepper tone warnings with an optional cool-off
  delay.
- Fcc to any mailbox, or file replies beside their originals.
- Glossary expansion in the body.

**Sending.**
- Send vs Queue; **Timed Queue**; send on check.
- **Send Again:** any stored message becomes an editable new one.
- **Redirect:** `Resent-*`, keeps the original author.
- **Turbo Redirect:** redirect, queue and trash the original in one
  gesture.

**Addressing.**
- Nicknames, nestable, including a "me" nickname.
- **Quick Recipients**, a most-recently-used recipient menu.
- **Contact-photo toolbar buttons.** Drag a message onto a photo to
  forward it; Option-drag to redirect.
- Directory services: Ph, LDAP, Finger.

**Search.**
- Criteria-row Find with **regex**, match case and scope.
- Results act like a mailbox and can be saved.
- **Live search windows** pick up new messages (preference 395).
- Toolbar field searches mail or the web; Web Words. Windows 7 added X1
  indexed search and saved searches.

**Junk.**
- Numeric score (0–100) with a threshold, an address-book whitelist and
  trainable Junk/Not Junk.
- `«Junk Score»` as a filter input; age-based junk retention.

**Contextual Filing.** Select a word; the context menu offers mailboxes
whose names match it.

**Extensibility and automation.**
- EMSAPI message and translator plug-ins.
- AppleScript dictionary and a Scripts menu folder.
- **`x-eudora-setting:` links:** 1,033 documented setting IDs (394
  preferences and 639 strings). Each opens an editor with the current and
  default value and help, then asks for confirmation. Hidden power, made
  inspectable.

**Usage Statistics:** sent, received, read, replied, attachments and junk
accuracy by day, week, month and year. **Extra Warnings:** granular
confirmations. Customizable toolbar, tabbed tool windows.

## What users loved, and what they didn't

- **Loved:**
  - efficiency over appearance
  - relationships you could act on: sender, label and identity were
    handles for selecting, filing and automating
  - "fast as hell" (HN recollection)
  - Engst's Eudora 6.2 workflow: searching 33 mailboxes for unread or
    labelled mail in about a second, used as a live attention queue
    spanning his filing system
  - fear of migrating decades of mail: people keep Eudora for that reason
    alone
- **Disliked:**
  - weak HTML rendering and a dated UI (Macworld, 2004)
  - obscure preferences
  - limited filter logic (two terms)
  - poor IMAP (Engst)
  - Windows Eudora's lack of charset support
  - the Sponsored and Light modes' restrictions
  - migration pain: Engst's 2011 move of about 1M messages saw two tools
    disagree (970,283 vs 849,206 messages) on attachments, labels and
    status

## Internals worth learning from (source audit)

- **The TOC is part index, part irreplaceable metadata.** Summaries hold
  offsets, status, labels, personality, junk and mood scores, and edited
  subjects. Rebuilding from mailbox text recovers the messages but **loses
  local edits, labels, status and scheduled times** when no usable TOC
  exists. It can also resurrect deleted mail or old draft revisions.
- **Compaction** rewrites mailboxes non-transactionally. Its error path
  can deliberately discard work in progress to protect the mailbox.
- **Autosave existed but defaulted to off** (interval 0).
- **Received MIME was rewritten:** attachments were extracted into a
  folder and replaced by notes. So the store is **not** a byte-for-byte
  archive of the original messages.
- **What was fast:**
  - summary-first lists, with bodies loaded on demand and purgeable
  - only visible rows invalidated; optional deferred re-sorting
  - cached compiled regexes and nickname hashes for filters
  - pooled IMAP connections (default maximum 5)
  - POP pipelining, SMTP recipient pipelining
  - cooperative background threads for checking and sending, separate
    from foreground filtering

## What survives today

- Gmail has message-derived filters and templates.
- Thunderbird has saved searches, outgoing filters and a redirect add-on.
- Spark and Mimestream have templates.
- Apple Mail has Redirect, Send Again and rules.

The gap is the **integration**: select similar → preview a rule → file →
reply with an identity and template → pin an attention view, all with
complete local copies.

Rare or absent everywhere:

- user-visible send-pipeline statuses
- per-recipient queue control
- multi-key sort
- the Concentrator digest view
- hover transparency for links (vs Outlook's opaque SafeLinks rewriting)
- photo quick-buttons
- a statistics window
- inspectable advanced settings

## What Sweetbriar should take

1. **Identities as first-class objects** with reply routing and identity
   criteria and actions in rules.
2. **An Out Box as an inspectable queue** with statuses, a timed queue,
   send now vs queue, and an honest "possibly sent" state.
3. **Rules:** Make Filter, phases (incoming, pre-send, post-send, manual),
   address-book and nickname set membership, a preview, execution records,
   a stale-rule hint (the dinosaur) and a run log.
4. **Select similar, search results as actionable mailboxes, live saved
   searches** feeding the "reading session" (see [musashi.md](musashi.md)).
5. **Redirect, Send Again and stationery/templates** with identity,
   options and owned attachments. Redirect must be honest about DMARC and
   fall back to forward-as-attachment.
6. **A Concentrator-style digest view** built from reversible display
   ranges (quote, signature and forward boundaries), with Expand All always
   available.
7. **ScamWatch-style link inspection;** MoodWatch/BossWatch as optional
   pre-send checks.
8. **Display overlays** (local subject or sender edits, notes) that never
   rewrite originals.
9. **Inspectable advanced settings:** typed, searchable, with defaults and
   help; importable via `sweetbriar:` setting links with confirmation.
10. **Statistics, Link History and Extra Warnings** as later delights.
11. **An Eudora importer** that snapshots both forks, attachments, spools,
    settings, nicknames and stationery, keeps the original archive, and
    produces a reconciliation manifest.
12. **Don't revive:** ESP (file sync over mail), ad-supported modes,
    Mac-specific encodings, rewriting received MIME.
