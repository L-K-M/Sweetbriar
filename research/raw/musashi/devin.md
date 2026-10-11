<!-- provenance: helper=devin model=swe-2-max started=2026-10-11T18:42:09+00:00 exit=0 brief=research/briefs/ -->

# Musashi — a Japanese classic-Mac email client, and what Sweetbriar can take from it

*Research report, 2026-10-11. All Japanese quotations are the author's translations
and are marked as such. "Fact" claims cite inline sources; user quotes are marked
as quotes/recollections; anything unverifiable is marked (unverified).*

---

## 1. TL;DR

- **Musashi** (MUSASHI in early Japanese versions) was a small, fast, multi-user
  POP3/SMTP email client for classic Mac OS, written almost single-handedly by
  **Mamoru Misono (弥園 護)** of Sono Software / SONOSOFT in Osaka, shipped as
  Kagi-registered shareware (≈$25–$33 / ¥3,000) from 1997, and made **freeware
  with version 3.5** (dated 2008-03-14, announced on the renewed site
  2008-04-24) ([SONOSOFT site, 2008](https://web.archive.org/web/2008/http://www.sonosoft.com/)).
- Its signature ideas: **true multi-user accounts with private mailboxes on one
  shared Mac** (predating macOS fast user switching), **local mailboxes + Local
  Forward between users**, spatial/optional list folder views, per-encoding
  fonts, plug-in import/export converters, and a **viewer with a focus mark,
  Space-to-page-down reading, and Ctrl-1–9 variable-speed auto-scroll**.
- Contemporary reviewers loved its footprint ("runs on 2MB RAM"), stability and
  clean UI; dislikes centered on a thin/poorly translated English manual,
  hierarchical-address-book friction, sequential account polling, and 68k
  attachment decode bugs ([ATPM review](https://atpm.com/4.07/page15.shtml),
  [Mac Orchard user reviews, archived 2002-01-29](https://web.archive.org/web/20020129171458/http://www.macorchard.com/cgi-bin/cgiwrap/themaco/frameit/email.html)).
- Musashi 2.x used **mbox-like per-mailbox files**; 3.x moved to per-user
  single-file **Incoming/Outgoing Archives + Message Index** — a consolidation a
  2002 migrant specifically cited as a fragility/backup worry ([Kimux diary](http://kimux.org/mac/musashi2-viewer.shtml)).
- An ambitious rewrite, **"Musashi-V"** (public beta 2000-10 → 2001-01), promised
  user-defined **labels**, **named saved searches** decoupled from filters,
  **thread sorting**, **mail-merge "repeated send"**, **unlimited folder depth**
  and a return of folder sharing — but **no stable V release is verified**; the
  beta URL was reused for 3.4 betas in 2002, and none of the V features appear in
  the final 3.5 build ([Musashi-V beta page](https://web.archive.org/web/20010126222600/http://www.sonosoft.com:80/jp/musashibeta.html), [Impress digest](https://internet.watch.impress.co.jp/www/digest/backno/0010/31.htm), author's binary inspection of Musashi 3.5).
- Sweetbriar should revive: named/saved searches surfaced as smart folders,
  per-user private profiles with local forward, remote-mail-style selective
  download over JMAP/IMAP, per-charset display fonts, pluggable import/export
  converters, and Musashi's keyboard-centric triage — while replacing 3.x's
  monolithic archives with a crash-safe, message-per-file + SQLite index design.

---

## 2. Findings

### 2.1 History

**Maker.** Musashi was developed by **Mamoru Misono** under the name
**Sono Software** (later SONOSOFT / ソノソフト株式会社). The current corporate
page lists Misono as 代表取締役 (president), founding dated **July 1997** and
incorporation **2008-11-20**, in Osaka, doing "application development,
maintenance and operation" and "system development" — i.e. today it is a
contract-development shop, and Musashi no longer appears on the site
([Sonosoft company page](https://sonosoft.com/?page_id=32), fetched 2026-10-11).
Fan documentation describes Misono as Sonosoft's "唯一のプログラマ" — sole
programmer — at the time ([Rj's Musashic Page](http://rj-chaos.sakura.ne.jp/chaos/Rj_MUSA.html), translation).

The earliest English site (1998) introduces Sono Software as "the site which
sends software, developed by Mamoru Misono, to the whole world through the
internet" ([sonosoft.com, 1998 capture](https://web.archive.org/web/1998/http://www.sonosoft.com/)).

**Name.** Officially unexplained. The community FAQ says the 2.x About box drew
two swords — suggesting Miyamoto Musashi — and repeats a rumored backronym
"Multi-User-Simple-Action-Sincerity-Human-Interface"; "the truth is known only
to the author, Misono." Even the About dialog's example date (1967-05-20) is
described as a mystery ([Musashi FAQ §1-07](https://web.archive.org/web/2006/http://serennz.cool.ne.jp/sb/sp/mac/musa_1.html), translation).

**Platforms.** 68K and PowerPC builds for System 7–Mac OS 9, then a
Carbon build for **Mac OS X** shipped with the 3.4 line in 2002 (betas from
2002-03-15, labeled "MacOSX and Classic"; final 3.4.1 on 2002-06-14)
([sonosoft.com news](https://web.archive.org/web/20021202144725/http://www.sonosoft.com:80/index.html),
[Impress digest](https://internet.watch.impress.co.jp/www/digest/backno/0204/17.htm)).
Macintosh Garden documents System-7-class compatibility and notes use under
SheepShaver/Basilisk II ([Macintosh Garden](https://macintoshgarden.org/apps/musashi)).
Six localizations existed at the end: Japanese, English, French, German,
Spanish, Italian — each in OS X and Classic PPC/68K builds ([2008 download section](https://web.archive.org/web/2008/http://www.sonosoft.com/)).

**Version timeline** (from bundled release notes and site news; dates are the
developer's own):

| Date | Version | Notes |
|---|---|---|
| 1997 | 1.0-J betas → 1.2.4 | betas distributed partly on magazine CD-ROMs; 1.2.3 English-only; per JP release notes in the 1.2.4 archive ([Garden listing](https://macintoshgarden.org/apps/musashi)) |
| 1997-09-05 | 2.0.1 | reviewed by INTERNET Watch; ¥3,000 shareware ([Impress](https://internet.watch.impress.co.jp/www/article/970905/musashi.htm)) |
| 1998-01 | J-2.1 | Vector review ([Vector](https://vector.co.jp/magazine/softnews/980122/n9801225.html)) |
| 1998-07 | 2.2.2 | ATPM review; $30 shareware ([ATPM](https://atpm.com/4.07/page15.shtml)) |
| 1998-12-26 | 3.0 (JP only) | new archive-file mail store, virtual viewers, plugins ([JP 3.4.1 リリースノート, bundled doc]) |
| 1999 | 3.0.1–3.1.1 | early builds had real data-loss bugs (see 2.4) |
| 2000-05-08 | 3.2 | Cyrillic/Turkish/Greek plug-ins, attachment removal |
| 2000-07 → 2001-01 | "Musashi-V" public betas | big rewrite, Japanese PPC only ([beta page](https://web.archive.org/web/20010126222600/http://www.sonosoft.com:80/jp/musashibeta.html)) |
| 2001-07-28 | 3.2.5 | bugfix |
| 2001-12-20/22 | 3.3 / 3.3.1 | SMTP auth (CRAM-MD5), POP-before-SMTP |
| 2002-03 → 06 | 3.4 / 3.4.1 | **first OS X (Carbon) builds**; printing fix |
| 2006-06-19 | 3.4.2 | last maintenance release (bundled "What's new?") |
| 2008-03-14 (announced 2008-04-24) | **3.5 = freeware** | shareware code removed, minor fixes; "almost no changes" (author's words, translation: [2008 site](https://web.archive.org/web/2008/http://www.sonosoft.com/)) |

**Business story.** Sold as shareware via **Kagi** (the Register companion app
produced a mail/fax order form to Kagi, Berkeley; $25 single-user, $9 upgrade
from v2, 40-day evaluation — *How to register* and README in
`mu-ppc-341-en.sea.hqx`, listed on [Macintosh Garden](https://macintoshgarden.org/apps/musashi)).
Contemporary listings vary — $30 (ATPM, 1998), $33 (Mac Orchard, ~2001-02),
¥3,000 (Japanese reviews) — suggesting price drift or listing error; the README
inside 3.4.1 says $25.

**Why it declined.** No single "killed by X" event is documented. The observable
arc: a hugely prolific 1999–2002 cadence → a four-year gap → a 2006 micro-fix →
the 2008 freeware release described by the author as cosmetic. Meanwhile Misono
incorporated Sonosoft as a systems company (2008) ([company page](https://sonosoft.com/?page_id=32)).
Contributing pressures visible in the record: the Japanese shareware mail-client
market collapsed as free bundling (Outlook Express, then Apple Mail) absorbed
users; the **Musashi-V rewrite produced public betas but no verified stable
release**; and the move to OS X (Mail.app free, Eudora 5.1-J carbonized) stranded
a classic-Mac codebase — kimux's 2002 migration diary is a representative data
point. That Musashi's
code was never open-sourced or sold is inferred from absence of evidence;
no source release or acquisition is documented anywhere (unverified negative).

**What happened to the code/team.** Misono kept the code; 3.5 binaries ship the
same 68K/PPC/Carbon trio as before. The company still exists under him in Osaka.
The user community ran a mailing list (`musashi-users@pmb.biglobe.ne.jp`) and
produced third-party **modules** (icon/color packs, incl. a BeOS-style one),
converters (めんこぶ "Menkobu", KAEDE address-book export, MUSASHIビューア /
MUSASHI2 Viewer), and AppleScript helpers (Moz_MUSA `mailto:` bridge,
MUSAdora_sent) ([Rj's page](http://rj-chaos.sakura.ne.jp/chaos/Rj_MUSA.html),
[Musashi FAQ §1-05](https://web.archive.org/web/2006/http://serennz.cool.ne.jp/sb/sp/mac/musa_1.html)).

---

### 2.2 Feature inventory (primary sources)

Primary evidence: the English/Japanese HTML manuals and release notes inside
the 3.4.1 and 1.2.4 archives listed on [Macintosh Garden](https://macintoshgarden.org/apps/musashi),
plus `MENU`/`STR#`/`MPIF` resource dumps of the binaries (author's extraction;
all quotes below are the manual's own English).

**Architecture — multi-user + multi-account.** Musashi was designed so several
people share one installed app: a **manager password**, per-user password, and
"he cannot see other users' messages" (3.4.1 manual §4, bundled in
`mu-ppc-341-en.sea.hqx` from the [Garden listing](https://macintoshgarden.org/apps/musashi);
[Impress 2.0.1 review](https://internet.watch.impress.co.jp/www/article/970905/musashi.htm)
confirms mailboxes are invisible across users and a shared mailbox exists for
exchange). Each *user* then holds multiple *accounts* (POP3 receive + SMTP
send). Versions had send-only accounts, APOP, POP-before-SMTP, and SMTP
authentication (3.4.1 README and What's-new doc, bundled).

**Storage (2.x vs 3.x).** Per the community FAQ: 2.x kept `InBox`/`OutBox`/`Trash`
and each mailbox as separate files in mbox format (records separated by `"." + LF`);
3.x consolidated to per-user `Incoming Archive`, `Outgoing Archive`, `Message Index`,
`Local Archive`, `Message UIDL List`, `Signature`, `Filters`, `Address Book`
inside `System Folder:Preferences:Musashi Preferences Folder`
([FAQ §1-04](https://web.archive.org/web/2006/http://serennz.cool.ne.jp/sb/sp/mac/musa_1.html), translation).
A documented trick imported an mbox mailbox into 3.x by renaming a copy
`Local Archive` so it arrived as a pseudo local-forward ([FAQ §3-07](https://web.archive.org/web/2006/http://serennz.cool.ne.jp/sb/sp/mac/musa_3.html)).

**Windows.** Two viewer types: **Folder Viewer** (icons, spatial layout in icon
modes, or list mode; 3.x = max 3 levels, a folder holds folders *or* messages,
never both — JP manual; "Root window" = top folder viewer) and
**Message Viewer** = list pane + reading pane with a **focus mark** and
button panel. A third type, **Virtual Message Viewer**, surfaced any transient
result set: newly arrived mail, finds, remote-mail results, imports, filtered
output, local forwards received (3.4.1 manual §7, §10, §14 — bundled).

**Keyboard behavior** (from FAQ §1-06, translation): Space = page-scroll and,
at end of message, advance to the next message (jp341 manual §7 confirms the
same Space-→-next-message behavior in 3.x, translation);
`Ctrl-1…9` = auto-scroll at 9 speeds; Delete / `Ctrl-Shift-1…9` = reverse;
arrows = prev/next message (when list focused); `Cmd-R` reply, `Cmd-Opt-R`
reply-to-sender, `Cmd-Shift-R` reply-all; `Cmd-D/Shift-D/Opt-D` forward /
direct-forward / **local forward**; `Cmd-M` poll all accounts; `Cmd-T` empty
trash; `Cmd-<digit>` = switch between open windows; `Cmd-Opt-F`…`Cmd-Ctrl-Shift-F`
= find unread/read/unsent/sent; `Cmd-Return` send; Option+arrows jump to first/
last message; Option+click Move/Copy buttons → popup destination menus. Editor:
shift-arrows extend selection, option-arrows jump line start/end, drag text to
move, option-drag to copy.

**Find/search.** `Find` submenu: by status (Unread/Read/Unsent/Sent), by
detailed conditions (`Find...`), and string search within the listed messages
(`Find string...`, `Cmd-G` next). Results appear in a Virtual Message Viewer
(3.4.1 manual §14-1 — bundled).

**Filters.** Edited in a dialog; **first match wins**, priority by drag-order;
also applicable **manually** to selected messages and (per Japanese docs/Rj) to
**outgoing** mail (3.4.1 manual §12 — bundled, [Rj's page](http://rj-chaos.sakura.ne.jp/chaos/Rj_MUSA.html)).
Resource strings show predicate targets Subject/To/Cc/From/Reply-To/Headers/
Body/Whole Message/Account with Contains/Does-Not-Contain/Begins/Ends/Earlier/
Later/On-Time operators (binary `STR#` resources in `Musashi-3.4.1`).

**Address book.** Hierarchical groups containing entries **and other groups**;
names usable as aliases in To/Cc/Bcc (levels separated by `:`); the editor is a
real window with insert/drag-reassign (3.4.1 manual §8, §11 — bundled; ATPM review confirms
drag-between-groups). Shared-with-user tiers planned for templates in V.

**Messages & attachments.** Encodings: BinHex, Base64 (incl. AppleDouble/
AppleSingle), uuencode for send *and* extract (bundled README). Attachments render as
inline icons at the caret and delete like characters; option to extract
automatically to a folder; removable from received bodies (3.4.1 manual §6-2, §7-4 + bundled release notes).

**Remote Mail.** Lists server-held messages (subject/sender/date/size) before
download; mark for receive or delete; results open in a Virtual Viewer
(3.4.1 manual §13 — bundled) — a dial-up-era feature Mac Orchard reviewer Mark Webb praised.

**Customization.** Per-user preferences in five categories (Account / Message /
Color / Detail / Module): per-account signatures, `%N/%S/%D` reply-header
templates, quote prefix + optional sender-name prefix, date formats incl.
seconds and weekday, auto-check interval + check-at-startup, "send queued when
checking", word-wrap column, inline-input conversion, "Use MacOS default
controls" (Kaleidoscope compatibility!), auto-close dialogs, notify sound, and
colors for message/quote/URL highlights. **Per-encoding fonts** — a different
font per charset (3.4.1 manual §6, §14-4 — bundled). **Modules** = external resource files
swapping icons + color tables; **Plug-ins** (code resources) = message and
address-book import/export plus per-language text converters (3.4.1 manual §3 — bundled).

**Plug-in inventory** (binary `MPIF`/`STR#` in 3.4.1): message I/O — Musashi
Archive, Eudora TOC/Archive (in & out), Text File, raw data; address book —
Eudora Nickname in/out, UNIX `/etc/aliases`, raw; charset converters — ISO-2022-JP,
Shift-JIS, EUC-JP, ISO-8859-1/-2/-5/-7/-9, KOI8-R, Windows-1251, Big5, EUC-CN,
EUC-KR, plus raw ASCII/Quoted-Printable/Base64 pass-throughs.

**Other notable details.** Status flags: unread/read/sent/unsent/replied/
forwarded/pending/has-attachment (list icons); message "highlight" colors;
trash retained until `Empty Trash`; `Compress Archive` command; `Switch User`
without quit; clickable URLs (Cmd-click in 3.x; plain click in V beta); URL
underline + color prefs; character counter (option-click in editor); in 1.x,
messages over a **24KB** display limit were split into pages turned by
buttons (bundled 1.2.4 usage doc 使い方について.txt); print with selectable headers;
`mailto:` handling via third-party AppleScript (Moz_MUSA). The JP module
appendix documents the theming surface precisely: `cicn` resources
20001–20053 redefine every 24px panel-button icon, 21198/21199 are the
"has unread" and "has subfolders" marks, and a 150×200 PICT provides the
module title image (bundled jp341 manual appendix, translation).

**Musashi-V beta features** (design preview, never stable): user-defined
**labels** usable in search; **named saved "search conditions"** selectable from
a menu, with filters *required* to reference one (decoupling); **thread sort**;
unlimited folder depth; active/pull-model folder sharing; size column +
per-user column sets in list; multipart messages splittable per-part; editor
search-and-replace; **repeated-send mail-merge** (`$0`–`$F` vars + TSV parameter
file; produces saved drafts for review — explicitly *not* auto-sent)
([Musashi-V beta page](https://web.archive.org/web/20010126222600/http://www.sonosoft.com:80/jp/musashibeta.html), translation).

---

### 2.3 What users loved

Quotes are verbatim; Japanese items are marked translations.

- *"Musashi is an absolutely gorgeous e-mail client… The user interface is
  spectacular."* — Mac Orchard editors ([archived listing](https://web.archive.org/web/20020129171458/http://www.macorchard.com/cgi-bin/cgiwrap/themaco/frameit/email.html)).
- *"Uses only small memory, does all it has to do in an efficient and
  transparent way, has a simple and beautiful GUI… stable and does very well."*
  — Martin Buenger (same source).
- *"I love this program! Runs on 2MB RAM, simple interface, filtering,
  multi-user, custom configurable, clean."* — John Jones (same source).
- *"Musashi might have been named after the famous Japanese swordsman, as it is
  elegant, reliable, swift and sure, takes up very little memory… questions are
  answered gladly by real people at the Sonosoft home base."* — Gene Adam
  (same source) — direct author support was part of the appeal.
- *"It took me all of five minutes [to get running]… a lean little program… not
  once have I had a need to go back to Emailer."* — Frank Petrie, ATPM,
  comparing it to Claris Emailer and Eudora ([ATPM](https://atpm.com/4.07/page15.shtml)).
- "軽量で軽快な動作" — "lightweight and nimble" was the product's own pitch,
  echoed by reviewers ([2008 site](https://web.archive.org/web/2008/http://www.sonosoft.com/),
  [Vector](https://vector.co.jp/magazine/softnews/980122/n9801225.html); translation).
- Customization and "Macintosh-ness": 12+ mailbox icons, per-window colors/fonts —
  "いかにもMacintoshらしい作り" ("a very Macintosh way of doing things"),
  Impress 1997 ([review](https://internet.watch.impress.co.jp/www/article/970905/musashi.htm), translation).
- Multi-user privacy on a family/shared Mac was the headline differentiator in
  Japanese coverage — other clients' mailboxes were not isolated per user
  ([Impress 1997](https://internet.watch.impress.co.jp/www/article/970905/musashi.htm)).

### 2.4 What users disliked / its limits

- **Data-safety incidents in early 3.x**: release notes admit bugs where
  cancelling a user-switch password or a receive-time crash **lost the user's
  mail** (3.0.5, 3.0.1 notes; bundled リリースノート).
- **3.x storage consolidation drew a specific complaint**: a five-year user
  noted all mail sat in one file, worried one failure could destroy everything,
  and found partial backup hard; migrating ~240MB to Eudora required a
  third-party viewer ([Kimux](http://kimux.org/mac/musashi2-viewer.shtml), translation).
- Missing basics vs. rivals: **no spell-checker**, no automatic URL/email
  highlighting *and activation* in early versions (Mark Barnes; ATPM notes the
  two-page English manual was "poorly translated"), no per-size download limit
  flagged by Vector; accounts poll **sequentially**, not in parallel (Mark
  Webb); 68k attachment decode sometimes failed (Webb); groups had to be built
  by re-typing addresses (ATPM).
- Structural limits: 3-level folder cap and no mixing of folders+messages (JP
  manual); no IMAP — POP3-only throughout; **no SSL/TLS** (Garden listing states
  "no SSL encryption" [Macintosh Garden](https://macintoshgarden.org/apps/musashi));
  no threading until the unreleased V; list columns fixed (until V); no HTML
  mail composition (FAQ §5-02 asked "can't create HTML mail?"); modules don't
  work on Mac OS X ([Rj](http://rj-chaos.sakura.ne.jp/chaos/Rj_MUSA.html)).
- Adoption ceiling: English users flagged it was "almost unknown" and had "a
  terrible name for western people" (Buenger).

---

### 2.5 Lost or degraded features vs. today's mainstream clients

| Musashi feature | Today (Apple Mail / Outlook / Gmail / Spark / Thunderbird / Mimestream) |
|---|---|
| Per-user **private mail profiles on one machine** with manager password + "Switch User" | macOS user accounts cover this but at OS level; **within** one login, clients generally assume one identity set. None offer lightweight in-app profile switching with mutual invisibility. Gmail separates by browser profile; Outlook profiles are clunky; Apple Mail has no profiles. |
| **Local Forward** between users (server-free handoff) | No equivalent; sharing = forwarding through the server. |
| **Virtual Message Viewer** — every result set (new mail, searches, remote-mail picks, imports, manual filters) is a first-class, savable window | Saved searches/smart mailboxes exist (Mail, Mimestream, Thunderbird) but transient result *sets* as objects you can bulk-act-on, print, save-as-text are weaker; "apply filters to this selection, get a result window" is rare. |
| **Remote Mail** — browse server headers, mark keep/delete before downloading | JMAP/IMAP make this *cheap* (metadata before bodies), yet no mainstream client exposes a selective-download UI for POP-style flows; Mimestream/Apple Mail always fetch. |
| Per-charset **display fonts** + language plug-in charset pipeline | Clients hard-code UI text fonts; per-charset font overrides effectively absent. |
| Space-advances-message reading loop + **Ctrl-1–9 speed scroll** + focus mark | Mail/Outlook/Mimestream page-scroll but Space does not chain to next message universally; nobody offers variable-speed auto-scroll. |
| Mail-merge **repeated send** with tab-separated parameters (V beta) | Absent natively in all six (add-ons/scripts only). |
| **Spatial folder-icon windows** + per-folder custom icon | All list-based; folder icons are fixed. |
| Drag messages to desktop → text/mbox export; Option+drag → one file per message | Drag-to-desktop file export exists in Mail/Outlook but format/behavior inconsistent (eml); batch export to mbox needs extra steps. |
| Filter = **first-match ordered list, manual re-run on demand**, incl. outgoing mail | Server-side rules (Gmail/Exchange) run only at receipt; client rules (Mail/Thunderbird) can run on demand; outgoing filters are rare (Thunderbird supports them, others don't). |

*Opinion:* the modern mainstream has absorbed filtering, search, multi-account
— but **selective download, in-app multi-user privacy, and transient result-set
windows remain genuinely lost or degraded**, not merely nostalgic.

---

### 2.6 Re-imagining for 2026 (Sweetbriar, macOS 27 "Golden Gate", Apple silicon)

Current platform facts (verified 2026-10-11): **macOS 27 Golden Gate** shipped
2026-09-14, Apple-silicon-only (M1+; Intel unsupported), and is the **last**
release with full Rosetta 2 — i.e. new apps should be native ARM now
([Apple](https://www.apple.com/os/macos/),
[Apple Support HT149035](https://support.apple.com/en-us/149035),
[MacRumors](https://www.macrumors.com/2026/09/14/apple-releases-macos-golden-gate/)).
27.0.1 (bug fixes) shipped ~2026-09-28
([9to5Mac](https://9to5mac.com/2026/09/28/apple-releases-macos-27-0-1-golden-gate-plus-macos-tahoe-26-7-1-and-macos-sequoia-15-8-1/),
[HT127257](https://support.apple.com/en-us/127257)). Nothing in 27's changes
(Siri AI, Visual Intelligence, Liquid Glass tweaks) alters the feature design
below; what matters for Sweetbriar is the baseline: fast native SwiftUI/AppKit,
no Intel tail to serve.

Feature-by-feature revival plan, annotated **client-side** vs **server-dependent**:

| Musashi idea | 2026 form | Support needed |
|---|---|---|
| Multi-user profiles on one Mac | In-app profiles w/ Keychain-locked stores, "switch profile" like Safari profiles | Purely client-side (per-profile SQLite/maildir dirs) |
| Local Forward | "Send to profile" → writes to other profile's Local Archive queue; recipient profile sees items in an "Inter-user" virtual box | Client-side |
| Virtual Message Viewer | **Result-set windows**: searches, today's arrivals, filtered output, remote-pick results open as windows/collections you can act on or pin | Client-side; JMAP `Email/query` snapshots keep them live |
| Saved searches as named, shareable objects (V) | Named searches in Rust core, exposed as menu items + smart folders; filters reference named searches | Client-side; server-side search via JMAP `FilterCondition`/IMAP `SEARCH` accelerates but isn't required |
| Remote Mail | "Triage server": fetch headers (IMAP `ENVELOPE`+`BODY.PEEK`, JMAP `Email/query` metadata, Gmail API `format=metadata`, Graph `$select`), show list, user marks fetch/delete/archive; then fetch bodies | Header-only fetch is supported by all modern APIs, so the capability exists server-side; the *triage UI* itself is client-side |
| Labels (V) | Keyword/label model on IMAP `KEYWORDS`, JMAP `keywords`, Gmail labels; fallback local-only labels | Mostly client-side; sync quality varies (Exchange categories, Proton labels) |
| Thread sort (V) | JWZ threading in core for local; JMAP `Email/thread`/`Thread/get` on server where available | Both |
| Mail-merge repeated send | Templates with `{{var}}` + CSV/TSV param file → N *drafts* for review, then bulk send | Client-side entirely |
| Folder views & custom icons | List + icon spatial view optional; per-mailbox color/icon metadata synced via IMAP `METADATA` or local-only | Mostly client-side; icon sync needs `METADATA` (rare) |
| Per-charset fonts / encoding pipeline | Rust-side charset normalization to UTF-8; per-script display-font prefs | Client-side |
| Plug-in import/export converters | Core `importer`/`exporter` trait; formats: mbox, Maildir, EML, Thunderbird, Eudora TOC, CSV address book, vCard | Client-side |
| Filter engine (ordered, first-match, manual-run, incoming+outgoing) | Rule engine in Rust core; optional sync to server-side Sieve / Gmail filters / EWS rules as a bonus | Client-side core; server push only if provider exposes rule APIs (Gmail API does; Fastmail Sieve via JMAP partial) |
| Trash-keeps-until-emptied | Local semantics already standard; add "expunge on demand" | Client-side |
| Keyboard triage loop (Space→next message, Ctrl-1–9 autoscroll) | Space page-down → auto-advance to next unread; autoscroll speeds | Client-side |
| Background send/receive with progress window, cancelable | Async Rust engine + cancellable tasks, `sendLater` outbox | Client-side |

**The reverse-mapping warning:** several Musashi features were *expressions of
POP-era constraints* — remote mail existed because downloading was expensive;
sequential polling because connections were dial-up; local multi-user because
one Mac had one OS account in practice. With Gmail/Exchange/JMAP the
*constraints* are gone but the *use cases* (selective ingest before storing,
per-person privacy on a family iMac, result-set workflow) remain — implement
them for their utility, not for fidelity to POP mechanics.

---

## 3. Options compared

### What to adopt vs. adapt vs. skip

| Idea | Verdict for Sweetbriar | Why |
|---|---|---|
| Named saved searches + filters referencing them | **Adopt** | V-beta's cleanest idea; now standard-ish (Mail smart mailboxes) but pairing named conditions with filter rules is still rare |
| Multi-user profiles + Local Forward | **Adapt** | macOS 27 handles OS-level profiles; in-app switching still differentiates for shared family Macs and testing identities |
| Virtual viewers as result-set objects | **Adopt** | Unique interaction model: every query result is actionable and exportable |
| Remote Mail selective download | **Adapt** | With JMAP/IMAP it's a "preview & selective sync" feature — genuinely missing from modern clients |
| Spatial folder icons / per-folder icon | **Adapt** | Skip spatial layout; keep per-mailbox color/icon (cheap, beloved) |
| 24KB paging, 3-level folders, mbox | **Skip** | Technical limits, not features |
| Modules (resource-file theming) | **Adapt** | Modern form: theme bundles / icon packs; low priority |
| Charset plug-ins | **Adapt** | Rust charset-normalization at ingest; no plug-in needed but keep the *pipeline* idea |
| Mail-merge repeated send | **Adopt (low priority)** | Power-user differentiator; also useful for test mail |
| Ordered first-match manual filters incl. outgoing | **Adopt** | Matches Sweetbriar's local-core model; outgoing filters are a differentiator |

### Where modern clients already do it

| Capability | Notes |
|---|---|
| Labels | Gmail/Mimestream/Fastmail native; Thunderbird tags; Apple Mail "flags" (named colors) |
| Saved searches | Apple Mail Smart Mailboxes, Mimestream, Thunderbird |
| Threading | All mainstream (Gmail conversation view strongest) |
| Rules | Mail rules, Thunderbird filters, server-side Sieve/Gmail filters |
| Templates | Partial: Spark/Postbox snippets; Mail drafts workaround |

Musashi's *durable* differentiation is therefore: **in-app multi-user privacy,
result-set windows, remote/selective sync UX, outgoing filters, mail merge,
per-mailbox identity (icon/color), and a keyboard-speed-reading loop.**

---

## 4. Recommendations for Sweetbriar

**Do:**

1. **Result-set-first UI.** Make searches, filtered output, "mail waiting on
   server" and imports all open as named, manipulable collections (Sweetbriar's
   "Virtual Viewer"). This is the single most Musashi-specific, most
   still-missing pattern.
2. **Multi-user / multi-identity with Local Forward.** Cheap to build on a
   per-profile store; honors a real shared-Mac use case mainstream clients ignore.
3. **Selective-sync "Remote Mail" over IMAP/JMAP.** Headers-first triage UI;
   `BODY.PEEK`/`Email/query` metadata. For EWS/Gmail degrade to subject+snippet.
4. **Named saved searches shared by the filter engine.** One `Query` object in
   the Rust core; filters compose queries; queries are first-class menu items.
5. **Ordered, first-match filters that also run manually and on outgoing mail.**
   Store rules in the core; optionally mirror to Sieve/Gmail filters later.
6. **Crash-safe storage, learning from 3.x's mistake:** message-per-file
   (maildir-style) or append-only segments + SQLite index with WAL and periodic
   checkpoint + incremental backup hooks. Never a single opaque all-mail file —
   the Kimux migration story shows the failure mode users noticed.
7. **Import/export plug-in surface** (Rust trait, FFI-safe): mbox, Maildir,
   EML, Eudora TOC, CSV/vCard address books — Musashi's most practical idea.
8. **Keyboard triage loop:** Space=read-through + advance; option/ctrl
   modifiers for speed-scroll; `Cmd-<digit>` window switching.
9. **Per-mailbox color/icon + small footprint polish.** Cheap, high personality.

**Trade-offs / the strongest counter-argument.** Musashi itself is the
counter-argument: its ambition (multi-user, plug-ins, modules, V rewrite)
outran a one-person shop; V never shipped, and the product coasted into
freeware. If Sweetbriar chases Musashi's full menu — spatial views, module
theming, six localizations, mail merge — it repeats the same scope explosion.
**Argue for the minimum revival core (items 1–5 + 6's storage discipline)**
before any personality extras; every feature beyond that should have to prove
it earns its complexity. The other honest caveat: multi-user privacy has
largely been subsumed by OS accounts, so Sweetbriar's in-app profiles must be
justified by *convenience* (fast switching, shared family machine, testing),
not by nostalgia for Mac OS 9.

---

## 5. Risks and open questions

- **Legal/provenance:** Musashi's code and assets remain Sonosoft property; no
  source release is documented. Take ideas, not assets. (unverified whether any
  license exists beyond the binary downloads on Macintosh Garden.)
- **Musashi-V lineage:** the beta is described only by its own page and fan
  docs; whether any V code fed into 3.4/3.5 is unknown. Rj reports hearsay that
  a "version 4" was scrapped mid-development ([Rj](http://rj-chaos.sakura.ne.jp/chaos/Rj_MUSA.html), marked as rumor there too).
- **Auth methods:** the English manual documents CRAM-MD5; JP release notes add
  LOGIN/PLAIN; confirm exact set from binaries before claiming (partially verified).
- **Price discrepancy:** $25 (3.4.1 README) vs $30 (ATPM 1998) vs $33 (Mac
  Orchard ~2001) vs ¥3,000 (JP) — reported, not reconciled.
- **Magazine coverage:** Japanese magazine CD-ROMs carried the 1.0 betas
  (release note says "一部雑誌の付録CD-ROM"), but specific issues are
  unidentified; Macworld/TidBITS reviews not found (PowerMail note is the only
  TidBITS mention located).
- **Musashi ML archive** (`pmb.biglobe.ne.jp`) is presumed offline; not recovered.
- **Sonosoft today:** the company is alive but silent on Musashi; no
  discontinuation announcement located — the last update page just went quiet.
- Whether any Musashi binaries survive runnable in emulators (SheepShaver /
  Basilisk II) is plausible per Garden notes but not tested here.

---

## 6. Sources

Consulted 2026-10-11; Wayback captures as linked.

- [Macintosh Garden — Musashi](https://macintoshgarden.org/apps/musashi) (download
  listing; HTTP mirror used when HTTPS failed)
- [SONOSOFT site, 2008 WordPress capture](https://web.archive.org/web/2008/http://www.sonosoft.com/) —
  Musashi 3.5 freeware post 2008-04-24; ScreenGrid; PrintOptions; six-language downloads
- [SONOSOFT English index, 2002-12 capture](https://web.archive.org/web/20021202144725/http://www.sonosoft.com:80/index.html) —
  release news incl. first "MacOSX and Classic" 3.4 betas (2002-03-15)
- [SONOSOFT 1998 homepage](https://web.archive.org/web/1998/http://www.sonosoft.com/)
- [Musashi-V beta page, 2001-01-26 capture](https://web.archive.org/web/20010126222600/http://www.sonosoft.com:80/jp/musashibeta.html) —
  V feature list (labels, saved searches, threads, mail merge, unlimited folders, folder sharing)
- [Sonosoft company overview (live)](https://sonosoft.com/?page_id=32) — Misono, founding/incorporation dates, Osaka
- [INTERNET Watch review of MUSASHI 2.0.1, 1997-09-05](https://internet.watch.impress.co.jp/www/article/970905/musashi.htm)
- [Vector review of MUSASHI J-2.1, 1998-01](https://vector.co.jp/magazine/softnews/980122/n9801225.html)
- [Impress Watch digest — Musashi-V beta 10.31](https://internet.watch.impress.co.jp/www/digest/backno/0010/31.htm)
- [Impress Watch digest — Musashi 3.4 beta 4](https://internet.watch.impress.co.jp/www/digest/backno/0204/17.htm)
- [ATPM 4.07 review: Musashi 2.2.2 (July 1998)](https://atpm.com/4.07/page15.shtml)
- [Mac Orchard email applications page, 2002-01-29 capture](https://web.archive.org/web/20020129171458/http://www.macorchard.com/cgi-bin/cgiwrap/themaco/frameit/email.html) —
  editor capsule + five user reviews (Buenger, Jones, Barnes, Adam, Webb)
- [Musashi FAQ (SimpleBoxes / おおたに), 2006-09-12 capture](https://web.archive.org/web/20060912070615/http://serennz.cool.ne.jp/sb/sp/mac_musa.html)
  and subpages [musa_1](https://web.archive.org/web/2006/http://serennz.cool.ne.jp/sb/sp/mac/musa_1.html),
  [musa_3](https://web.archive.org/web/2006/http://serennz.cool.ne.jp/sb/sp/mac/musa_3.html) —
  file layout, name origin, shortcuts, shared mail vs local forward, mbox trick
- [Rj's Musashic Page](http://rj-chaos.sakura.ne.jp/chaos/Rj_MUSA.html) — modules, V-beta
  notes, Misono sole programmer, community links, utilities
- [Kimux — MUSASHI2 Viewer / migration notes](http://kimux.org/mac/musashi2-viewer.shtml) —
  2.x→Eudora 5.1-J migration, 3.x single-file storage concern
- [TidBITS — PowerMail 3.0.6 with Musashi import](https://tidbits.com/2000/11/03/powermail-3-0-6-released-mac-os-x-version-in-public-beta/)
- [Macintosh Repository — Musashi 3.x](https://www.macintoshrepository.org/903-musashi-3-x)
- [Apple — macOS 27 Golden Gate](https://www.apple.com/os/macos/) and
  [security content / compatibility (HT149035)](https://support.apple.com/en-us/149035)
- [Apple — What's new in the updates for macOS 27 (HT127257)](https://support.apple.com/en-us/127257)
- [MacRumors — macOS Golden Gate release](https://www.macrumors.com/2026/09/14/apple-releases-macos-golden-gate/)
- [9to5Mac — macOS 27.0.1](https://9to5mac.com/2026/09/28/apple-releases-macos-27-0-1-golden-gate-plus-macos-tahoe-26-7-1-and-macos-sequoia-15-8-1/)

Bundled artifacts (primary source; from archives downloaded via the Macintosh
Garden links above): `mu-ppc-341-en.sea.hqx`/`musashi-68k-35-*.sea`/`mu-osx-341.sit`
— README, "How to register" (Kagi, $25/$9, 40-day), "What's new?" (3.2→3.5
timeline), English + Japanese HTML User's Manuals (§§1–14 + appendix), Japanese
リリースノート and 使い方 for 1.2.4, and binary `MENU`/`STR#`/`MPIF`/`PICT` resource
dumps (menus, filter predicates, plug-in format names, charset list). Extraction
performed locally; tools adapted from XADMaster (LGPL-2.1+) per
[XADStuffIt parsers](https://github.com/MacPaw/XADMaster).
