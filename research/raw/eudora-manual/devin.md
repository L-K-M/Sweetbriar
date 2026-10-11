<!-- provenance: helper=devin model=swe-2-max started=2026-10-11T18:24:20+00:00 exit=0 brief=research/briefs/ -->

# Eudora 6.2 for Macintosh — Feature Inventory from the User Manual

Research report for the Sweetbriar email client project.
Primary source: *Eudora 6.2 for Macintosh* user manual (QUALCOMM, November 2004;
[archive.org scan](https://archive.org/details/bitsavers_qualcommeual_8147561)),
OCR text at `sources/eudora62-mac-manual.txt`. Page numbers cited below refer to the
printed manual pages shown in the OCR text. The OCR has recognition errors; page/section
cites are reliable but quoted UI strings should be treated as approximate.

Current-product facts were verified against vendor documentation on 2026-10-11 unless
noted otherwise.

## 1. TL;DR

- Eudora 6.2 was a power user's client: the manual documents roughly 20 settings panels,
  three trigger contexts for filters (incoming / outgoing / manual), up to five actions per
  filter, five priority levels, 15 labels, a dozen message statuses, multi-column sorting,
  per-message server actions for POP, scheduled send, and AppleScript — almost none of
  which survives intact in mainstream clients today.
- Its defining architectural choice was *local truth*: mail lived in mbox-format mailboxes
  on disk, the Out box was a real queue you could inspect and edit, and the application
  kept statistics, link history, and junk scores about that corpus. Modern clients assume
  server truth; Sweetbriar's Rust-core local store is the natural revival of Eudora's model.
- The most distinctive and most revivable features: **Personalities** (first-class multiple
  identities), **Redirect** (RFC-resend, still in Apple Mail but rare elsewhere), **Send
  Again**, **Timed Queue** (client-side scheduled send), **Make Filter**, filters that can
  fire **outgoing** as well as incoming mail, **Contextual Filing**, **Content Concentrator**
  (thread-digest preview), **ScamWatch** URL hover inspection, **MoodWatch** tone warnings,
  **Quick Recipients + contact-photo toolbar buttons**, and **Usage Statistics**.
- Several beloved behaviors map poorly onto modern servers: POP leave-on-server and
  skip-big-messages are POP-specific, label colors have no IMAP/JMAP standard equivalent
  (IMAP keywords exist; Gmail labels and Graph categories are per-account), and outgoing
  filters need client-side execution since server APIs (Graph, Gmail) don't offer them.
- The three-tier Sponsored/Paid/Light business model is an early freemium design worth
  noting, though advertising-supported email clients are a hard sell in 2026.
- Eudora's Mac source (C, 1,433 files, ~70 MB) is BSD-licensed via the Computer History
  Museum since 2018 — usable as a behavioral reference, not as implementation code.
- Main competitive gap to exploit: no current mainstream client offers user-definable
  message statuses, outgoing+manual filters with 19 actions, nickname/group addressing
  with recipient-history ranking, or a real, inspectable send queue.

## 2. Findings — feature inventory

Conventions for modern equivalents: **Y** = equivalent exists today, **~** = partial or
degraded equivalent, **N** = no mainstream equivalent. Products: Apple Mail on macOS 26/27
([Mail User Guide](https://support.apple.com/guide/mail/welcome/mac)), New Outlook for Mac
(legacy Outlook for Mac reaches end of support **October 2026** —
[Microsoft Support](https://support.microsoft.com/en-us/outlook/outlook-for-mac-features)),
Gmail web, and Thunderbird (current release line v15x; see
[release notes](https://www.thunderbird.net/en-US/thunderbird/157.0.1/releasenotes/)).

### 2.1 Editions, modes, and setup (manual pp. 15–30)

- **Three operating modes** (p. 23): *Sponsored* — full features, free, displays
  advertisements, includes everything in Paid except SpamWatch; *Paid* — full features, no
  ads, requires purchase and registration (phone support: Paid only, p. 23); *Light* —
  free, reduced feature set, sponsor image but no ads. Mode switching happens in-app via
  the Payment & Registration window (pp. 23–24), which also hosts registration,
  ad-customization "Profile", and update-checking.
- **Getting Started configuration** (pp. 20–22): username, mail server (POP or IMAP),
  real name, SMTP server, authentication method (Passwords, Kerberos, APOP), SSL options.
- **Import**: Eudora could import mail and settings from other Mac clients (including
  Outlook Express) during setup (p. 21).
- **Multiple users**: each macOS user account gets its own Eudora Folder, settings, and
  mail store (pp. 406, 414).
- **Help**: topic-based online help menu (p. 25); Eudora Help menu with per-topic pages.

| Feature | Mail | Outlook | Gmail | Thunderbird |
|---|---|---|---|---|
| Tiered free/paid editions | N (free w/ OS) | ~ (free vs M365 tiers) | ~ (free vs Workspace) | N (free) |
| In-client mode switching | N | N | N | N |
| Import from other clients | Y | Y | ~ (import mail/contacts) | Y |
| Per-OS-user separation | ~ (OS handles it) | ~ | N/A | ~ (profiles) |

### 2.2 Accounts and personalities (pp. 80–95, settings pp. 363–370)

- **Personalities** = named identities, each a full account definition (incoming server +
  SMTP + return address + real name + signature + stationery defaults). One *dominant*
  personality plus unlimited *alternate* personalities (pp. 80–84).
- **Dominant vs. alternate**: dominant is the default identity and default check target;
  alternates can be checked individually or all at once ("Check Mail" checks all enabled
  personalities; per-personality *Personality Extras* settings control check inclusion,
  leave-on-server days, skip-size, notifications) (pp. 363–370).
- **Personality assignment to messages**: incoming messages remember which personality
  received them; replies automatically come from that personality. A filter action
  (*Make Personality*) can reassign, which redirects future replies (p. 180). A `«Personality»`
  filter criterion exists (p. 177).
- **Personality extras** include separate checking options per personality (pp. 363–370).

Modern state: all four mainstream clients support multiple accounts; only Gmail "Send as"
(up to 99 aliases — [Gmail Help](https://support.google.com/mail/answer/22370)) and
Outlook shared-mailbox Send As come close to identity *sets*, and Gmail is **ending
Send-as for third-party addresses January 2027** (same source). None treats identities as
first-class objects with per-identity rules, signatures, stationery, and checking options.

| Feature | Mail | Outlook | Gmail | Thunderbird |
|---|---|---|---|---|
| Multiple accounts | Y | Y | ~ (delegated/linked) | Y |
| Multiple identities per account | ~ (alias addresses) | ~ (Send As, shared mbx) | ~ (Send as; dying for 3rd-party) | Y (identities) |
| Per-identity signature/stationery/defaults | ~ | ~ | ~ | ~ |
| Auto pick identity on reply | Y (account match) | ~ | ~ (send-as match) | Y |
| Identity as filter criterion/action | N | N | N | ~ (via headers) |

### 2.3 Checking mail — POP and IMAP (pp. 96–126, settings pp. 312–322)

- **POP options** (pp. 96–100): Authentication (Passwords/Kerberos/APOP); *Overlap
  commands* (pipelining); *Check for mail every N minutes*; *Don't auto-check when using
  battery*; *Send on check*; **Leave on server for N days** (blank = forever); **Delete
  from server when emptied from trash**; **Skip messages over N KB** — oversized messages
  arrive as headers + first few lines + a truncation notice (p. 100).
- **Per-message server actions** (pp. 100, 141): a partial/skipped message's summary
  carries a server-status icon; the user sets **Fetch from Server**, **Delete from
  Server**, or **Fetch & Delete** — resolved on the next mail check. The open message's
  icon bar has matching Fetch/Trash icons.
- **Check Mail Specially** (Option + Check Mail, p. 100): a one-off *Mail Transfer
  Options* dialog giving special instructions for a single check (e.g., delete all
  retrieved messages).
- **IMAP** (pp. 101–126): Eudora retrieves *minimal* headers by default for IMAP accounts
  (p. 120) and fetches full content on open. IMAP mailboxes/folders are created and managed
  on the server through the same mailbox UI; the Trash commands distinguish *local Trash*,
  *remote IMAP Trash*, *all Trash*, and *expunge deleted items in current IMAP mailbox*
  (p. 290–291).
- **Mail checking UX**: check schedule per personality; progress dialog with task list
  (tasks can be individually canceled); "mail check" and "send queued" can be combined
  or separate; password prompting and *Save Password* / *Forget Password* commands —
  Forget Password exists both globally and per-personality to prevent unauthorized checks
  while away from the machine (p. 295–296).
- **New-mail attention** (Getting Attention settings, p. 354): alert dialog, sound, open
  mailbox on arrival, dock icon notification, plus filter-driven attention (Speak, Play
  Sound, Open, Notify User/In report).

Modern state: IMAP IDLE/push replaces polling everywhere; POP still supported in Mail,
Thunderbird, and (newly, 2025) New Outlook. Leave-on-server and delete-on-empty persist
in Mail/Thunderbird POP settings; **per-message fetch/delete decisions and skip-over-size
are gone** — IMAP's structure makes the "partial fetch then decide" workflow unnecessary
(though RFC 8970 `PREVIEW` gives a server-assisted approximation). Forget Password
survives only as Keychain/account sign-out.

| Feature | Mail | Outlook | Gmail | Thunderbird |
|---|---|---|---|---|
| POP + IMAP | Y | Y (POP added 2025) | IMAP/POP bridging | Y |
| Leave on server N days | Y | ~ | N/A | Y |
| Skip messages > N KB, fetch later | N | N | N | ~ (fetch headers only mode) |
| Per-message fetch/delete actions | N | N | N | N |
| Check schedule / battery awareness | N (push) | N | N | ~ |
| Forget password per identity | ~ | N | N | ~ |

### 2.4 Mailboxes and mail storage (pp. 134–165)

- **Fixed mailboxes**: In, Out, Trash (and Junk with SpamWatch, pp. 184–186, 329). Out is a
  real, user-visible mailbox holding Unsent/Sendable/Queued/Timed Queue messages — the
  send queue is inspectable, editable, and printable (pp. 134–140).
- **User mailboxes**: arbitrarily many, organized into nested *mailbox folders*
  (folders-of-mailboxes), plus *mailbox aliases* (shortcuts to mailboxes stored elsewhere)
  and *external mailboxes* (pointers to mailboxes outside the Eudora Folder) (pp. 143–151).
- **Mailbox drawer** (p. 151): collapsible drawer on mailbox windows listing all mailboxes
  for one-click transfer.
- **Transfer menu**: hierarchical mailbox menu for manual filing; transfers also by
  drag-and-drop or Option-click column-group selection.
- **Contextual Filing** (p. 170): highlight any word in a message, control-click, and a
  submenu offers *"Transfer to [mailbox name]"* for every mailbox whose name matches the
  selected text — filing-by-word, zero typing. Sits in the same contextual menu as Web
  Words.
- **Trash semantics**: messages moved to Trash keep status; Empty Trash on Quit option;
  IMAP-aware trash variants (§2.3). Deleted-marked messages show strike-through styling.
- **Storage format**: Eudora mailboxes are text (mbox-family) files in the Eudora Folder —
  user-backupable, scriptable, and portable (pp. 406, 414). Address book auto-backups
  noted (p. ~261).

| Feature | Mail | Outlook | Gmail | Thunderbird |
|---|---|---|---|---|
| Local folders/mailboxes | Y (On My Mac) | ~ (local folders limited) | N | Y |
| Nested folders | Y | Y | ~ (label nesting) | Y |
| Inspectable send queue | ~ (Outbox transient) | ~ | N | ~ |
| Aliases/shortcuts to mailboxes | N | ~ (favorites) | ~ (labels) | ~ (virtual folders) |
| mbox storage | Y | N | N | Y |

### 2.5 The message list — columns, statuses, sorting (pp. 136–142)

- **Columns** (user-toggleable, reorderable): Status, Priority, Attachments, Label,
  **Junk Score**, Who (sender or recipient, context-aware), Date, Size, Mailbox, **Mood**
  (MoodWatch chilies, p. 140), Subject, Server Status (p. 136–141).
- **Status values/icons** (pp. 137, 60): Unread (•), Read, Unsent, Sendable (down arrow),
  Unsendable, Queued, **Timed Queue** (clock icon), Sent, Replied, Forwarded, Redirected,
  plus Recovered/Server states. Status is a first-class, user-and-filter-settable property.
- **Priorities**: 5 levels (Highest/High/Normal/Low/Lowest) shown with arrow glyphs;
  settable by sender, by user, or by filter (Raise/Lower one notch is an action).
- **Labels**: 15 label slots, each with name + color; labels 1–7 mirrored Finder label
  colors on classic Mac OS, 8–15 configured in Eudora Labels settings (pp. 180, 347).
- **Sorting** (pp. 138–139): click column header to sort; **Shift-click adds secondary
  sort keys** (multi-column sorting); sort order is *sticky* per mailbox. Columns available
  for sort include Mood and Junk Score.
- **Group Subjects** (p. 139): pseudo-threading that groups messages sharing a normalized
  subject under the top message — Eudora's answer to conversation view, without
  References:-based threading.
- **Selection**: Shift-click ranges, Command-click noncontiguous, **Option-click selects
  the whole group in grouped-by-column views** (p. 300+).

| Feature | Mail | Outlook | Gmail | Thunderbird |
|---|---|---|---|---|
| Custom columns | ~ (fixed set) | Y | N | ~ (fixed set) |
| User-settable message status | ~ (flags) | ~ (flags/categories) | ~ (stars/inbox) | Y (tags, status) |
| Labels w/ colors | ~ (flags only) | Y (categories) | Y | Y (tags; 5 built-in + custom) |
| Multi-key sorting | N | ~ | N | N |
| Priority levels | ~ | Y (importance) | ~ (importance marker) | Y |
| Subject grouping/threading | Y (conversations) | Y | Y | Y (threaded + references) |

### 2.6 Reading messages (pp. 106–126, 187–188, 336)

- **Preview pane** with adjustable height; **Content Concentrator** (pp. 187–188, settings
  p. 333): Option-click a message to group the whole thread (by subject or by sender) and
  view them *concatenated* in the preview with redundant headers and quoted text trimmed;
  per-mailbox concentration levels — None / Compact (headers trimmed to To/From/Subject/Cc)
  / Terse (only From: + body).
- **Styled text & HTML**: Eudora rendered styled text and HTML "as best it can"; no HTML
  frames (p. 118). *Open in Browser* command (File menu) hands HTML mail to the browser.
  HTML graphics controlled by an *Automatically download HTML graphics* setting plus a
  per-message **Get HTML Graphics** icon-bar button; downloaded images go to a managed
  Cache Folder (p. 118). **This is remote-content blocking done right — a privacy control
  most clients got wrong for years.**
- **Blah Blah Blah**: icon-bar toggle reveals all hidden headers (p. 110–111); filters can
  match `«Any Header»` including hidden ones.
- **URLs**: active links in messages; Option-click/Command-click opens; poorly formatted
  links openable via File > Open Selection (p. 116). **ScamWatch (URL Protection)**
  (pp. 116–117): hovering a link shows the true destination in a help tag; deceptive URLs
  (e.g., raw-IP hosts, mismatched display/real host) get an explicit warning tag and a
  confirmation dialog before opening.
- **Speak** (Edit menu, p. 119): PlainTalk speech reads open messages or a selected range
  in the list ("Next message" between items); quotes spoken in higher pitch wrapped in
  "quote…unquote".
- **Attachments**: detach to a chosen Attachments Folder; open via double-click or
  Command-click; Option-drag copies an attachment out; incoming attachments can be
  auto-sorted by filter (Move Attachments action, p. 182); attachment column shows count.
- **Save As** to text file; Print / Print One Copy with auto headers+footers (p. 120).
- **Message window icon bar** (p. 110): Fetch/Delete-from-server icons (POP), Trash, Get
  HTML Graphics, Blah Blah, plug-in buttons.

| Feature | Mail | Outlook | Gmail | Thunderbird |
|---|---|---|---|---|
| Remote image blocking per message | Y | Y | Y | Y |
| True-URL-on-hover (anti-phish) | ~ (tooltip shows URL) | ~ (safelinks rewrite) | ~ | ~ |
| Read-aloud | ~ (system speech) | ~ (Read Aloud) | N | N |
| Full-header toggle | Y (raw source) | ~ (headers dialog) | Y (show original) | Y |
| Thread-digest preview (Concentrator) | N | N | ~ (conversation view) | N |

### 2.7 Composing (pp. 31–57)

- **Composition window** (p. 32): title bar, **icon bar**, optional formatting toolbar,
  header fields, message body. The icon bar is the per-message override panel — every
  outgoing setting can be flipped for this message only:
  Priority popup · Signature popup · Attachment Type (encoding) popup · Text Styles
  Toolbar toggle · **Quoted-Printable Encoding** · **Include Macintosh Information** ·
  **Word Wrap** (~76 cols) · **Keep Copy** (retain copy in Out) · **Return Receipt** ·
  plug-in buttons · **MoodWatch indicator** · Send/Queue button (label follows the
  Immediate Send setting) (pp. 32–34).
- **Addressing**: To/Cc/Bcc/Reply-To/Fcc fields; nickname auto-completion and optional
  immediate expansion; *Make Address Book Entry* from a message or selected address;
  Reply-To honored; Fcc ("folder carbon copy") files the sent message into a chosen mailbox
  (pp. 34–39, 236–240).
- **Styled text** (pp. 41–45): formatting toolbar with font/size/style/color/alignment/
  indent/lists; styled text sent as HTML or MIME-enriched; Clear Formatting; per-message
  font controls obey the fixed-width print font setting for plain text.
- **Insert objects** (p. 45–50): attach files; embed images/sounds/movies inline
  (PureVoice audio plug-in); horizontal divider lines; active URLs (auto-linked, angle
  brackets recommended); drag & drop supported throughout.
- **Attachment encodings** (pp. 51–52): per-message popup — **MIME (Base64)**,
  **BinHex**, **AppleDouble**, **AppleSingle**, **Uuencode** — plus the "Include
  Macintosh Information" toggle for resource forks/type codes. (Today: MIME/Base64 only;
  the others are dead formats.)
- **Signatures** (pp. 70–73): standalone named documents edited in the Signatures window;
  a *Standard* plus an *Alternate* signature plus arbitrary others; per-personality default;
  per-message override via the icon-bar Signature popup (selectable or None); option to
  omit signature on replies/forwards.
- **Stationery** (pp. 74–77): any outgoing message can be saved as a reusable stationery
  file — headers, body, formatting, *and the full icon-bar state* are captured. New
  Message With [stationery] creates a pre-filled message; **Reply With [stationery]**
  powers auto-reply filters (p. 183); Option-double-click a stationery file to edit the
  template itself; a default stationery can be set in Composing Mail settings.
- **Spell check** (pp. 54–55): check on send or on demand, user dictionaries, multiple
  dictionaries.
- **MoodWatch** (pp. 58–60, settings p. 360): spell-checker-style scan for "offensive
  language" rated **ice cube → 1–3 chili peppers**; real-time as you type; warns at a
  configurable threshold before send; can also scan incoming and even all stored mail;
  offending phrases highlighted in color. Icon sits in the compose icon bar and in the
  mailbox Mood column.
- **Return Receipt** request (p. 40); **priority** (p. 39).

| Feature | Mail | Outlook | Gmail | Thunderbird |
|---|---|---|---|---|
| Per-message send-option overrides | ~ | ~ (options ribbon) | ~ | ~ |
| Multiple signatures | Y | Y | Y | ~ (per-identity) |
| Stationery/templates | N (removed) | ~ (My Templates add-in) | Y (Templates) | N (removed) |
| Fcc to chosen folder | N | ~ (rules) | N | ~ (copies setting) |
| Return receipt | N | Y (read receipt) | ~ (Workspace only) | Y |
| Offensive-language warning | N | ~ (MailTips differ) | N | N |
| Encoding choice | N | N | N | N |

### 2.8 Sending (pp. 58–69, 295)

- **Send vs Queue** (p. 58): Immediate Send setting flips the button; queued messages sit
  in Out as *Queued* until *Send Queued Messages* / next scheduled send.
- **Timed Queue** (p. 59–60): queue a message for a specific date/time (clock icon
  status); on quit, Eudora warns about queued messages or timed messages due within 12
  hours (p. 25). Scheduled send requires Eudora running at the send time — it is a
  client-side queue.
- **Send on check**; *Send Queued Messages* command; per-account **SMTP relay** option —
  route all outgoing through one SMTP account (p. 62); SSL for SMTP/POP/IMAP (p. 380+).
- **Progress**: task progress window with per-task cancel.
- **Send Again** (p. 131): re-opens a stored message (sent or received) as an editable
  outgoing message, stripping delivery artifacts — "treat any stored message as
  stationery". Also the recovery path for bounced mail.
- **Reply variants** (pp. 127–131): Reply, Reply to All, **Reply to selected text**
  (quotes only the selection), **Reply With [stationery]** and **Reply Quoting Selection
  With [stationery]** — replies instantiated from a template.
- **Forward vs Redirect** (pp. 129–130): Forward quotes the original under your name;
  **Redirect** resends the message so the recipient sees the original sender's address
  (replies go to the original sender) — no quoted wrapper. *Redirect & signatures*: your
  signature is attached only if the message was originally yours (matched against the
  "me" nickname).
- **Turbo Redirect** (p. 130): Option + choose recipient from Quick Recipient submenu →
  redirects, queues, *and deletes the original* in one gesture (Shift+Option variant keeps
  the original); can be made the default Redirect To behavior via Miscellaneous settings.
- **Message > Attach** submenu also hosts plug-in commands.
- **Mail loop safety**: Eudora refuses to auto-redirect/reply/forward messages bearing
  `Precedence: bulk` or `Precedence: list` headers (p. 183).

| Feature | Mail | Outlook | Gmail | Thunderbird |
|---|---|---|---|---|
| Scheduled send | Y (Send Later) | Y (Send later/Schedule send) | Y (Schedule send, ≤100) | ~ (Send Later addon/local) |
| Inspectable editable queue | ~ | ~ (Drafts) | ~ (Scheduled folder) | ~ |
| Redirect (resend w/ orig. sender) | **Y (Message > Redirect)** | N | N | **~ (Message > Redirect since TB91, edit-as-new style) + Mail Redirect add-on** |
| Send Again | N | ~ (Resend this message, new in 2025) | ~ (edit + resend) | ~ (Edit as New) |
| Reply With template | N | ~ (Quick Steps/Template) | ~ (templates manual) | N |
| Reply quoting selection only | ~ | N | N | N |
| Bulk/list auto-reply suppression | Y (server) | Y (server) | Y (server) | ~ |

Apple Mail genuinely still has Redirect on Tahoe/Golden Gate ([Mail User
Guide](https://support.apple.com/guide/mail/reply-to-forward-or-redirect-emails-mlhlp1010/16.0/mac/27)).
Thunderbird added a Redirect command in v91 that behaves like Edit-As-New with the
original Reply-To, per [bug 12916](https://bugzilla.mozilla.org/show_bug.cgi?id=12916)
and the [Mail Redirect add-on](https://mailredirect.sourceforge.io/index.html) docs.

### 2.9 Filters (pp. 170–188)

The heart of Eudora's power-user reputation.

- **Trigger contexts** (p. 175): each filter independently enables **Incoming**, **Outgoing**
  (runs on the copy of mail you send), and/or **Manual** (runs when the user chooses Special
  > Filter Messages). Outgoing filtering is essentially extinct today.
- **Criteria** (pp. 177–179): two terms joined by a conjunction (ignore / **and** / **or** /
  **unless**). Header operand popup: `To:`, `Cc:`, `From:`, `Subject:`, `Reply-To:`,
  `«Any Recipient»`, `«Any Header»`, `«Junk Score»`, `«Body»`, `«Personality»` — or type
  any arbitrary header name (e.g., `X-Priority`).
- **Match operators**: contains / doesn't contain · is / is not · starts with / ends with ·
  **appears / doesn't appear** (header-existence test) · **intersects / doesn't intersect
  nickname** · **intersects / doesn't intersect address book** (address-book-membership
  matching — a whitelist mechanism) · **is greater than / is less than** (Junk Score).
- **Actions** (up to 5 per filter, pp. 180–182): None · **Make Status** · **Make Priority**
  (set, or Raise/Lower a notch) · **Make Label** · **Make Personality** · **Make Subject**
  (`&` = old subject) · **Play Sound** · **Speak** · **Open** (mailbox and/or message) ·
  **Print** · **Notify User** (normal attention and/or in a filter report) · **Forward To** ·
  **Redirect To** · **Reply With** (stationery → auto-reply/vacation) · **Server Options**
  (set POP Fetch/Delete flags) · **Copy To** · **Transfer To** · **Move Attachments**
  (per-filter attachment folder) · **Skip Rest**.
- **Execution semantics** (pp. 175–176): filters run top-to-bottom; each message obeys the
  first matching filter (Transfer/Skip Rest end processing; Transfer implies Skip Rest).
  Filter order is user-reorderable by dragging. A filter may use the same action twice if
  it doesn't move the original (two Copy Tos OK; two Transfer Tos not).
- **Make Filter** (p. 171): select messages → Eudora prefills a filter from their common
  sender/recipient — the fastest path from example to rule.
- **Filter list UX**: named automatically from first criterion; icons show transfer (→),
  copy (⇒), skip-rest (•), label color tinting, and a **dinosaur icon for filters unused 30
  days**; a *Last used* field per filter; **filter report** on demand (p. 181).
- **Loop protection**: no auto forward/redirect/reply on Precedence: bulk/list (p. 183).

Modern state: server-side rules dominate (Gmail filters, Outlook rules, iCloud mail rules
are thin). Apple Mail rules run only on incoming mail, locally; Gmail filters are
server-side but limited to ~10 actions and no arbitrary-header matching beyond from/to/
subject/keywords; Thunderbird filters support custom headers and run on
incoming/manual/junk-classification/archival but **not outgoing**; Sieve exists server-side
but few consumer clients expose it. **No mainstream client filters outgoing mail.**

| Capability | Mail | Outlook | Gmail | Thunderbird |
|---|---|---|---|---|
| Incoming filters | Y | Y | Y | Y |
| Outgoing filters | N | N | N | N |
| Manual-run filters | Y (Apply Rules) | ~ | N (apply on create only) | Y |
| Arbitrary header matching | ~ (any header exists) | ~ | N | Y |
| Address-book-membership test | ~ (VIP/group-ish) | ~ | ~ (contact group via search) | ~ |
| 15+ action types incl. open/speak/print | ~ (8-ish) | ~ (10-ish) | ~ (8) | ~ (12) |
| Filter-from-example (Make Filter) | ~ (from selected) | ~ (create rule from msg) | Y (filter messages like these) | Y |
| Unused-filter detection | N | N | N | N |

### 2.10 Finding and searching (pp. 189–218)

- **Find Messages** (p. 189–215): a criteria-row search dialog — multiple criteria rows
  (Any/All matching), header-field popups like filters, match modes (contains, is,
  etc.), **Match case**, scope to selected mailboxes / all mailboxes / open mailbox;
  results in a mailbox-style window that behaves like a mailbox (sortable, operable);
  **Save search results to a mailbox**.
- **Regular expressions** (p. 215): full regex matching as a find option — effectively
  gone from consumer clients.
- **Toolbar Search field** (pp. 213–215): one field, popup chooses **Search Web** /
  **Search Eudora** / **Search All** / **Search Mailbox** / **Search Mailfolder**; remembers
  recent searches. This is a unified local+web search box — decades ahead of its time.
- **Web Words** (pp. 217–218): highlight any word(s) in a message → control-click →
  "Search Web for X" launches a browser search.
- **Find within message/mailbox** (Edit > Find): standard incremental find; Find Again.
- **Link History** (p. 226): window listing URLs previously visited from email — searchable
  history of clicked links.
- **IMAP-aware searching**: searches local store; IMAP messages fetched minimally are
  searched on downloaded content (p. 120–121).

| Feature | Mail | Outlook | Gmail | Thunderbird |
|---|---|---|---|---|
| Multi-criteria search builder | ~ (tokens) | Y | ~ (search options + operators) | Y |
| Regex | N | N | N | N |
| Saved searches → mailbox | Y (Smart Mailboxes) | Y (Search Folders, classic only) | ~ (filters+labels) | Y (Saved Search folders) |
| Search scope control | Y | Y | ~ | Y |
| Web search from client | N | ~ (Bing in search) | Y (it *is* web) | N |
| Link history | N | N | N | N |

### 2.11 Junk mail — SpamWatch (pp. 184–186, settings pp. 329–331)

- **Paid-mode-only** feature (Sponsored lacks it; Light lacks it). Statistical scoring
  produces a **Junk Score 0–100** shown in its own column; threshold slider (default 50)
  routes ≥ threshold to the **Junk mailbox** (a protected system mailbox).
- **Whitelist**: senders in the Address Book are never junked; "Add Not Junk senders to
  address book" option; **Not Junk** (Command-J) retrains and rescues; **Junk** command
  trains and files.
- **Hygiene**: don't mark Junk mailbox unread; auto-remove junk older than N days
  (default 30) with warning; junk stats feed the Statistics window.
- **Plug-in scoring**: `«Junk Score»` is also a filter criterion — users can build their own
  junk pipelines on the score (e.g., 30–50 → "Maybe Junk" mailbox).
- Internals: a *SpamWatch* plug-in plus "Junk mail database" stored in the Eudora Folder;
  the engine was a Qualcomm-customized Bayesian-style scorer (manual pp. 184–186, 329–331;
  internals not documented — implementation detail (unverified)).

| Feature | Mail | Outlook | Gmail | Thunderbird |
|---|---|---|---|---|
| Trainable junk | ~ (limited) | Y (server) | Y (server) | Y (Bayesian, trainable) |
| Visible numeric spam score | N | N | N | ~ (junk status) |
| User threshold control | N | ~ (levels) | N | ~ |
| Address-book whitelist | ~ (VIP-ish) | Y (safe senders) | ~ (contacts) | Y |
| Junk as filter input | ~ | ~ | ~ | ~ |

### 2.12 Address book, nicknames, recipients (pp. 232–265)

- **Nicknames**: short names expanding to full addresses — typed anywhere an address goes;
  auto-completion and optional immediate expansion; a special **"me" nickname** listing
  *your own* addresses (used by Redirect signature logic, p. 130).
- **Recipient List / Quick Recipients**: dynamic most-recently-used recipient list driving
  the *New Message To / Forward To / Redirect To / Turbo Redirect To* submenus (pp. 127–130,
  256). **Recipient history** list tracks who you write to (p. 256).
- **Groups**: nickname entries made of other nicknames; nestable (p. ~240).
- **Contact photos**: attach a picture to an entry; **toolbar buttons with the contact's
  photo create a pre-addressed new message**; **drag a message onto a contact's photo to
  Forward (or Option-drag to Redirect) it to that person** (pp. 249–254). One of Eudora's
  most beloved touches.
- **Make Address Book Entry** from selected text/message; entries carry real name,
  address(es), nickname, notes, photo.
- **vCard** import/export; **multiple address books** (Address Book within Address Book,
  p. ~243).
- **Palm conduit** (pp. 258–263): HotSync integration with bidirectional / desktop-to-Palm /
  Palm-to-desktop / disabled modes and a "slow sync" force-full-sync option — dead
  technology today but the *sync-direction control* concept survives in CardDAV clients.
- **Directory Services** (pp. 266–269): separate from the address book — **Ph**, **LDAP**,
  **Finger** query support with a server list (globe icon = discovered servers); used for
  expanding/looking up addresses at composition time.

| Feature | Mail | Outlook | Gmail | Thunderbird |
|---|---|---|---|---|
| Nickname expansion | ~ (autocomplete) | ~ | ~ | ~ |
| Named groups | ~ (groups in Contacts) | Y | Y | Y (lists) |
| Frequent-recipient menu | N | ~ | N | N |
| Photo quick-compose buttons | N | N | N | N |
| Drag-to-contact forwarding | N | N | N | N |
| LDAP/Ph directories | ~ (LDAP) | Y (GAL) | ~ (Directory) | Y (LDAP) |
| vCard | Y | Y | Y | Y |

### 2.13 Windows and UI management (pp. 216–229, 404–406)

- **Normal vs tabbed windows** (pp. 216–229): seven tool windows — Mailboxes, Signatures,
  Stationery, Personalities, Address Book, Filters, Directory Services — can live as
  standalone windows or be **grouped into tabbed windows**; tabs detach, regroup, reorder;
  restore-to-default layout. Tabbed utility palettes years before tabbed browsers were
  common.
- **Toolbar** (pp. 404–406): fully customizable main toolbar — Command-click a blank
  area to open the Button Creation dialog; Command-drag rearranges; buttons for menu
  commands, **contact photos**, web-search field, **function-key mappings**; per-user
  layout.
- **Dock icon**: unread count badge and bounce attention options.
- **Window menu**: lists open windows plus the seven tool windows and Statistics.
- **Modifiers** (pp. 300–309): rich keyboard/mouse grammar — Spacebar scrolls/opens next
  message; arrow keys navigate messages (with configurable modifiers); Tab moves between
  header fields; Option/Shift/Command modify click semantics everywhere (selection,
  sorting, Turbo Redirect, stationery edit, attachment copy, group selection).
- **Moving Around settings** (p. ~358): what happens after reading/deleting — do nothing /
  open next / open next unread / only-if-unread / open next unread *with same subject* —
  plus key-exchange customization for menu commands.

| Feature | Mail | Outlook | Gmail | Thunderbird |
|---|---|---|---|---|
| Customizable toolbar | Y | Y | N | Y |
| Tabbed tool windows | N | N | N | ~ (tabs for messages) |
| Function-key command mapping | ~ (system) | N | N | ~ |
| Configurable next-message behavior | ~ | ~ (auto-advance lab) | Y (auto-advance) | ~ |

### 2.14 Plug-ins — EMSAPI (pp. 408–411)

- **EMSAPI** (Extended Messaging Services API): third-party extension boundary. Two plug-in
  kinds — *message plug-ins* (operate on messages/text; appear on the Message Plug-ins
  submenu, icon bar, Attach submenu, or Special menu, or run invisibly) and *resource
  plug-ins* (preferences/behavior resources). Installed by dropping into Eudora Stuff /
  enabling via Finder Get Info (pp. 408–411).
- **Bundled examples**: Rot13 Text, UPPERlower Text, **PureVoice** (voice messages),
  **ESP** (the sharing protocol itself is a plug-in), plus third-party virus-scan and
  compression plug-ins advertised. EMSAPI SDK was distributed by Qualcomm.
- **Plug-in Settings** submenu for plug-ins with preferences; **About Message Plug-ins**
  dialog lists all installed.

Modern state: client extension APIs today — Thunderbird MailExtensions, Outlook web
add-ins, Gmail add-ons (Workspace); **Apple Mail has no public plug-in API** (Mail bundles
were removed years ago; MailKit on macOS exposes content blockers/share extensions, not
mail processing). A Rust-core app can do better: a stable FFI/WASM extension surface.

### 2.15 AppleScript and automation (pp. 297–298)

- **Scripts menu** (p. 297): create a `Scripts` folder inside Eudora Stuff → its
  AppleScripts appear as menu items; toolbar buttons can point at them; Open Scripts
  Folder command. The manual's front matter references "Eudora's AppleScript Dictionary"
  (p. iv); bundled sample scripts: Dictionary lookup, List Settings,
  Open Sender's Home Page, Photo Phind, Thesaurus.
- Scriptable objects include mailboxes, messages, filters, and settings — Eudora was one
  of the most scriptable Mac apps of its era.

Modern state: Apple Mail remains scriptable (robust dictionary); New Outlook for Mac
does not expose the legacy app's AppleScript surface (unverified — Scriptability appears
reduced vs. legacy Outlook); Gmail has Apps Script instead; Thunderbird has no
AppleScript but has MailExtension APIs. macOS Shortcuts is the modern surface.

### 2.16 Settings panels (pp. 310–375)

Scrollable icon list ("Getting Started, Checking Mail, Sending Mail, Composing Mail, Junk
Mail, Attachments, Replying, Fonts & Display, Styled Text, Moving Around, Toolbar,
Getting Attention, Extra Warnings, Hosts, Personalities, Personality Extras, MoodWatch,
Content Concentrator, Eudora Labels, Spell Checking, Statistics, SSL, Misc,
Sponsored-mode Ads settings, ESP settings, Plug-in Settings"). Highlights:

- **Extra Warnings** (p. ~356): granular confirmations — delete warnings, queue warnings,
  empty-trash, quit-with-queued-mail. Data-safety-by-prompting.
- **Miscellaneous** (p. 357): Turbo-redirect default, empty trash on quit, "saved
  messages" behaviors.
- **Hosts** (p. ~350): per-account host config incl. SMTP relay.
- **Esoteric/x-settings**: hidden `x-eudora-setting` options tweakable via Settings >
  (URI-style settings editing) — power-user escape hatch (index refs; (unverified)
  mechanics).
- **Statistics settings** (p. 377): graph colors, series (current/previous/average),
  reset.

### 2.17 Usage Statistics (pp. 227–228, settings p. 377)

- Statistics window (Window menu): Day/Week/Month/Year graphs of **messages received,
  sent, read, replied, forwarded, redirected, attachments**, plus **SpamWatch junk
  accuracy** stats (accuracy improves with Junk/Not Junk training). Printable report;
  color/graph-type settings; reset button. Personal email telemetry before "personal
  analytics" was a category.

### 2.18 ESP — Eudora Sharing Protocol (pp. 376–404)

- **ESP Groups** (pp. 376–404): *share groups* of people who keep a common folder of
  files synchronized **entirely through email** — each member designates a local shared
  folder; add/delete/update events propagate as ESP control messages handled by the ESP
  plug-in; cross-platform (Mac + Windows Eudora); advanced setup, global ESP settings
  (update frequency), an ESP Groups window, and an ESP mailbox/folder. A poor-man's
  Dropbox/Docs-workspace built on mail. Fully extinct.

### 2.19 Miscellaneous machinery

- **UUCP** support as a mail transport (p. ~417); SMTP/POP/IMAP/UUCP listed in mail
  transport reference.
- **MIME**: full multipart support; AppleDouble/AppleSingle for Mac files (p. 51–52, 418).
- **Word Services** integration (system-wide spelling/lookup services on classic Mac OS).
- **Printing** everywhere incl. Print One Copy and statistics reports.
- **Passwords**: Save/Forget/Change Password commands; per-personality forget.
- **Sponsored-mode ad window**: persistent ad view in Sponsored mode.
- **Mail check on launch**, automatic checks, overlap/pipelining, APOP/Kerberos, SSL/TLS.

## 3. Options compared

### 3.1 Feature density vs. modern clients (inventory summary)

| Area | Eudora 6.2 (2004) | Best modern equivalent | Gap |
|---|---|---|---|
| Message statuses | 10+ first-class statuses incl. send-pipeline states | TB tags/flags | No client exposes queue lifecycle in the list |
| Filter contexts | incoming+outgoing+manual | incoming-only (all four) | outgoing filtering extinct |
| Filter actions | 19 incl. speak/print/open/personality/server ops | ~8–12 | notify-class actions gone |
| Sorting | multi-key, sticky, 12 columns | single-key | multi-key sorting extinct |
| Threading | Group Subjects + Content Concentrator | true conversations | concentrate/digest view unique |
| Scheduling | client-side Timed Queue | server-side schedule send | client queue more inspectable |
| Identities | personalities w/ per-identity everything | accounts + weak aliases | identity objects extinct |
| Anti-phish | ScamWatch true-URL hover + warn | SafeLinks rewriting (Outlook) | hover transparency rare |
| Addressing | nicknames+groups+recipient history+photo buttons | autocomplete + contacts | recipient-history UX extinct |
| Automation | AppleScript + EMSAPI + filters | Shortcuts/MailExtensions/add-ins | deeper scriptability available |
| Business model | free-ads / paid / light freemium | free+subscription | ads model dead |

### 3.2 Where each modern client is *ahead* of Eudora 6.2

- **Conversation threading** (true References/Message-ID threading everywhere vs.
  Eudora's subject grouping).
- **Server-side everything** (rules run when client is off; Gmail/Outlook schedule send
  works with client closed; Eudora's queue died with the app).
- **Snooze/remind** (Gmail/Outlook/Apple Mail follow-up) — Eudora had no concept.
- **Search** (server indexes; Gmail operators; Apple's categories) — Eudora's Find was
  fast but local-substring/regex only.
- **Mobile/synced state** — Eudora was single-machine by design.
- **Undo send** (Gmail/Apple Mail grace period) — Eudora's queue gave a manual equivalent.
- **OAuth2/modern auth, S/MIME & OpenPGP, DKIM/DMARC visibility, TLS by default** —
  Eudora 6.2 predates these expectations (SSL existed; certs were manual).

### 3.3 Revival approaches for signature features (design space)

| Feature | Client-only (Rust core) | Server-synced (IMAP keywords / JMAP / Graph) | Hybrid |
|---|---|---|---|
| Labels w/ colors | store in DB only | map to IMAP keywords/JMAP keywords; Graph categories (per-account) | **best: local canonical + sync where supported** |
| User statuses | DB only | custom keywords | hybrid |
| Outgoing filters | **only option — run on Out pipeline before send** | n/a (no server API) | client-only by necessity |
| Timed queue | client must run | Graph `sendMail` immediate only; Gmail API has no scheduled-send API (unverified — web UI only); JMAP `EmailSubmission` has no hold | **client-only queue; degrade gracefully** |
| Priorities | X-Priority/Importance on send | header on stored copy | trivial |
| Personalities | identity records in core | per-backend mapping (aliases, Send-As, shared mbx) | hybrid |
| Nicknames/Quick Recipients | local, ranked | contacts sync via CardDAV/Graph contacts | hybrid |

## 4. Recommendations for Sweetbriar — reviving the beloved 20+

Ranked roughly by (distinctiveness × feasibility). Each proposal assumes the shared Rust
core (`sb-core`: sync, mbox+SQLite store, `mail-parser`, full-text index), native macOS
UI (AppKit/SwiftUI), and backends for IMAP/JMAP/Graph/Gmail-API.

1. **Personalities → first-class "Identities".**
   An Identity = {name, return address, signature, stationery, default folder map, send
   route (SMTP submission / Graph sendMail / Gmail send / JMAP EmailSubmission), per-identity
   checking options, nicknames}. Store in core; resolve reply-identity from the receiving
   account (exactly Eudora's Make Personality semantics). *Trade-off:* backends disagree
   on what an identity is (Graph shared mailboxes, Gmail send-as — which dies for 3rd-party
   Jan 2027); Sweetbriar must model identity as *client-side routing + From:* choice, not
   server objects. *Counter-argument:* aliases are a niche power feature; multi-account is
   enough for most users. Still, it's the single most-requested Eudora-ism and cheap to
   layer on the account model.

2. **Redirect (+ Turbo Redirect).**
   Implement RFC 5322 §3.6.6 resend (`Resent-From/To/Date/Message-ID`), preserving the
   original body byte-for-byte — the Eudora/Apple-Mail semantic. Add Quick-Recipient
   turbo variant and the filter `Redirect To` action. *Trade-off:* DMARC — redirecting to
   some domains fails when the original sender's domain enforces strict alignment and the
   resending host isn't aligned; must surface bounce risk and fall back to
   forward-as-attachment. *Counter-argument:* forward-as-attachment covers 90% of cases;
   but support-desk workflows genuinely need resend semantics — Apple still ships it for
   a reason.

3. **Send Again / "edit as new" + Stationery.**
   One command converts any stored message (sent, bounced, received) into an editable
   draft, stripping Received/Resent headers. Stationery = draft templates preserving
   headers, body, and per-message send options — implement as a `template` flag in the
   drafts store with a picker in compose. *Trade-off:* must rebuild MIME correctly
   (re-use `mail-builder`); attachments already local → trivial. *Counter-argument:* none
   serious; it's strictly useful and ~free given a real local store.

4. **Outgoing + incoming + manual filters with Eudora's action set.**
   Rust-core filter engine operating on the Out-pipeline *before* submission (the only
   place outgoing filtering can live) and on incoming sync events; manual runs on
   selection. Reproduce the 19-action set minus dead ones (Play Sound→system sound,
   Speak→`AVSpeechSynthesizer`, Open→window commands, Print→NSPrintOperation, Server
   Options→deleted). Keep **Make Filter** (create rule from selected messages) and the
   **dinosaur/last-used** metadata. Persist filters in a portable format (JSON; export to
   Sieve where the server supports it — JMAP Sieve RFC 9661). *Trade-off:* running rules
   on send adds latency and a new failure mode; ordering semantics (first-match,
   transfer-implies-skip-rest) must be documented. *Counter-argument:* server-side rules
   (Gmail/Exchange) already exist and run when the client is off — duplicating them
   locally risks rule double-execution. Mitigation: never auto-migrate server rules;
   label client rules as "on this Mac".

5. **First-class message statuses.**
   A real `status` enum in the DB (unread/read/unsent/sendable/queued/timed/sent/replied/
   forwarded/redirected + user-visible), derivable from IMAP flags + local send state.
   *Trade-off:* mapping to IMAP `\Seen`/custom keywords and JMAP keywords is lossy —
   keep local canonical value, sync best-effort. *Counter-argument:* most users only need
   read/unread/flagged; extra states add UI complexity. But the *send pipeline states* are
   what make the queue inspectable — core to data-safety storytelling.

6. **Labels with colors (15 named slots → unlimited named labels).**
   Local canonical label set; per-backend projection: JMAP keywords / IMAP keywords /
   Graph categories / Gmail labels. Color stored locally (no standard color channel
   exists anywhere). *Trade-off:* colors and names won't survive cross-device sync except
   via Sweetbriar's own sync later. *Counter-argument:* duplicating Gmail labels &
   Exchange categories into one concept is a mapping headache (label≠folder semantics);
   worth it — unified tags are table stakes for a power client.

7. **Multi-column sorting + sticky per-mailbox sort + the full column set.**
   AppKit `NSTableView` with custom sort descriptors; persist per-mailbox view state in
   the core. Include a computed **Junk Score** and **Identity** column. *Counter-argument:*
   trivial effort, high daily-use value.

8. **Group Subjects & Content Concentrator → "Digest view".**
   Two separate revivals: (a) subject-cluster grouping as an alternative thread mode where
   References threading is unavailable (POP, broken clients); (b) Concentrator → a
   "read thread as one document" preview that strips quoted duplication — implement via
   the parser's quote-boundary detection. *Trade-off:* quote detection is heuristic and
   will misfire on weird clients. *Counter-argument:* modern clients use conversation
   views; the *concentrated digest* is still unique and cheap.

9. **ScamWatch → link inspector.**
   On hover: resolve display-URL vs. `href` host, flag numeric-IP/punycode/userinfo-in-URL/
   lookalike domains in a tooltip; on click of a flagged URL, confirmation sheet showing
   the decoded target. Runs entirely in the renderer (no network). *Counter-argument:*
   SafeLinks-style rewriting already "protects" Outlook users — but it *obscures* targets;
   transparency is the Eudora way and costs little.

10. **MoodWatch → tone advisory.**
    On-device heuristic (lexicon) or a small local model via Core ML / Apple's NaturalLanguage
    framework — flag aggressive/profane phrasing while composing, show a discrete
    1–3-pepper indicator, warn at threshold. Must be off-by-default-friendly and never
    send text off-device. *Counter-argument:* spell-check-style profanity lists are crude;
    but Outlook's MailTips (reply-all storms, external recipients) prove the appetite —
    keep it opt-in.

11. **Timed Queue.**
    The Out mailbox is a real core object; `send_at` timestamp on messages; a persistent
    scheduler in the core sends when due (and on next launch if missed). On quit, warn
    about pending/due-soon items (Eudora did, p. 25). Show clock-icon status. *Trade-off:*
    client must be running — unlike Gmail's server-side schedule send; document this
    honestly and add "suggested send times" for the common cases. *Counter-argument:*
    server-side scheduling is strictly better *where available* — but IMAP/SMTP has no such
    API and Graph/Gmail scheduled-send APIs are limited/absent (Gmail API exposes no
    scheduled-send endpoint as of 2026 — (unverified)); a client queue is the only
    cross-backend answer.

12. **Quick Recipients + recipient history + photo toolbar buttons.**
    Ranked recipient model in core (frequency × recency per identity); menu submenus for
    New/Forward/Redirect To; avatar buttons on the toolbar via Contacts.framework photos;
    **drag message → avatar = forward, Option-drag = redirect**. *Counter-argument:*
    pure delight feature; low cost, high Eudora-authenticity.

13. **Nicknames & groups in addressing.**
    Local nickname book (name→addresses, nestable), inline expansion with visible
    feedback, "me" nickname for self-detection (drives redirect-signature logic and
    reply-to-self handling). Sync via CardDAV/Graph contacts with nickname field.
    *Counter-argument:* contacts databases already exist; but nickname-as-typed-alias is
    faster than contact autocomplete for power users.

14. **Statistics window → activity dashboard.**
    The local store makes this trivial SQL: volume by day/hour, sent/received/replied/
    attachment counts, junk accuracy, per-identity breakdown. Print/PDF export. A
    signature "Eudora-feel" feature almost no one has.

15. **Scripts menu → automation surface.**
    Ship a real AppleScript/JXA dictionary (mailboxes, messages, filters, send) plus a
    Shortcuts.app intents extension; a `~/Library/Application Support/Sweetbriar/Scripts`
    folder populates a Scripts menu exactly like Eudora's. *Trade-off:* designing a stable
    scripting dictionary is ongoing API-maintenance cost. *Counter-argument:* Shortcuts
    covers casual automation; AppleScript still matters to the Eudora diaspora — this is
    a community feature that costs little relative to its signaling value.

16. **Search: Eudora Find + toolbar field, upgraded.**
    Reproduce the criteria-rows dialog (any/all, header ops incl. arbitrary headers and
    body, match case, scope by mailboxes, save-results-as-smart-mailbox) over a Tantivy
    index; keep Eudora's dual-purpose toolbar field (mail search / web search). Optional
    regex mode — no one else ships it. *Counter-argument:* modern users prefer one big
    search box; offer both (box with operator syntax + builder UI).

17. **Remote-content controls done Eudora's way.**
    Default-block remote images; per-message "load images" button + per-sender allowlist;
    cache folder with LRU eviction (exactly Eudora's model, p. 118). Mainstream has caught
    up — implement it cleanly and visibly.

18. **Skip-big-messages / header-only fetch → per-message fetch control.**
    For slow links: sync envelope+preview only (IMAP `RFC822.SIZE`+`BODY.PEEK` /
    RFC 8970 `PREVIEW` where advertised; JMAP `preview` property is native), flag message
    as "partial", and expose Fetch Full / Delete Without Fetch actions — direct
    modernization of Eudora's server-status column. *Trade-off:* adds a partial-message
    state to the model. *Counter-argument:* ubiquitous bandwidth makes it niche; keep as
    an optional "bandwidth saver" mode.

19. **Extra Warnings + data-safety prompts.**
    Configurable confirmations (empty trash, quit-with-queued-mail, delete-nonempty-mailbox),
    empty-trash-on-quit option — trivially cheap, strongly on-brand for "never lose mail".

20. **Make Filter + Filter report + unused-filter hygiene.**
    Covered in #4 but worth calling out as its own UX: create-rule-from-selection, a
    per-filter `last_fired` timestamp, dinosaur-icon staleness, and an optional filter
    run-log panel ("which filters touched this message") — huge debuggability win no
    client offers.

21. **Blah Blah Blah → one-keystroke full headers.**
    Toggle raw-header display in-place (not a modal), plus `«Any Header»` filter matching
    over the full header block. Near-free.

22. **ESP → do NOT revive (explicit non-recommendation).**
    File-sync-over-email is obsolete (iCloud Drive, Syncthing, shared JMAP/Graph folders
    where server-side sharing exists). Note it as the boundary of nostalgia: Eudora's
    share-groups solved a real problem that infrastructure now solves. *Counter-argument
    to my own recommendation:* a "shared mailbox workspace" via JMAP sharing (RFC 9670)
    could echo ESP for shared *mail* — defer to a later milestone.

23. **Speak messages.**
    `AVSpeechSynthesizer` for message and list-range reading incl. quote-pitch shift —
    accessibility feature with Eudora precedent. Cheap.

24. **Freemium tiers — the modes, reimagined.**
    Eudora's Sponsored/Paid/Light shows the team thought hard about pricing UX; for
    Sweetbriar: free tier (core features, local accounts), paid tier (advanced
    filters/personalities/automation, cross-device sync later). Skip the ads — an ad
    window in an email client is unviable in 2026. *Counter-argument:* gating
    filter/personality features (Eudora gated plenty to Paid) risks the same frustration
    Light users felt; better to gate on sync/multi-device than on power features.

### Honorable mentions (documented, not recommended for v1)

- **AppleSingle/BinHex/Uuencode encodings, UUCP, Ph/Finger, Palm conduit, Rot13
  plug-in, mailbox aliases, Fcc field, Word Services** — historically interesting,
  no modern purpose (CardDAV/LDAP covers directories; keep Fcc only as a "copy to
  folder on send" option).
- **Sponsored-mode ad window** — museum piece.

## 5. Risks and open questions

- **DMARC breaks Redirect.** Strict-sender domains (p=reject) resent from our servers will
  fail authentication. Open: measure how often resend fails; consider SRS-style rewriting
  for the envelope sender on redirect (unverified feasibility per-backend).
- **Scheduled send requires a running client** on IMAP/SMTP; JMAP `EmailSubmission` has no
  future-send property (unverified — check current drafts); Gmail API lacks scheduled
  send. Mitigation: wake-on-launch catch-up + clear "sends while Sweetbriar is running" UX.
- **Keyword/label interoperability.** IMAP keywords are inconsistently supported
  (Gmail exposes labels as folders, not keywords; some servers cap keyword count).
  Labels-as-folders vs. labels-as-tags is a real semantic fork; needs a per-backend
  capability table in the core.
- **Outgoing-filter latency/failure.** A filter that rewrites or moves a just-sent message
  must not break submission guarantees; design for atomic "submit-then-filter-local-copy".
- **Regex search** over a tokenized index needs a literal-prefix fast path or it will be
  slow on multi-GB stores.
- **Threading**: Group Subjects is only a fallback — JMAP `Thread` objects / IMAP
  `THREAD=REFERENCES` give real threads where supported.
- **Identity routing on Microsoft 365**: Send As/Send on Behalf require tenant-granted
  permissions ([Microsoft Graph
  docs](https://learn.microsoft.com/en-us/graph/outlook-send-mail-from-other-user));
  Sweetbriar must surface server-side denial cleanly rather than silently falling back.
- **AppleScript dictionary maintenance** is a long tail; decide the object model early to
  avoid churn.
- **OCR reliability**: page numbers in this report are manual print pages; a few OCR
  artifacts (e.g., "pase64", "eniry") mean minor UI labels may be imperfectly transcribed.
- **Eudora source availability**: the Mac codebase (C, ~70 MB, 1,433 files) is BSD-licensed
  at the [Computer History
  Museum](https://computerhistory.org/blog/the-eudora-email-client-source-code/) — worth
  consulting for exact filter/sort semantics; the Windows codebase (8,651 files) is
  separate. Pandora Mail (Windows shareware, [pandoramail.com](https://pandoramail.com/))
  and HERMES/Eudoramail are living evidence of demand for a faithful Eudora successor.

## 6. Sources

Primary:
- *Eudora 6.2 for Macintosh User Manual*, QUALCOMM 2004 —
  [archive.org scan](https://archive.org/details/bitsavers_qualcommeual_8147561) (OCR:
  `sources/eudora62-mac-manual.txt`)

Modern equivalents (consulted 2026-10-11):
- Apple Mail User Guide (macOS 26/27) — [welcome](https://support.apple.com/guide/mail/welcome/mac),
  [redirect](https://support.apple.com/guide/mail/reply-to-forward-or-redirect-emails-mlhlp1010/16.0/mac/27),
  [categories](https://support.apple.com/en-gb/guide/mail/mlhlp1190/mac),
  [revisit/follow-up](https://support.apple.com/guide/mail/mark-emails-to-revisit-later-mlhlp1052/mac)
- Outlook for Mac — [feature matrix & legacy EOL](https://support.microsoft.com/en-us/outlook/outlook-for-mac-features),
  [release notes](https://support.microsoft.com/en-us/office/outlook-for-mac-release-notes-for-beta-channel-builds-d6347358-5613-433e-a49e-a9a0e8e0462a)
- Gmail Help — [Send as / alias changes (Jan 2027)](https://support.google.com/mail/answer/22370),
  [Schedule send](https://support.google.com/mail/answer/9214606?co=GENIE.Platform%3DDesktop&hl=en&oco=1),
  [labels/filters/snooze](https://support.google.com/mail/answer/9259770?hl=en)
- Thunderbird — [v157.0.1 release notes](https://www.thunderbird.net/en-US/thunderbird/157.0.1/releasenotes/),
  [v158 release notes](https://www.thunderbird.net/en-US/thunderbird/158.0/releasenotes/),
  [redirect bug 12916](https://bugzilla.mozilla.org/show_bug.cgi?id=12916),
  [Mail Redirect add-on](https://mailredirect.sourceforge.io/index.html),
  [Simple Mail Redirection](https://addons.thunderbird.net/EN-US/thunderbird/addon/simple-mail-redirection/)
- Microsoft Graph — [sendMail](https://learn.microsoft.com/en-us/graph/api/user-sendmail?view=graph-rest-1.0),
  [send from another user](https://learn.microsoft.com/en-us/graph/outlook-send-mail-from-other-user)
- JMAP ecosystem — [Fastmail "Ten years of JMAP"](https://www.fastmail.com/blog/ten-years-of-jmap/),
  [Cyrus JMAP docs](https://www.cyrusimap.org/stable/imap/developer/jmap.html),
  [Stalwart JMAP conformance](https://stalw.art/blog/road-to-stalwart-1-0/),
  [jmap-client crate](https://crates.io/crates/jmap-client),
  [Stalwart mail server](https://github.com/stalwartlabs/mail-server/)
- Eudora legacy — [CHM Eudora source release](https://computerhistory.org/blog/the-eudora-email-client-source-code/),
  [CHM press release](https://computerhistory.org/press-releases/eudora-source-code/),
  [The Register coverage](https://www.theregister.com/software/2018/05/23/finally-historic-eudora-email-code-goes-open-source/389170),
  [Pandora Mail](https://pandoramail.com/), [HERMES Eudoramail](https://hermes.cx/)
- Mail categories reporting — [MacRumors on macOS 15.4/iOS 18 categories](https://www.macrumors.com/2025/02/21/revamped-mail-app-mac-ipad/),
  [Macworld how-to](https://www.macworld.com/article/2585948/how-to-ios-18-macos-15-change-mail-categories-list-view.html)
