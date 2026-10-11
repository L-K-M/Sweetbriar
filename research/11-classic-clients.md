# 11 — What made classic clients beloved, and what Sweetbriar should revive

Cross-client synthesis. Detailed per-client documents:

- [classics/eudora.md](classics/eudora.md): manual, Mac source audit, history
- [classics/mailsmith.md](classics/mailsmith.md): manual and help book
- [classics/claris-emailer.md](classics/claris-emailer.md): 1995 manual, readmes
- [classics/musashi.md](classics/musashi.md): original archives, EN and JP manuals, resource forks

Other clients come from [raw/classics-other/codex.md](raw/classics-other/codex.md):
Opera M2, The Bat!, Mulberry, Alpine/Pine, Mutt, Gnus, Pegasus, PowerMail,
Entourage, Lotus Notes, Becky!, NeXTMail, Sparrow, Mailbox, Google Inbox,
Postbox, MailMate, Mail Act-On/MailTags. The landscape notes and HN, Reddit
and MPU discussions supplied the rest. Checked 2026-10-11.

## The pattern behind the love

Across every beloved client, the recurring appeal is **agency**. Users
could:

1. Find anything, precisely (regex, field queries, search results you can
   act on).
2. Act on what they saw: select similar, file by keyboard, redirect, send
   again, make a rule from this message.
3. Teach the app their workflow: rules, stationery, clippings, scripts,
   identities, named priorities.
4. Trust where their mail was and see what the app was doing: a visible
   queue, local copies, logs, export.
5. Work fast in dense, keyboard-driven, native interfaces.

Beckley on why Eudora's Thunderbird-based successor failed: "Users come to
depend on those subtle interactions, and, when they aren't there, it just
feels different." The lesson: revive **interaction details and workflow
semantics**, not screenshots.

## Why the features disappeared

| Client | Documented end |
|---|---|
| Eudora | Qualcomm exit, 2006 |
| Claris Emailer | Claris restructuring, 1998 |
| Mailsmith | "Competing with free"; never shipped IMAP; 32-bit death |
| Sparrow | Google acquisition, 2012 |
| Mailbox | Dropbox shutdown, 2016 |
| Google Inbox | Folded into Gmail, 2019 |
| Postbox | Acquired by eM Client, 2024 |
| MailTags / Mail Act-On | Mail plug-ins broken in Sonoma |
| Musashi | One developer's intermittent maintenance |

Broader "simplification" theories are inference. **Many "lost" features
survive somewhere**: MailMate (search, Markdown, keyboard bindings, rules
on any mailbox), Thunderbird (saved searches, outgoing filters), MailMaven
(MailTags successor: notes, projects, review dates, expected replies,
alternate subjects), Vivaldi (Opera M2's views), Gmail (templates,
filter-from-message). **Sweetbriar's opportunity is a coherent, native,
cross-provider implementation.** Few individual features would be unique.

## Revival catalog, ranked for Sweetbriar

Ranked by fit with Sweetbriar's priorities (retrieval, safety,
responsiveness, cross-platform reuse, everyday usefulness) and by how
foundational each item is to the data model. **Data model** marks items the
store must anticipate from day one even if their UI ships later.

