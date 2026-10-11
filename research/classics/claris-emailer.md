# Claris Emailer (Fog City Software / Claris, 1995–1998)

Synthesis of [raw/emailer/devin.md](../raw/emailer/devin.md) and
[raw/emailer/codex.md](../raw/emailer/codex.md). Primary source: the 1995
*Claris Emailer (Lite) User's Guide*
([archive.org](https://archive.org/details/claris-emailer-lite-manual)),
Apple and Claris readmes, the Fog City utilities pages, TidBITS, Macworld
and MacUser reviews.

## In one paragraph

Emailer was built by Guy Kawasaki's Fog City Software, with Jud Spencer as
lead developer, and licensed to Claris in April 1995. Version 1.0 shipped in
July 1995. Its founding idea was **one In Box, one Out Box, one address book
and one rule engine across every mail service you used**: POP/SMTP, AOL,
CompuServe, eWorld, RadioMail, later AppleLink and OfficeMail. It was
offline-first for dial-up, connecting in scheduled batches. Macworld (July
1997) called it "the best e-mail client currently available for the
Macintosh". It died in Apple's January 1998 restructuring of Claris into
FileMaker. The final 2.0v3 was rushed out first, and the core developers
went to Microsoft, where Outlook Express for Mac became Entourage and then
Outlook for Mac (community recollection). The source was never released
and may be lost.

## Timeline

| Date | Event |
|---|---|
| 1995-04 | Claris licenses Emailer; Fog City keeps developing |
| 1995-07 | 1.0: multi-service, Schedules, Locations, Mail Actions |
| 1995–96 | 1.1.x; Emailer Lite (one Internet account) bundled with the Apple Internet Connection Kit and Mac OS 8 |
| 1997-04 | 2.0: database storage, hierarchical folders, stronger Actions, spelling, multiple and random signatures, 19 named and coloured priorities |
| 1998-01/02 | 2.0v3: Redirect, live-linked groups, contextual menus, folder dragging; Claris becomes FileMaker Inc. |
| 1998-10 | TidBITS "Emailer and the Death of Software"; rescue petitions fail |

## Feature inventory (highlights)

- **Services and identities:** one or many accounts per service.
  - Per-recipient destination service and a "Send via" account.
  - Two accounts sharing one POP mailbox give two identities.
  - Address translation per service.
- **Connections.** Connect Now and Connect Again. Status window shows the
  subject being downloaded and an estimate. Stop, and Option-Stop to cancel
  everything pending.
  - **Schedules:** named, timed or repeating, with account selection and
    separate get/send. They needed the app running.
  - **Locations:** per-network dial profiles.
- **Out Box:**
  - offline composing; "Do Not Schedule" holds a message
  - **Send Now sends only this message**, versus flushing the queue
  - **individual recipients can be marked for resending**
- **Mail Actions:**
  - named, enabled or disabled, ordered rules
  - criteria on any header, body, account or service, plus **is / isn't in
    the address book** and **is / isn't in group X**
  - effects: set priority, file, **auto-reply with edited text**,
    auto-forward, print, run AppleScript, add sender to contacts, add or
    remove group members
  - ordered with stop semantics, plus a "don't auto-reply to mailing
    lists" safety
  - **Auto File Log:** a durable audit of what the rules filed where
  - the AppleScript `filter` verb re-runs Actions on stored mail
- **Automatic handling:**
  - file messages after reading, sending or replying
  - **auto-delete read, sent or deleted mail after N days**
  - Go to New Mail
- **Keyboard:**
  - **Cmd-Opt-F, type the folder name, Return** to file
  - Cmd-[ / ] for previous and next
  - holding Command reveals button shortcuts
- **Address book:** type-to-filter (reviewers' favourite), multiple
  addresses per contact across services, groups.
- **Composer:** multiple and **random signatures**, configurable
  attribution line, Paste as Quotation, a spell checker that skips quotes,
  enclosures with automatic StuffIt compression.
- **Scripting:** AppleScript menu and dictionary. **Fog City's script
  library** filled gaps: BBEdit editing, unwrap and quote reduction,
  digest extraction, mail merge, templates and glossaries, FileMaker
  archiving, Eudora import and export.
- **Storage:** 1.x stored one Finder file per message (wasting HFS
  blocks). 2.0 used a monolithic Mail Database plus Mail Index, with
  Option-launch rebuilds.

## What users loved, and what they didn't

- **Loved:**
  - "all inboxes integrated together as one"
  - filters that could reply and forward ("perhaps the best I've seen",
    Engst 1995)
  - "knowing which email address to use based upon the recipient's email"
  - keyboard filing
  - filing straight from the message window
  - reliability ("never crashed" over 13 years for one user)
  - the scripting community (Emailer-talk)
- **Disliked:**
  - the 2.0 database: whole-file backups (87 MB copied after one change),
    corruption with ghost entries and rebuild rituals, painful export
  - slow all-folder search
  - plain text only, so HTML mail showed as tags by 2000
  - MacUser found 2.0 more complex
  - rule logic hard to reason about; scripts could trigger but not edit
    Actions

## What survives today

- Every client unifies IMAP accounts.
- Thunderbird and Outlook span protocol families.
- Scheduled *sending* is common; scheduled *fetching* and batch modes are
  gone.
- **Gone:**
  - per-recipient send state
  - "send just this one"
  - the Auto File Log
  - built-in file-after-read
  - per-folder retention
  - named priorities
  - random signatures
  - a configurable attribution line
- Apple Mail rules sync through iCloud, but their scripts and sounds don't.
- Outlook for Mac no longer supports client rules.

## What Sweetbriar should take

1. **The unified-service soul:** one normalized model over IMAP, Graph,
   JMAP, Gmail and Proton/Bridge adapters, while keeping the user-facing
   simplicity of one In Box, Out Box, address book and rule engine.
2. **An Out Box as an inspectable queue:** Draft, Held, Ready, Submitting,
   Accepted, Needs attention; per recipient where SMTP allows; "send this
   one now" vs flush; honest uncertainty. Graph's `202 Accepted` doesn't
   mean delivered.
3. **Actions with explanation:** preview on selected mail, "why this
   matched", per-message execution records so a resync never repeats an
   auto-reply, and an Auto File Log as the audit and undo view. Auto-reply
   safety: `Auto-Submitted`, list detection, rate limits.
4. **Workflow automation:** file after read, send or reply; file replies
   with their originals; per-folder retention with tombstones and no
   silent purges.
5. **Keyboard filing** with learned destinations, and holding a modifier
   to reveal shortcuts.
6. **Named, coloured priorities** that map to IMAP/JMAP keywords and RFC
   9979 flags where possible.
7. **Profiles as modern Locations:** per-network or per-Focus sync,
   download and notification policy, with a visible pause.
8. **Small delights:** random signatures, a configurable attribution, a
   status window showing what is being fetched.
9. **Avoid:** monolithic opaque storage, modal connection dialogs,
   service-specific address translation, and above all the exit cost.
   Ship export on day one.