| # | Revive | Antecedents | Sweetbriar form | Tier | Data model |
|---|---|---|---|---|---|
| 1 | **Search you can trust and act on** | Eudora Find (regex, results act as a mailbox, live searches), Mailsmith Advanced Query (PCRE, fuzzy), Musashi (search within results, load rule into search), PowerMail, Opera M2, MailMate | One query AST for search, saved views, rules and scripts; visible coverage; exact by default; regex expert mode; results are actionable mailboxes ([05-search](05-search.md)) | v1 | Yes |
| 2 | **An inspectable Out Box with honest states** | Eudora (statuses, Timed Queue, Send Again), Emailer (send this one now vs flush; per-recipient resend; Do Not Schedule) | Held / Queued / Submitting / Accepted / Failed / OutcomeUnknown; send now vs queue; timed send; per-recipient results where SMTP reports them ([04-data-safety](04-data-safety.md)) | v1 | Yes |
| 3 | **Independent local archive and recoverable drafts** | Mutt local stores, Notes replicas, NeXTMail versioned drafts, Mailsmith 2.2 per-message files, Eudora's fear-of-migration loyalty | "Available offline" ≠ "independent copy"; immutable originals, versioned drafts, export with manifest, tested restore | v1 | Yes |
| 4 | **Keyboard filing and composable batch actions** | Emailer Cmd-Opt-F, Mailsmith 2.1, Postbox Quick Bar, Mail Act-On, Mutt macros | Command palette; fuzzy, account-qualified destinations learned per sender or list; batch with visible result and undo; all commands in menus | v1 | — |
| 5 | **Overlapping live views and reading sessions** | Opera M2 views, The Bat! virtual folders, Gnus groups, MailMate smart mailboxes, **Musashi's Virtual Message Viewer**, **Engst's Eudora attention queue** | Saved views across accounts; a "reading session" that stays put while rules file mail; separate Read / Done / File; "delete view" ≠ "remove label" ≠ "delete message" | v1 | Yes (session state) |
| 6 | **Identities with context rules** | Eudora Personalities, Alpine roles, Mulberry, Pegasus, Mutt hooks, Emailer "Send via" | First-class identities; reply from the receiving identity; optional rules suggest From, signature, template and Sent location; mismatch warnings; never locked once composing (Alpine's flaw). More valuable now that Gmail drops third-party Send-as (January 2027) | v1 | Yes |
| 7 | **Explainable rules across the lifecycle** | Eudora (incoming, outgoing, manual; Make Filter; dinosaur), Emailer Mail Actions (address-book/group criteria, auto-reply, Auto File Log), Mailsmith distributed filters, The Bat! event types, Mulberry Sieve | Phases: arrival, pre-send, post-send, read/replied, manual. Preview ("42 match, 11 would move"), trace ("why did this happen?"), execution records, revisions, stale-rule hints. Runs on this Mac vs on server; Sieve editor; mailbox-attached mode as advanced | v1 core, v2 depth | Yes |
| 8 | **Private annotations and display overlays** | Mailsmith Notes, MailTags (keywords, projects, notes, dates), Becky notes, Eudora Make Subject/Pencil, Musashi Get Info | Versioned private notes, alternate subject/sender, tags; search both original and overlay; never rewrite received mail; explicit sync policy (keywords where possible) | v1 | Yes |
| 9 | **Follow-up states, not just snooze** | Mailbox Later, Inbox reminders, Becky reminders, MailTags dates, MailMaven expected replies | Distinct "read later", "reply by", "waiting for reply" with conservative reply matching; handles sleep, time zones and missed deadlines | v2 | Yes |
| 10 | **Text workbench and controllable composer** | Mailsmith (rewrap quoted, Paste Quoted, six clipboards, rectangular selection, Zap Gremlins, clippings with placeholders), Mutt external editor, MailMate Markdown, Claws reply-to-selection, Emailer random signatures and attribution | Plain text and Markdown first-class; quote-depth rewrap; selected quotation; editable attribution; clippings/templates with typed placeholders (portable files, Musashi format); external editor round trip; clipboard history | v1 basics, v2 depth | Templates |
| 11 | **Redirect, Send Again, select similar, Make Filter** | Eudora, Emailer 2.0v3, Apple Mail | Honest redirect (Resent-*, DMARC-aware, forward-as-attachment fallback); edit-as-new; Option-click select similar; rule from selection | v1 | — |
| 12 | **Custom bundles with batch completion** | Google Inbox bundles, HEY sender bundles, Opera | Bundles by query, list, project or sender set; collapsed summaries; review schedules; "archive reviewed" with preview and undo; deterministic first | v2 | Views |
| 13 | **Thread trees and correspondence context** | Gnus, Mutt, MailMate thread arcs and correspondence expansion | Optional flat / conversation / tree views; include related Sent and archived mail; reversible manual grouping | v2 | Thread graph |
| 14 | **Mailing-list reading and attention state** | Opera (seen vs read, follow/ignore), Gnus scoring, Becky, MailMate list submailboxes and mute | List-Id views; follow/mute threads; private seen/action state separate from provider \Seen; learned priority only with reasons | v2 | Yes |
| 15 | **Automation and durable message links** | Emailer and Mailsmith AppleScript (Mailsmith menu-intercept scripts; Fog City's script library), PowerMail, Claws Actions, MailMate emate CLI, Eudora EMSAPI | One versioned command API: menus, shortcuts, AppleScript, App Intents (mail domain), CLI, D-Bus; `sweetbriar:` links that survive filing; pre-send hooks (Gruber's guard); out-of-process extensions with declared permissions | v1 API, v2 depth | IDs |
| 16 | **Digest/concentrated reading** | Eudora Content Concentrator, Mailsmith digest bursting | Thread-as-one-document view with quotes and signatures folded, reversible; MIME digests as navigable items | v2 | Derived |
| 17 | **Attachment library** | Postbox, The Bat! save-all, HEY | Files view by type, sender, date, project; provenance back to messages; safe batch save | v2 | Derived |
| 18 | **Link safety and pre-send checks** | Eudora ScamWatch, MoodWatch, BossWatch; Mailsmith netiquette warnings; Postbox domain fencing | Hover shows the true URL, warnings on deceptive links; optional pre-send checks (recipient domains, missing attachment, tone), advisory | v1 links, v2 checks | — |
| 19 | **Workflow automation and retention** | Emailer (file after read, send or reply; auto-delete read/sent after N days) | Per-folder workflow rules; retention with tombstones, never silent | v2 | — |
| 20 | **Statistics, Link History, inspectable advanced settings** | Eudora | Activity dashboard from local data; link history; typed searchable advanced settings with confirmation links | v3 | — |
| 21 | **Spatial personality** | Musashi icons and modules, Emailer named colours | Optional folder symbols and colours, accessible state badges | v3 | — |
| 22 | **Project collections** | Entourage Project Center, Notes links, MailMaven projects, HEY Collections | Lightweight collections of message refs, notes, follow-ups, approved file links | v3 | Yes |

**Explicitly not revived:**
- ESP (file sync over mail)
- ad-supported modes
- dial-up chrome (modal connection dialogs, Locations as dial profiles;
  network-aware sync profiles are the modern form)
- POP-only assumptions
- monolithic opaque stores
- rewriting received MIME
- auto-confirming dialogs (Musashi's timer)
- menu hiding
- AppleScript recording (expensive)
- BinHex/AppleDouble/StuffIt
- Lotus Notes-style application platforms

## Guardrails (from the reviews and the failures)

- **Progressive disclosure.** A conventional native mail window by default.
  Power appears on demand. Sparrow's restraint is remembered as fondly as
  Mailsmith's depth.
- **Explain before acting.** Every rule, bundle, batch, retention policy
  and destructive action shows scope and preview, and keeps a trace.
- **Server vs Mac.** Always state where a rule runs. Never run equivalent
  client and server side effects twice.
- **Local metadata is primary data.** Every one of Eudora, Mailsmith and
  Emailer lost metadata in rebuilds.
- **Exit path.** Export everything, with a manifest. Emailer users had to
  write their own extraction scripts.

## Key sources

See the per-client documents. Notable: [Opera M2 views](https://stuff.mit.edu/afs/sipb/project/opera/share/opera/help/m2.html), [The Bat! virtual folders](https://www.ritlabs.com/en/support/help/63/), [Alpine roles](https://alpineapp.email/alpine/alpine-info/roles/), [Mulberry manual](http://www.mulberrymail.com/downloads/Mulberry/docs/ReferenceMac.pdf), [MailMate manual](https://manual.mailmate-app.com/), [MailMaven](https://mailmaven.app/support/gtk/organization.html), [Google Inbox launch](https://blog.google/products-and-platforms/products/gmail/an-inbox-that-works-for-you/), [SmallCubed (Sonoma plug-in break)](https://smallcubed.com/support/), [HN: choosing email software](https://news.ycombinator.com/item?id=31238233), [CHM Eudora history](https://computerhistory.org/blog/the-eudora-email-client-source-code/).
