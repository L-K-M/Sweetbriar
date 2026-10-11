<!-- provenance: helper=codex model=gpt-6.1-sol started=2026-10-11T19:00:15+00:00 exit=0 brief=research/briefs/ -->

# Sweetbriar: what beloved email clients can teach a new native client

Research date: **2026-10-11**. Current-product documentation and status statements below were checked on that date; historical sources retain their publication dates. This is documentary research, not a compatibility or performance test.

## 1. TL;DR

- **Revive control over correspondence:** dependable search, keyboard filing, overlapping views, private annotations, and recoverable local data. These recur in discussions about [MailMate and other clients](https://news.ycombinator.com/item?id=18057643), [local archives](https://news.ycombinator.com/item?id=31238233), and [MailTags/Mail Act-On workflows](https://tidbits.com/2017/01/19/73-mac-automation-stories-from-tidbits-readers/).
- **Many “lost” features survive somewhere.** MailMate documents powerful queries and keyboard workflows; MailMaven documents successors to SmallCubed’s tagging and rule workflows; Vivaldi preserves Opera M2’s view-based approach. Sweetbriar’s opportunity is a coherent, native, portable implementation. [MailMate](https://manual.mailmate-app.com/), [MailMaven](https://mailmaven.app/support/gtk/processing.html), [Vivaldi](https://vivaldi.com/blog/vivaldi-mail-technical-preview/)
- **Some losses have clear causes:** acquisitions ended Sparrow and Postbox development; Dropbox closed Mailbox; Google consolidated Inbox into Gmail; Apple’s legacy Mail plug-in system disappeared in Sonoma. Broader explanations about economics or simplification remain inference. [Sparrow](https://tidbits.com/2012/07/21/sparrow-bought-by-google/), [Postbox](https://support.postbox-inc.com/hc/en-us/articles/26988085310103-Postbox-has-been-acquired-by-eM-Client), [Mailbox](https://blog.dropbox.com/topics/company/saying-goodbye-to-carousel-and-mailbox), [Inbox](https://blog.google/products-and-platforms/products/gmail/inbox-signing-find-your-favorite-features-new-gmail/), [SmallCubed](https://smallcubed.com/support/)
- **Use Opera’s model without banning folders:** one message can belong to several saved views, labels, projects, and attention states, while retaining real server mailbox membership. [Opera explanation](https://stuff.mit.edu/afs/sipb/project/opera/share/opera/help/m2.html), [JMAP mail model](https://datatracker.ietf.org/doc/html/rfc8621)
- **Recover the less obvious classics:** Alpine’s context-sensitive identities, Mulberry’s server-side filtering interface, Entourage’s linked project material, and NeXTMail’s versioned drafts. [Alpine roles](https://alpineapp.email/alpine/alpine-info/roles/), [Mulberry manual](http://www.mulberrymail.com/downloads/Mulberry/docs/ReferenceMac.pdf), [Entourage review](https://tidbits.com/2004/06/14/entourage-2004-important-incremental-changes/), [NeXTSTEP guide](http://bitsavers.informatik.uni-stuttgart.de/pdf/next/NeXTSTEP_User_Guide_1994.pdf)
- **Snooze alone is insufficient.** Inbox’s custom bundles, highlights, reminders, and assists formed a larger workflow; modern Gmail’s fixed categories and HEY’s sender bundles are partial equivalents. [Inbox launch, 2014-10-22](https://blog.google/products-and-platforms/products/gmail/an-inbox-that-works-for-you/), [Gmail categories](https://support.google.com/mail/answer/3094499?co=GENIE.Platform%3DDesktop&hl=en-NP), [HEY bundles](https://help.hey.com/article/765-bundle-emails)
- **Target macOS 27 Golden Gate and 26 explicitly.** Apple confirms 27’s release on 2026-09-14, Apple-silicon compatibility, and 27.0.1 on 2026-09-28. Its advertised Mail/Spotlight search improvements raise the baseline; they do not establish measured superiority. [Releases](https://support.apple.com/en-us/100100), [Compatibility](https://support.apple.com/en-us/127455), [Changes](https://support.apple.com/en-us/127257)
- **Recommendation:** build search, recoverable storage, keyboard actions, and saved views first; add metadata and follow-up workflows next. The strongest objection is that this could reproduce an expert’s complicated toolbox. Progressive disclosure and a small initial feature set should be product requirements.

## 2. Findings

### How to read the evidence

“Lost” here means **retired, weakened, less discoverable, or fragmented across products**, rather than absent from every modern client. The feature selections below identify each client’s distinctive strengths. Reviews and discussions establish that particular people valued them; manuals establish that the features existed. A population-wide ranking of users’ favorite features is **unverified**.

Affection evidence is especially strong for Opera M2, Sparrow, Inbox, Postbox, MailMate, and SmallCubed’s plug-ins. For Outlook Express, Netscape Messenger, Becky!, and NeXTMail, the evidence assembled here is stronger on capabilities than on which feature most users loved. Lotus Notes deserves particular caution: useful replication and document linking do not imply that everyone liked its interface.

No old binary was run. Current documentation does not prove that a client works on macOS 27, that its implementation is reliable, or that it outperforms a competitor. Unless specifically documented below, those claims are **unverified**.

### What changed for this topic in macOS 27

As checked **2026-10-11**, Apple lists macOS 27 Golden Gate’s release as **2026-09-14**, lists **27.0.1** as current, and dates that update **2026-09-28**. Apple’s compatibility page specifies Macs with Apple silicon. macOS 26 Tahoe is the preceding release family and remains relevant to Sweetbriar’s proposed support matrix. [Apple release register](https://support.apple.com/en-us/100100), [27 compatibility](https://support.apple.com/en-us/127455)

Apple’s **2026-09-28** update notes advertise improved Mail and Spotlight search, Siri AI using personal context across mail and other material, and Liquid Glass readability controls plus toolbar/sidebar refinements. Availability varies by device and region. These are documented product changes, not independent search-quality or latency results. Sweetbriar should offer exact, inspectable retrieval alongside any optional natural-language search. [Apple’s macOS 27 changes](https://support.apple.com/en-us/127257)

Apple’s current Mail guide is explicitly for **macOS 27**. It documents categories, Smart Mailboxes, and rules; separate guidance documents local “On My Mac” mailboxes and Remind Me. Therefore, saved views, local storage, and resurfacing messages are already part of the native baseline. Remind Me’s documented return-to-attention behavior should not be assumed identical to a snooze operation that hides a message until a deadline. [Mail 27 guide](https://support.apple.com/en-ie/guide/mail/welcome/mac), [Smart Mailboxes and categories](https://support.apple.com/en-ie/guide/mail/mlhlp1190/16.0/mac/27), [Local mailboxes](https://support.apple.com/en-nz/guide/mail/mlhlp1021/mac), [Remind Me](https://support.apple.com/en-ph/guide/mail/mlhl96dfe8ce/mac)

The relevant extension break predates 27: SmallCubed states that **macOS 14 Sonoma** no longer loads its traditional Mail plug-ins. Apple’s supported MailKit documents content blocking, message actions, composition assistance, and security extensions. That is evidence of a changed extension surface, not evidence that all extensions vanished or that arbitrary former plug-in behavior can be reproduced. [SmallCubed legacy support](https://smallcubed.com/support/), [Apple MailKit](https://developer.apple.com/documentation/MailKit)

### Client survey: keyboard, protocols, and programmable reading

Each row selects **one to five features**, grouping closely related behavior. These are design lessons, not recommendations to run obsolete software.

| Client | Features selected | Why people valued them; evidence |
|---|---|---|
| **Mulberry** | 1. IMAP access-control and quota tools. 2. Disconnected operation with reconnection synchronization. 3. Graphical server-side Sieve rules. 4. Explicit identities and outgoing-account/security settings. 5. Multiple mailboxes open together. | Serious server capabilities were exposed directly to users. A **2002-04-29** review particularly praised its IMAP-first design and mixed mailbox support; the Mac manual documents ACLs, offline operation, identities, and Sieve. [Review](https://www.linux.com/news/mulberry-powerful-multi-platform-email-client/), [Manual, printed pp. 24, 85–86, 151, 191 onward](http://www.mulberrymail.com/downloads/Mulberry/docs/ReferenceMac.pdf) |
| **Pine / Alpine** | 1. Visible command-key help. 2. Selection followed by “Zoom” to restrict the index. 3. Roles choosing From, signature, headers, and sent-copy destination. 4. Resuming postponed or interrupted compositions. | Discoverable keyboard control and continuity between reading and composing. The cited documentation is **Alpine** documentation; it does not prove that every Pine version had identical behavior. [Configuration](https://alpineapp.email/alpine/UW/tech-notes/config.html), [Roles](https://alpineapp.email/alpine/alpine-info/roles/), [Compose recovery](https://alpineapp.email/alpine/alpine-info/commands/compose.html) |
| **Mutt; continuing ideas in NeoMutt** | 1. Remappable keys and multi-action macros. 2. Threaded reading. 3. Regex/Boolean selection and view limiting. 4. Context-dependent hooks. 5. External-editor composition. | Users can build repeatable workflows instead of performing individual mouse operations. Mutt’s manual documents these mechanisms. NeoMutt separately documents extensions including Notmuch/IMAP tagging and configurable threading; its feature list distinguishes stable and development features. [Mutt manual](http://www.mutt.org/doc/manual/), [NeoMutt features](https://neomutt.org/feature), [NeoMutt documentation](https://docs.neomutt.org/reference/releases/features.html) |
| **Gnus** | 1. Editable scoring and kill rules. 2. Adaptive scoring with decay. 3. Topic/group organization and search groups. 4. Detailed thread-oriented reading. 5. Offline Agent operation. | An unusually explicit model of attention: interestingness, group state, and offline availability can be configured independently. Its score-tracing commands make the reasons inspectable. This remains documented functionality, not an extinct idea. [Gnus manual](https://www.gnu.org/software/emacs/manual/html_mono/gnus.html), [Score files](https://www.gnu.org/s/emacs/manual/html_node/gnus/Score-File-Format.html), [Command reference](https://www.gnu.org/software/emacs/refcards/pdf/gnus-booklet.pdf) |

### Client survey: rich desktop tools and integrated communications

| Client | Features selected | Why they matter; evidence |
|---|---|---|
| **The Bat!** | 1. Sorting Office rules for incoming, outgoing, read, and replied messages. 2. Quick Templates. 3. Virtual folders. 4. Color groups usable in organization. 5. Bulk attachment saving. | Correspondence can trigger different actions throughout its lifecycle. A firsthand CRN account praises automation and a focus on mail; the vendor documents the specific mechanisms. [Features](https://ritlabs.com/en/products/thebat/features.php), [Virtual folders](https://www.ritlabs.com/en/support/help/63/), [Color groups](https://www.ritlabs.com/en/support/help/68/), [Firsthand review](https://www.crn.com/features/channel-programs/187202508/beyond-outlook-five-alternative-e-mail-apps) |
| **Pegasus Mail** | 1. Rich filtering and trainable Spamhalter. 2. Selectable identities. 3. Distribution-list management. 4. Folder-oriented desktop working space. 5. Selective POP preview/download plus offline reading/composition. | Control over acquisition, organization, and repetitive correspondence. The vendor overview supports these selections; CRN’s Pegasus contributor values improved searches and reusable “Link Folder” results. Historical speed figures in that review are not comparable with 2026 hardware. [Overview](https://www.pmail.com/overviews/ovw_winpmail.htm), [User review](https://www.crn.com/features/channel-programs/187202508/beyond-outlook-five-alternative-e-mail-apps) |
| **Becky! Internet Mail** | 1. Multiple mailbox profiles. 2. Conditional/regex filters, including creating a rule from a dragged message. 3. Templates. 4. Reminders that return mail or initiate composition. 5. Portable-media operation and synchronization. | Useful workflow primitives without requiring a separate task manager. The Japanese and English vendor pages document these capabilities; their popularity ordering is **unverified**. [English overview](https://www.rimarts.co.jp/becky.htm), [Japanese feature description](https://www.rimarts.co.jp/becky-j.htm) |
| **Outlook Express** | 1. Combined email and NNTP reading. 2. Watch/ignore thread controls. 3. Message rules. 4. HTML stationery and separate user identities. | A comprehensible personal communications workspace. Microsoft’s contemporary IE5 deployment guide documents the features, including separate settings and contacts for identities. It explicitly cautions that identities are not a security boundary. Do not confuse this product with present-day Outlook. [Microsoft guide, Outlook Express section](https://download.microsoft.com/download/A/6/0/A609121D-93BF-4168-9887-7595A7B7D9A9/IE5_en-us.pdf) |
| **Netscape Messenger / Communicator** | 1. Browser/mail/news integration. 2. Offline mail and newsgroup reading. 3. An Outbox for later sending. 4. Profiles and rich HTML composition. | Communications shared the browser’s workspace and could remain usable disconnected. Preserved Netscape help documents offline behavior and notes platform differences; this is historical documentation, not current Mozilla behavior. [Vendor help mirror](https://archive.cs.rutgers.edu/archive/a/solaris/solaris/local/netscape-407/nethelp/netscape/home/home.htm), [Offline help mirror](https://www.cs.vu.nl/~eliens/imm/%40archive/online/archive/netscape-v4.05j/nethelp/netscape/navigatr/nav.htm) |
| **Claws Mail** | 1. Custom “Actions” invoking external commands. 2. Dynamic templates. 3. Per-folder processing distinct from arrival filters. 4. Selective quotation when replying. | Small, composable operations support specialized workflows. A **2019** HN participant specifically values its speed and file-based mail handling; the manual provides stronger feature evidence than that anecdote. [Manual](https://www.claws-mail.org/manual/claws-mail-manual.pdf), [Discussion](https://news.ycombinator.com/item?id=19712437) |
| **PocoMail** | 1. PocoScript for conditional message processing. 2. Simpler graphical filters alongside scripting. 3. A Console for deciding which incoming mail deserves attention. | A firsthand CRN contributor values these controls, but also reports eventually leaving because the complexity exceeded their needs. That is an unusually useful counterexample to feature accumulation. Current availability and macOS compatibility are **unverified**. [Firsthand PocoMail account](https://www.crn.com/features/channel-programs/187202508/beyond-outlook-five-alternative-e-mail-apps) |

### Client survey: Mac traditions and project context

| Client | Features selected | Why they matter; evidence |
|---|---|---|
| **PowerMail** | 1. Indexed, relevance-oriented search. 2. Multilingual mail handling. 3. AppleScript-driven processing. 4. Labels and configurable filtering. | Kirk McElhearn’s **2000-05-08** migration review values search and language support while identifying search-condition limitations. Later vendor release notes document FoxTrot search integration and Spotlight/Quick Look support; those belong to a later generation, not necessarily the reviewed 2000 version. [Review](https://tidbits.com/2000/05/08/migrating-to-new-climes-with-powermail/), [Release notes](https://services.ctmdev.com/VersionChecking/PowerMail/rnotes.html) |
| **Microsoft Entourage** | 1. Mail alongside contacts, calendar, tasks, and notes. 2. Links between messages, notes, and other material. 3. Project Center collecting related mail, contacts, tasks, and files. | Reviews from **2000-10-09** and **2004-06-14** describe correspondence in the context of a project rather than an isolated inbox. Project Center is a compelling reference for linked context, without implying that Sweetbriar should build an entire office suite. [Original tour](https://tidbits.com/2000/10/09/entourage-the-grand-tour/), [2004 review](https://tidbits.com/2004/06/14/entourage-2004-important-incremental-changes/) |
| **Lotus Notes** | 1. Local replicas and offline work with later synchronization. 2. Document/application links inside correspondence. 3. Shared application context beyond ordinary mail. | Replication and linked records are the transferable strengths. Historical IBM documentation explicitly describes offline updates and conflict handling; HCL documents their continuing lineage. A **2026-09-09** discussion praises disconnected work and application context while criticizing UI complexity. Replication should not be romanticized as conflict-free. [IBM Notes 8.5 documentation](https://www.ibm.com/docs/en/notes/8.5.3?topic=sidebar-working-offline), [Local replicas](https://help.hcl-software.com/notes/14.5.1/client/rep_rep.html), [Document links](https://help.hcl-software.com/traveler/12.0.2/ConvertingNotesDocumentLinksToWebLinks.html), [Opposing user experiences](https://www.osnews.com/story/145936/lotus-notes-and-the-dangers-of-starting-from-scratch/) |
| **NeXTMail** | 1. Text, graphics, sound, and file/folder attachments. 2. Integrated voice recording/editing. 3. **Every draft save retains another version.** 4. Search followed by “Focus” on matching messages. 5. Custom outgoing and displayed headers. | The **1994** NeXTSTEP guide describes mail as a native document application. Printed pp. 206–207 explicitly explain restoring earlier draft versions; pp. 222–223 explain progressively focusing search results. The guide also distinguishes NeXTSTEP, MIME, and plain-text delivery formats. [Original user guide](http://bitsavers.informatik.uni-stuttgart.de/pdf/next/NeXTSTEP_User_Guide_1994.pdf) |
| **Early Apple Mail, especially the Tiger era** | 1. Spotlight-backed Smart Mailboxes. 2. Search-based organization. 3. Convenient photo-attachment slideshows. 4. An ecosystem of scripting and workflow plug-ins. | Apple’s **2004-06-28** Tiger preview introduced Smart Mailboxes; contemporary TidBITS coverage documents attachment handling and users’ automation practices. These features belong to an evolving ecosystem, not all to Mail’s first release. [Tiger preview](https://www.apple.com/newsroom/2004/06/28Apple-Previews-Mac-OS-X-Tiger/), [2005 coverage](https://tidbits.com/2005/12/05/take-control-news05-dec-05/), [2006 workflow account](https://tidbits.com/2006/07/31/getting-things-done-with-your-macintosh-part-2/) |

### Client survey: redesigned inboxes and surviving Mac power tools

| Client | Features selected | Why people valued them; evidence |
|---|---|---|
| **Opera M2 / Opera Mail** | 1. Non-exclusive views instead of compulsory filing. 2. Full-text search. 3. Automatic mailing-list/contact organization. 4. Follow/ignore conversations. 5. Separate “unseen” and “unread” concepts. | Users could find the same message through different contexts without duplicating it. Opera’s archived history documents search and follow/ignore changes; Vivaldi’s account explains the database/view model and its continuation. [Opera history](https://help.opera.com/en/operas-archived-history/), [View semantics](https://stuff.mit.edu/afs/sipb/project/opera/share/opera/help/m2.html), [Vivaldi’s reconstruction](https://vivaldi.com/blog/vivaldi-mail-technical-preview/) |
| **Sparrow** | 1. Compact, restrained Mac presentation. 2. Gmail-oriented labels and shortcuts. 3. Quick reply. 4. Aliases and per-identity signatures. | A firsthand enthusiast’s **2011-02-09** account values elegance and minimalism; an AnandTech review documents keyboard and identity behavior. This appeal coexists with TidBITS’ criticism of performance and rough edges. [Enthusiast account](https://www.donaldjenkins.com/sparrow-mail-the-most-elegant-powerful-and-minimalist-mac-email-client/), [Review](https://www.anandtech.com/show/4249/sparrow-an-os-x-imap-client-for-gmail-users/2), [Contrary experience](https://tidbits.com/2012/07/21/sparrow-bought-by-google/) |
| **Mailbox** | 1. Short/long swipes with distinct actions. 2. Clear visual feedback during gestures. 3. “Later” presets and custom timed snoozes. 4. Automatic swipe rules learned from repeated actions. | Its central achievement was low-friction triage. TidBITS’ **2013-02-22** review explains the gesture design while identifying limitations; Dropbox’s **2014-04-09** announcement documents Auto-swipe. [Review](https://tidbits.com/2013/02/22/mailbox-for-iphone-eases-email-triage-but-lacks-key-features/), [Auto-swipe](https://blog.dropbox.com/topics/company/a-home-for-life) |
| **Google Inbox** | 1. Customizable bundles. 2. Highlights exposing useful information without opening mail. 3. Reminders interleaved with mail. 4. Assists supplying information needed for a task. 5. Time/place-based snooze. | The combination treated incoming mail as actionable material. Google’s **2014-10-22** introduction documents these features; a **2019** replacement discussion repeatedly asks for bundles and pinning. [Launch](https://blog.google/products-and-platforms/products/gmail/an-inbox-that-works-for-you/), [Users’ requests](https://news.ycombinator.com/item?id=19705440) |
| **Postbox** | 1. Quick Bar for move/copy/tag/switch. 2. Focus Pane and account groups. 3. Topics propagated through conversations. 4. Attachment/image search and compose reuse. 5. A tabbed workspace. | A polished collection of power features in one application. The retained vendor feature page documents these functions; Mac users specifically discuss missing keyboard filing and tabbed workspaces after considering replacements. [Features](https://www.postbox-inc.com/features), [User discussion](https://www.reddit.com/r/macapps/comments/16dl70t) |
| **MailMate** | 1. Keyboard-driven navigation/actions. 2. Plain-text/Markdown composition. 3. Advanced smart mailboxes and header queries. 4. Drill-down search and correspondence/thread views. 5. External-editor and command integration. | A living reference rather than simply a vanished classic. Its manual documents these mechanisms; HN and MPU participants describe them as central to their work. Advanced configuration also has a learning cost. [Manual](https://manual.mailmate-app.com/), [HN](https://news.ycombinator.com/item?id=18057643), [MPU](https://talk.macpowerusers.com/t/539-back-to-email/17645) |
| **Mail Act-On** | 1. Keystroke-triggered rules. 2. Fast move/copy selection. 3. Outgoing-mail rules and filing. 4. Undoable processing and reply templates. | Joe Kissell’s **2008-10-19** review especially values rapid processing and recovery from mistaken filing. These were integrated into Apple Mail’s working context. [Review](https://tidbits.com/2008/10/19/mail-act-on-2-supercharges-mail-rules/), [Manual](https://smallcubed.com/support/mao/manual) |
| **MailTags** | 1. Keywords. 2. Projects. 3. Notes. 4. Review/due-date metadata. | Mail could retain the user’s interpretation and next action without becoming a task in another application. TidBITS workflow accounts describe dependence on that metadata; SmallCubed documents the plug-in family. [Workflow](https://tidbits.com/2006/07/31/getting-things-done-with-your-macintosh-part-2/), [Vendor material](https://sc15.smallcubed.com/support/pk/presskit.html), [Reader accounts](https://tidbits.com/2017/01/19/73-mac-automation-stories-from-tidbits-readers/) |
| **Haiku Mail, in the BeOS tradition** | 1. Messages exposed as files. 2. Tracker queries serving as mail views. 3. Separate mail delivery, reading, and People contact tools. | A useful additional reference for operating-system composability: the filesystem and queries participate in the mail workflow. Haiku’s guide documents the current design; this alone does not establish which historical BeOS feature users loved most. [Mail workshop](https://i18n.haiku-os.org/userguide/data/export/docs/userguide/pl/workshop-email.html), [Mail application guide](https://i18n.haiku-os.org/userguide/data/export/docs/userguide/pl/applications/mail.html) |

### What people say they miss

This is a purposive sample of feature-rich discussions, **not a representative survey**. Separate reports of preferences from confirmed product capabilities or reproducible defects.

| Discussion and date | Recurring request or observation | Implication for Sweetbriar |
|---|---|---|
| [HN: best email client](https://news.ycombinator.com/item?id=18057643), **2018-09-24** | MailMate advocates value search, Markdown, keyboard operation, and standards-oriented mail handling. | People can accept a plain interface when its operations fit their work. |
| [HN: desktop email clients](https://news.ycombinator.com/item?id=19712437), **2019-04-21** | Reports include slow or incomplete searches, state lost during rebuilding, and terminal-client setup burdens. Other participants prefer Apple Mail or webmail. | Reliability and easy setup matter alongside customization. Individual complaints are not measured failure rates. |
| [HN: Inbox replacement](https://news.ycombinator.com/item?id=19705440), **2019-04-20** | Bundles, pinning, and batch handling recur; merely copying appearance does not satisfy those requests. | Recreate the workflow semantics, then design the surface. |
| [MPU: dream GTD mail app](https://talk.macpowerusers.com/t/my-dream-gtd-email-app/11642), **2019-04-06** | Participants want active correspondence, status, and usable links between mail and other tools. | A conversation’s lifecycle extends past an unread flag. |
| [HN: Opera M2](https://news.ycombinator.com/item?id=26798148), **2021-04-14** | Nostalgia centers on search, mailing-list/contact views, reading control, and keyboard use. A dissenting participant prefers a focused browser. | Preserve M2’s organization without requiring a browser suite. |
| [HN: choosing email software](https://news.ycombinator.com/item?id=31238233), **2022-05-02** | Users discuss local copies, plaintext/Maildir portability, and independence from providers. | Explicit local ownership and a practical exit path are product features. |
| [r/macapps: simple mail apps](https://www.reddit.com/r/macapps/comments/1e41kfq/dearth_of_simple_email_apps/), **2024**; [Postbox replacement discussion](https://www.reddit.com/r/macapps/comments/1gax8wj/email_client_again/), **2024** | Sparrow’s restraint is still remembered; replacing Postbox means relearning a familiar workspace. | Avoid forcing every user through a dense expert interface. |
| [r/macapps: what is wrong with native Mail?](https://www.reddit.com/r/macapps/comments/1j5kifd/genuine_question_whats_wrong_with_the_native_mail/), **2025** | Search, keyboard filing, and former plug-in workflows draw complaints; others are satisfied with built-in Mail. | The target audience is a segment with unmet needs, not all Mac users. |
| [MPU: Apple Mail/Siri AI search](https://talk.macpowerusers.com/t/apple-mail-siri-ai-search-is-it-really-this-bad/47013), **2026-09-22** | One MailMate migrant reports failed retrieval and group completion. Another reports those functions working and improved search. | The macOS 27 baseline needs repeatable comparative tests. This thread does not prove a universal Apple Mail defect. |

A further **2026-09-09–10** Notes discussion is a useful caution: contributors value offline applications and cross-site engineering work, while others remember a difficult interface and unnecessary complexity for ordinary email. Sweetbriar should borrow the preservation/context mechanisms without assuming users want an application platform. [OSnews discussion](https://www.osnews.com/story/145936/lotus-notes-and-the-dangers-of-starting-from-scratch/)

### Why features disappeared: evidence versus inference

**Documented product decisions:** Google acquired Sparrow in **July 2012**, and feature development stopped. Dropbox’s **2015-12-07** announcement scheduled Mailbox’s shutdown for **2016-02-26**, explaining a shift toward collaboration. Google’s **2018-09-12** Inbox announcement explained consolidation into Gmail and listed features being transferred. Postbox announced its acquisition on **2024-10-22**; its official notice says development/sales ended and support ran through **2024-12-22**. As checked **2026-10-11**, the retained Postbox site still states that it is no longer developed or available for purchase. [Sparrow](https://tidbits.com/2012/07/21/sparrow-bought-by-google/), [Mailbox](https://blog.dropbox.com/topics/company/saying-goodbye-to-carousel-and-mailbox), [Inbox](https://blog.google/products-and-platforms/products/gmail/inbox-signing-find-your-favorite-features-new-gmail/), [Postbox announcement archive](https://www.postbox-inc.com/blog/category/news), [Postbox notice](https://support.postbox-inc.com/hc/en-us/articles/26988085310103-Postbox-has-been-acquired-by-eM-Client)

**Documented platform break:** SmallCubed’s old Mail plug-ins depended on a host integration that Sonoma no longer accepts. MailMaven’s own documentation now describes keywords, projects, notes, review dates, expected replies, alternate subjects, keyboard processing, and outgoing rules. Thus the workflow survived by moving into a dedicated client, although this research did not test migration fidelity or macOS 27 compatibility. [Legacy support](https://smallcubed.com/support/), [MailMaven organization](https://mailmaven.app/support/gtk/organization.html), [Processing](https://mailmaven.app/support/gtk/processing.html)

**Inference, not established universal causes:** supporting many expert workflows creates UI and maintenance costs; provider differences complicate portable metadata; mobile interaction favors fewer visible controls; integrated office products and simple webmail serve different audiences. These are plausible explanations, not proof that an industry-wide decline was inevitable. Continuing Gnus, Mutt/NeoMutt, MailMate, Vivaldi, and SmallCubed-derived workflows are counterexamples to wholesale extinction.

### Ranked catalog of lost, weakened, or fragmented features

The ranking is **my recommendation**, weighted toward Sweetbriar’s stated priorities: retrieval, safety, responsiveness, cross-platform reuse, and everyday usefulness. It is not a count of forum mentions. All present-day equivalents are documented offerings checked **2026-10-11**, with runtime compatibility untested.

#### 1. Search you can trust and inspect

**Description and antecedents:** Full-corpus retrieval, field conditions, and rapid narrowing mattered in PowerMail, Opera M2, and MailMate. MailMate separately documents quoted/unquoted body search, Boolean conditions, attachment-name queries, and correspondence expansion. [PowerMail review](https://tidbits.com/2000/05/08/migrating-to-new-climes-with-powermail/), [MailMate search](https://manual.mailmate-app.com/view.html)

**Why weakened; equivalents:** Complaints concern missing results and opaque indexing. Their general cause is **unverified**; partial downloads and multiple indexes are engineering hypotheses to investigate, not an established explanation for every complaint. Apple advertises improvements in 27; Vivaldi documents explicit search scopes and conditions. Neither source establishes comparative completeness. [2026 opposing user reports](https://talk.macpowerusers.com/t/apple-mail-siri-ai-search-is-it-really-this-bad/47013), [Vivaldi search](https://help.vivaldi.com/mail/mail-get-started/search-messages-in-vivaldi-mail/)

**Sweetbriar revival:** Make exact search authoritative. Show account/folder scope, downloaded-body coverage, excluded areas, and indexing status. Offer a query language and matching graphical builder, highlighted evidence, stable sorting, and multilingual test corpora. For example, proposed syntax `from:finance@example.com after:2026-01-01 tag:renewal` should describe the same predicate whether typed or assembled visually. Optional semantic results must remain distinguishable from literal matches.

#### 2. Keyboard filing and composable batch actions

**Description and antecedents:** Postbox’s Quick Bar, Mail Act-On rules, and Mutt macros reduce a repeated operation to selecting messages, typing a destination/action, and confirming. [Postbox](https://www.postbox-inc.com/features), [Mail Act-On](https://smallcubed.com/support/mao/manual), [Mutt](http://www.mutt.org/doc/manual/)

**Why weakened; equivalents:** Postbox retirement and SmallCubed’s host-API break are documented losses. A universal retreat from keyboard workflows is **unverified**: MailMaven documents keyboard processing, and MailMate supports multi-action bindings. [MailMaven](https://mailmaven.app/support/gtk/processing.html), [MailMate bindings](https://manual.mailmate-app.com/custom_key_bindings)

**Sweetbriar revival:** Provide a command palette, fuzzy folder selection with account-qualified names, customizable shortcuts, batch operations, and an immediately visible result. Show the destination before execution and support undo where restoration is possible. Keep every command available through accessible menus.

#### 3. Overlapping live views instead of compulsory filing

**Description and antecedents:** Opera M2’s non-exclusive views, The Bat!’s virtual folders, Gnus search groups, and MailMate smart mailboxes let one message appear wherever relevant. [Opera semantics](https://stuff.mit.edu/afs/sipb/project/opera/share/opera/help/m2.html), [The Bat!](https://www.ritlabs.com/en/support/help/63/), [MailMate organization](https://manual.mailmate-app.com/organize.html)

**Why weakened; equivalents:** This is principally **fragmentation, not extinction**. Apple Mail Smart Mailboxes, Thunderbird Saved Searches, and Vivaldi filters all provide live selections. My hypothesis is that folder-based presentation is easier to explain, although that has not been established as the historical cause. Thunderbird’s documentation explicitly distinguishes deleting a view from deleting messages displayed through it. [Apple](https://support.apple.com/en-ie/guide/mail/mlhlp1190/16.0/mac/27), [Thunderbird](https://support.mozilla.org/en-US/kb/using-saved-searches), [Vivaldi](https://help.vivaldi.com/mail/mail-advanced/mail-filters/)

**Sweetbriar revival:** Use one query model for searches and saved views. Support cross-account views, nested conditions, counts, and per-view columns. Preserve real backend folders. Label “delete view,” “remove label,” and “delete message” distinctly; destructive actions must reveal their actual scope.

#### 4. Independent local archives and recoverable drafts

**Description and antecedents:** Mutt supports local mailbox formats; Notes exposes offline replicas; NeXTMail retained successive draft saves. The common value is continuity and inspectable local state, although replication, caching, and independent preservation are different operations. [Mutt](http://www.mutt.org/doc/manual/), [Notes replication](https://help.hcl-software.com/notes/14.5.1/client/rep_rep.html), [NeXTMail drafts, printed pp. 206–207](http://bitsavers.informatik.uni-stuttgart.de/pdf/next/NeXTSTEP_User_Guide_1994.pdf)

**Why weakened; equivalents:** Provider-independent copies remain a stated user preference. A plausible design pressure is prioritizing a synchronized provider mirror over a separately managed archive; that causal explanation is inference. Apple Mail supports local mailboxes; MailMate documents internal EML storage. Local files alone do not establish a server-independent archive or a complete backup. [User requests](https://news.ycombinator.com/item?id=31238233), [Apple local mailboxes](https://support.apple.com/en-nz/guide/mail/mlhlp1021/mac), [MailMate storage](https://manual.mailmate-app.com/preferences.html)

**Sweetbriar revival:** Distinguish “available offline” from “preserve an independent copy.” Retain immutable original messages, versioned drafts, attachment availability records, and exportable metadata. Offer EML/Maildir exports with a manifest, checksums, and a tested restore path. A remote deletion should be explained and reconciled according to the chosen retention policy.

#### 5. Custom bundles with batch completion

**Description and antecedents:** Inbox grouped related incoming material into actionable bundles; Opera offered overlapping organization. The desirable unit is a coherent set of work, not just a folder name. [Inbox introduction](https://blog.google/products-and-platforms/products/gmail/an-inbox-that-works-for-you/), [Users seeking its replacement](https://news.ycombinator.com/item?id=19705440)

**Why lost; equivalents:** Google documented consolidation into Gmail. Today’s Gmail categories are fixed, and its help explicitly says custom categories cannot be created. HEY bundles by individual sender, not multiple senders or subject; Apple Mail’s four categories are also a different mechanism. [Google sunset rationale](https://blog.google/products-and-platforms/products/gmail/inbox-signing-find-your-favorite-features-new-gmail/), [Gmail limits](https://support.google.com/mail/answer/3094499?co=GENIE.Platform%3DDesktop&hl=en-NP), [HEY limits](https://help.hey.com/article/765-bundle-emails), [Apple categories](https://support.apple.com/en-ie/guide/mail/welcome/mac)

**Sweetbriar revival:** Let users define bundles by query, project, list, or sender set. Offer collapsed summaries, optional review schedules, and “archive reviewed messages” with preview and undo. Protect pinned or unresolved items. Start with deterministic rules; avoid silent, uninspectable classification.

#### 6. Private annotations attached to correspondence

**Description and antecedents:** MailTags provided keywords, projects, notes, and dates; Becky also documents per-message notes; Entourage linked messages to contextual material. This makes interpretation persistent: “waiting for finance,” “renewal,” or a clearer local subject. [MailTags workflow](https://tidbits.com/2006/07/31/getting-things-done-with-your-macintosh-part-2/), [Becky Japanese documentation](https://www.rimarts.co.jp/becky-j.htm), [Entourage](https://tidbits.com/2000/10/09/entourage-the-grand-tour/)

**Why fragmented; equivalents:** The legacy Mail plug-in break is documented. MailMaven now documents notes, projects, review dates, expected replies, and alternate subjects, among other fields. IMAP keyword support is not equivalent to transporting arbitrary notes or project records. [SmallCubed](https://smallcubed.com/support/), [MailMaven metadata](https://mailmaven.app/support/gtk/organization.html), [MailMate’s keyword limitations](https://manual.mailmate-app.com/organize.html)

**Sweetbriar revival:** Store versioned private metadata in the shared Rust model with export/import. Display alternate subjects alongside the original when needed. Do not rewrite received messages or inject private annotations into outgoing headers by default. Decide cross-device synchronization explicitly.

#### 7. Explainable rules across the message lifecycle

**Description and antecedents:** The Bat! distinguishes arrival, outgoing, read, and replied events; Pegasus and Becky offer rich filtering; Mulberry exposes server-side Sieve. [The Bat! rule categories](https://www.ritlabs.com/en/support/help/64/), [Pegasus](https://www.pmail.com/overviews/ovw_winpmail.htm), [Mulberry Sieve instructions](https://nlp.stanford.edu/~manning/mobile/mulberry-sieve.shtml)

**Why fragmented; equivalents:** MailMaven documents inbox, outgoing, and keystroke rules. Different execution locations remain materially different: Sieve is a delivery-time filtering language, and ManageSieve manages server scripts. Availability on a particular provider must be negotiated, not assumed. [MailMaven processing](https://mailmaven.app/support/gtk/processing.html), [RFC 5228](https://datatracker.ietf.org/doc/html/rfc5228), [RFC 5804](https://datatracker.ietf.org/doc/html/rfc5804)

**Sweetbriar revival:** Give rules a visible trigger, scope, execution location, and history. Offer dry-run previews, conflict explanations, “why did this happen?”, and exportable definitions. A Sieve editor should preserve unsupported syntax or disclose that round-tripping is incomplete. Local rules must clearly say when the app needs to run.

#### 8. Follow-up states, not just snooze

**Description and antecedents:** Mailbox’s “Later,” Inbox reminders, Becky reminders/Agent tasks, and MailTags dates represent different states: deferred reading, a task, or an expected response. [Mailbox](https://tidbits.com/2013/02/22/mailbox-for-iphone-eases-email-triage-but-lacks-key-features/), [Becky reminders and Agent tasks](https://www.rimarts.co.jp/becky-j.htm), [MailTags](https://sc15.smallcubed.com/support/pk/presskit.html)

**Why weakened; equivalents:** Some functions transferred from Inbox to Gmail; Apple documents Remind Me, while MailMaven separately documents expected replies. Their existence does not make these states interchangeable. Mailbox and Inbox shutdowns removed particular integrated workflows; the general burden of maintaining deadlines and reply state is an engineering inference. [Google’s transferred features](https://blog.google/products-and-platforms/products/gmail/inbox-signing-find-your-favorite-features-new-gmail/), [Apple Remind Me](https://support.apple.com/en-ph/guide/mail/mlhl96dfe8ce/mac), [MailMaven](https://mailmaven.app/support/gtk/organization.html)

**Sweetbriar revival:** Model “read later,” “reply by,” and “waiting for reply” separately. Persist deadlines and resolve reply matching conservatively. Explain whether a reminder hides mail, moves it, or only resurfaces it. Handle sleep, offline operation, time-zone changes, and missed deadlines without discarding the original state.

#### 9. Composition roles and identity safeguards

**Description and antecedents:** Alpine roles, Mulberry identities, Pegasus identities, and Mutt hooks connect context to the sending identity, signature, template, or filing destination. [Alpine](https://alpineapp.email/alpine/alpine-info/roles/), [Mulberry](http://www.mulberrymail.com/downloads/Mulberry/docs/ReferenceMac.pdf), [Pegasus](https://www.pmail.com/overviews/ovw_winpmail.htm)

**Why fragmented; equivalents:** Identity support persists. MailMate documents choosing From based on context and prior correspondence; Postbox documented account-specific domain warnings. Missing or simplified controls are a product-specific issue, not a verified universal disappearance. [MailMate composition preferences](https://manual.mailmate-app.com/preferences.html), [Postbox domain fencing](https://www.postbox-inc.com/features)

**Sweetbriar revival:** Offer optional rules suggesting identity, signature, template, and sent-copy location. Keep the chosen From/account visible and changeable. Warn on personal/work mismatches; validate sending authority against backend capabilities. Do not reproduce Alpine’s documented inability to change a role once composition starts.

#### 10. Navigable thread trees and correspondence context

**Description and antecedents:** Gnus and Mutt make branching discussions navigable. MailMate adds thread arcs and correspondence expansion across mailbox boundaries. [Gnus](https://www.gnu.org/software/emacs/manual/html_mono/gnus.html), [Mutt](http://www.mutt.org/doc/manual/), [MailMate views](https://manual.mailmate-app.com/view.html)

**Why fragmented; equivalents:** Conversation views are common in the surveyed products, but MailMate’s tree/arcs show that a flat conversation is not the only presentation. The proposition that flattening universally displaced trees is **unverified**. Lower visual complexity is a plausible reason to prefer a flat view, but not a documented universal cause.

**Sweetbriar revival:** Offer optional flat, conversation, and tree views; keyboard subtree selection; and inclusion of related sent/archive messages. Use Message-ID, References, and In-Reply-To evidence, preserve uncertainty, and allow reversible manual grouping. Distinguish search matches from surrounding context. Those headers are defined by [RFC 5322](https://datatracker.ietf.org/doc/html/rfc5322).

#### 11. A controllable composer: plain text, Markdown, templates, selected quotation

**Description and antecedents:** Mutt uses the user’s editor; MailMate turns Markdown into HTML; Claws can reply quoting only selected text. The Bat!, Becky, and Postbox add reusable template workflows. [Mutt](http://www.mutt.org/doc/manual/), [MailMate](https://manual.mailmate-app.com/), [Claws manual](https://www.claws-mail.org/manual/claws-mail-manual.pdf)

**Why fragmented; equivalents:** These features still exist. Rich-business-mail requirements can conflict with strict plain-text editing; Sparrow-like simplicity and expert composer controls also pull in different directions. This is a design trade-off, not a documented general retirement.

**Sweetbriar revival:** Support plain text and Markdown as first-class modes, selected quotation, editable attribution, and an optional external-editor round trip. Provide interoperable HTML output when needed. Preserve incoming originals and attachments, and make sending format visible. A rich-text option should remain possible rather than forcing every user into Markdown.

#### 12. Attachment libraries, bulk operations, and provenance

**Description and antecedents:** Postbox searched documents/images across mail and made them reusable during composition; The Bat! documents saving all attachments in an email. [Postbox attachment tools](https://www.postbox-inc.com/features), [The Bat! features](https://ritlabs.com/en/products/thebat/features.php)

**Why fragmented; equivalents:** Postbox’s retirement removed a particular integrated workflow, not the category. HEY currently advertises an attachment library. MailMate documents Quick Look previews. [HEY features](https://www.hey.com/features/), [MailMate previews](https://manual.mailmate-app.com/preferences.html)

**Sweetbriar revival:** Offer a Files view by filename, type, date, sender, and project, with links back to source messages. Display download completeness, save batches safely through macOS file dialogs, avoid filename collisions, and deduplicate storage without hiding distinct message provenance. Content indexing should state which formats were actually extracted.

#### 13. A calm native workspace with optional density

**Description and antecedents:** Sparrow’s restrained UI contrasts with the configurable desktop workspaces of Pegasus, Mulberry, and Postbox. Different users want different densities. [Sparrow enthusiast](https://www.donaldjenkins.com/sparrow-mail-the-most-elegant-powerful-and-minimalist-mac-email-client/), [Mulberry review](https://www.linux.com/news/mulberry-powerful-multi-platform-email-client/)

**Why fragmented; equivalents:** Sparrow’s acquisition is documented; a general claim that newer interfaces are slower or worse is **unverified**. MailMaven documents multiple viewer windows with independent view/sort settings. [Sparrow acquisition](https://tidbits.com/2012/07/21/sparrow-bought-by-google/), [MailMaven viewers](https://mailmaven.app/support/gtk/main.html)

**Sweetbriar revival:** Begin with a conventional native mail window. Offer compact rows, user-chosen columns, stable keyboard focus, separate message/composer windows, and saved workspace settings. Adapt to macOS 27’s visual conventions while preserving readability. Keep advanced panels closed until requested.

#### 14. Automation and durable message links

**Description and antecedents:** PowerMail’s AppleScript processing, Claws Actions, and MailMate’s command-line tool make mail part of a larger workflow. [PowerMail](https://tidbits.com/2000/05/08/migrating-to-new-climes-with-powermail/), [Claws](https://www.claws-mail.org/manual/claws-mail-manual.pdf), [MailMate emate](https://manual.mailmate-app.com/emate.html)

**Why fragmented; equivalents:** Apple Mail’s legacy plug-in break demonstrates the cost of depending on a host’s undocumented behavior. MailMate still documents command integration and message-oriented URI handling. [SmallCubed support](https://smallcubed.com/support/), [MailMate URI preferences](https://manual.mailmate-app.com/preferences.html)

**Sweetbriar revival:** Define stable message identifiers, local links that survive filing, and a versioned action API shared by menus, shortcuts, automation, and the future Linux UI. Add macOS App Intents as an adapter. External scripts should receive only the data the user’s configured action requires.

#### 15. Mailing-list reading and explicit attention state

**Description and antecedents:** Opera separated unseen/unread and followed or ignored threads; Gnus offered scoring; Becky documents mailing-list management. These distinguish “arrived,” “noticed,” “read,” and “still needs action.” [Vivaldi’s Opera-derived explanation](https://vivaldi.com/blog/vivaldi-mail-technical-preview/), [Gnus scoring](https://www.gnu.org/s/emacs/manual/html_node/gnus/Score-File-Format.html), [Becky](https://www.rimarts.co.jp/becky.htm)

**Why fragmented; equivalents:** MailMate documents automatic mailing-list submailboxes and muting future replies. HEY separates new from previously seen mail. The important difference is how these states affect counts, notifications, and future arrivals. Maintaining private attention state alongside provider flags adds complexity; its role in past product decisions is an inference. [MailMate](https://manual.mailmate-app.com/organize.html), [HEY](https://www.hey.com/features/)

**Sweetbriar revival:** Detect List-Id, provide list views, follow/mute thread controls, and optional separate seen/action states. Keep provider “read” status distinct from private workflow state. If adding learned priorities, display reasons and allow corrections; never make uncertain classification an automatic deletion policy. List-Id is standardized in [RFC 2919](https://datatracker.ietf.org/doc/rfc2919/).

#### 16. Project collections that retain context outside email

**Description and antecedents:** Entourage’s Project Center and Notes document links connected mail to tasks, notes, files, and records. [Entourage 2004](https://tidbits.com/2004/06/14/entourage-2004-important-incremental-changes/), [Notes links](https://help.hcl-software.com/traveler/12.0.2/ConvertingNotesDocumentLinksToWebLinks.html)

**Why fragmented; equivalents:** MailMaven offers project metadata; HEY Collections combines multiple threads on a named page. These reproduce portions of the context model, not every Entourage or Notes function. My inference is that larger PIM/application models are harder to preserve in a focused, provider-neutral client. [MailMaven](https://mailmaven.app/support/gtk/organization.html), [HEY Collections](https://www.hey.com/features/)

**Sweetbriar revival:** Add lightweight collections containing message references, notes, follow-up states, and user-approved file links. Make export preserve those relationships. Defer shared project management, a calendar database, and a replacement for Notes applications: their scope would overwhelm the initial email product.

## 3. Options compared

The judgments in this table are recommendations derived from the documented designs above.

| Product direction | Main benefit | Main cost or limitation | Fit for Sweetbriar |
|---|---|---|---|
| **Conventional mail plus expert controls**, inspired by MailMate/Postbox/Act-On | Familiar setup; powerful retrieval and keyboard processing | Can accumulate obscure preferences and hidden configuration | **Best initial direction**, with advanced features discoverable through commands and optional panels |
| **Views-first organization**, inspired by Opera M2/Gnus | Cross-account context without repeated physical filing | Users must understand views versus server folders and the scope of actions | **Adopt internally and expose gradually** |
| **Action-oriented inbox**, inspired by Inbox/Mailbox | Bundles, deferral, and batch processing reduce repetitive triage | Classification and timers become additional state that must be understandable and recoverable | **Second-stage workflow layer** |
| **Integrated project/PIM environment**, inspired by Entourage/Notes | Correspondence retains rich project context | Much larger synchronization, UI, and portability burden | **Borrow lightweight linking; defer the suite** |

Present-day reference products, as documented **2026-10-11**, constrain claims of novelty:

| Reference | Documented overlap | What remains different from the proposed Sweetbriar design |
|---|---|---|
| **Apple Mail on macOS 27** | Categories, Smart Mailboxes, local mailboxes, Remind Me; advertised improved search | Proposed cross-platform core, inspectable query/coverage model, and integrated private workflow metadata require their own implementation. [Guide](https://support.apple.com/en-ie/guide/mail/welcome/mac), [Local mailboxes](https://support.apple.com/en-nz/guide/mail/mlhlp1021/mac) |
| **Thunderbird** | Cross-account Saved Searches with live results | Saved views themselves are established functionality; Sweetbriar must compete on native interaction, safety, and coherence. [Documentation](https://support.mozilla.org/en-US/kb/using-saved-searches) |
| **MailMate** | Smart mailboxes, Markdown, keyboard bindings, correspondence views, automation | A major design benchmark; broader backends and a more discoverable surface are hypotheses to validate, not proven advantages. [Manual](https://manual.mailmate-app.com/) |
| **MailMaven** | Rich metadata, keyboard processing, inbox/outbox rules, multiple viewers | Many proposed “revivals” already have a documented successor. Its fetched help still carries 2025 candidate-release information; 27 compatibility is **unverified** here. [Organization](https://mailmaven.app/support/gtk/organization.html), [Viewer guide](https://mailmaven.app/support/gtk/main.html) |
| **Vivaldi Mail** | Opera-derived organization, saved filters, explicit search conditions | Provides an important surviving reference; Sweetbriar’s intended standalone native UI is a different product choice. [Filters](https://help.vivaldi.com/mail/mail-advanced/mail-filters/), [Search](https://help.vivaldi.com/mail/mail-get-started/search-messages-in-vivaldi-mail/) |
| **Gmail / HEY** | Fixed categories and transferred Inbox features; sender bundles, collections, notes, and follow-up workflows in HEY | Useful workflow references, with different grouping semantics and service context. They do not establish a provider-neutral native implementation. [Gmail categories](https://support.google.com/mail/answer/3094499?co=GENIE.Platform%3DDesktop&hl=en-NP), [HEY](https://www.hey.com/features/) |

## 4. Recommendations for Sweetbriar

### A. Make preservation and reconstruction the foundation

**Recommendation:** Separate original mail, durable user state, synchronization records, and rebuildable indexes. Version drafts and settings; export messages plus metadata and relationships. Reindexing must never destroy annotations, saved queries, roles, or pending actions.

IMAP identifies messages using mailbox-scoped UIDs with UIDVALIDITY; message sequence numbers are unsuitable as persistent identity. JMAP represents mailbox membership and keywords explicitly. Sweetbriar should retain backend identity while assigning its own stable local identity, rather than assuming all providers share a folder-only model. [IMAP, RFC 9051](https://datatracker.ietf.org/doc/rfc9051/), [JMAP Mail, RFC 8621](https://datatracker.ietf.org/doc/html/rfc8621)

**Trade-off:** Additional storage, synchronization bookkeeping, and migration work. **Strongest counterargument:** duplicating provider state can itself introduce conflicts and false expectations of archival safety. Answer it with explicit retention semantics, recoverable operations, and restore tests. “Never lose data” must be a tested invariant with stated boundaries, not a marketing guarantee.

### B. Share the query and action model in Rust

**Recommendation:** Search, saved views, metadata, workflow states, and action execution belong in the reusable core. Native UIs should render results and issue commands, rather than independently interpreting rules.

A concrete evaluation shortlist, with documentation observed **2026-10-11**, is **rusqlite 0.40.2** for SQLite access, **Tantivy 0.26.2** for indexing, and **mail-parser 0.11.9** for message/MIME parsing. These are observed documentation versions, not validated dependency pins. Evaluate **UniFFI** for Swift bindings; its guide explicitly describes Swift 6 support as partial. [rusqlite](https://docs.rs/rusqlite/latest/rusqlite/), [Tantivy](https://docs.rs/tantivy/latest/tantivy/), [mail-parser](https://docs.rs/mail-parser/latest/mail_parser/), [UniFFI Swift](https://mozilla.github.io/uniffi-rs/latest/swift/overview.html)

SQLite’s documentation distinguishes durability settings; in WAL mode, synchronous FULL adds a synchronization at transaction commit. That supports a durability design but does not prove the whole message store is safe. Original-message writes, metadata commits, indexes, and backups need a coordinated protocol. [SQLite PRAGMA documentation](https://www.sqlite.org/pragma.html?skip_cache=true)

**Trade-off:** A shared model limits platform-specific improvisation. **Strongest counterargument:** a general query/rule framework could become an expensive abstraction before demand is known. Build it around a few shipped workflows; do not implement every classic rule language.

### C. Ship a small, complete first workflow

**Recommendation:** The first product slice should support account setup, reliable sync, durable drafts, exact search, unified views, keyboard move/tag/archive, and understandable undo. Include saved searches and conventional folders. Add notes, roles, and follow-up states only once that foundation is dependable.

**Proposed acceptance targets, not measurements:** evaluate at **100,000** messages and then **1,000,000**, with Japanese/Chinese text, malformed historical messages, duplicate identifiers, attachments, offline accounts, and partial downloads. On declared reference hardware, target visible command feedback within **50 ms** and the first warm exact-search page within **100 ms**, both at the 95th percentile. Track completeness separately from speed.

Run fault scenarios involving interrupted saves, disk-full errors, forced termination, failed remote operations, and backup restoration. A draft marked saved must survive the supported crash scenarios; an operation that failed to persist must remain visibly pending or failed.

**Trade-off:** Less immediate novelty than bundles or AI summaries. **Strongest counterargument:** a reliable conventional client may still lack a compelling reason to switch. Validate the keyboard/search/metadata workflow with actual MailMate, Postbox, and MailTags users before expanding scope.

### D. Be native on macOS without putting the core in Apple APIs

**Recommendation:** Prototype an AppKit table/outline for dense lists and thread navigation, with SwiftUI where it improves settings or secondary surfaces. Compare alternatives on actual data; this report establishes no AppKit-versus-SwiftUI performance result. Treat accessibility, keyboard navigation, drag-and-drop, Quick Look, menus, and multiple windows as acceptance conditions. Apple documents table/outline accessibility attributes. [AppKit accessibility](https://developer.apple.com/documentation/appkit/table-view-and-outline-view-attributes)

Use Core Spotlight as an optional system integration and App Intents for user-invoked actions. Core Spotlight exposes queries over indexed app content; it need not become the only implementation of Sweetbriar’s cross-platform search. Choose explicitly which accounts/content to expose to system search. Verify API availability and appearance on **27 and 26**. [Core Spotlight queries](https://developer.apple.com/documentation/corespotlight/searching-for-information-in-your-app?changes=lat_3_5), [App Intents Spotlight integration](https://developer.apple.com/documentation/appintents/spotlight)

**Trade-off:** Two native surfaces and an additional system index increase work. **Strongest counterargument:** Apple’s search stack could reduce macOS implementation effort substantially. Benchmark it, but retain portable query semantics and a rebuildable source of truth.

### E. Make powerful workflows explainable and optional

**Recommendation:** Add a compact metadata inspector, role suggestions, rule previews/history, and persistent follow-up states before learned classification. Later introduce custom bundles and editable priorities. Every automation should answer what matched, what changed, where it ran, and how to reverse or repair the result.

**Trade-off:** Explanations and previews can slow setup. **Strongest counterargument:** users seeking Mailbox’s speed may find this bureaucratic. Keep common actions immediate; put detailed explanations behind a concise status or history affordance. Require previews primarily for configuring broad or destructive automation.

### F. Preserve the user’s investment beyond the product’s lifetime

**Recommendation:** Publish export formats and a versioned metadata schema; maintain migration tests and readable backups. Prefer stable supported integration points. Provide a practical exit even if Sweetbriar’s business changes.

The acquisitions and shutdowns above demonstrate a continuity risk without establishing that any particular pricing model would prevent it. [Dropbox’s decision](https://blog.dropbox.com/topics/company/saying-goodbye-to-carousel-and-mailbox), [Postbox’s transition](https://support.postbox-inc.com/hc/en-us/articles/26988085310103-Postbox-has-been-acquired-by-eM-Client)

**Trade-off:** Supporting imports, exports, and compatibility consumes ongoing engineering time. **Strongest counterargument:** portability does not fund maintenance. Sustainable funding and documented ownership continuity remain separate project decisions; this report supplies no current price recommendation.

## 5. Risks and open questions

- **Selection bias:** these discussions disproportionately represent enthusiasts with unusual archives and workflows. Interview satisfied Apple Mail/webmail users as well. The sampled threads contain substantial counterevidence to a universal “modern mail is broken” story. [HN, 2019](https://news.ycombinator.com/item?id=19712437), [r/macapps, 2025](https://www.reddit.com/r/macapps/comments/1j5kifd/genuine_question_whats_wrong_with_the_native_mail/)
- **What is “saved”?** Define the boundary among local commit, provider acknowledgment, independent archive, and backup. Draft history without attachment preservation is incomplete. Replication conflicts are a real design concern, illustrated explicitly by historical Notes documentation. [IBM offline/conflict guidance](https://www.ibm.com/docs/en/notes/8.5.3?topic=sidebar-working-offline)
- **Backend capability differences:** build a matrix for Microsoft 365/Exchange, Proton, Gmail API, JMAP, and IMAP covering identity, labels, drafts, offline availability, deletion, and rule execution. This report does **not** verify current authentication policies or retirements for those services. IMAP and JMAP already expose different identity/membership models. [RFC 9051](https://datatracker.ietf.org/doc/rfc9051/), [RFC 8621](https://datatracker.ietf.org/doc/html/rfc8621)
- **Private metadata transport:** which data remains local, which synchronizes, how conflicts resolve, and what an export preserves need explicit decisions. Provider keywords are not a universal notes/project protocol. [MailMate’s documented keyword constraints](https://manual.mailmate-app.com/organize.html)
- **Compatibility evidence ages quickly:** fetching a manual in 2026 does not refresh its claims. MailMaven’s fetched help still advertises a 2025 candidate release and names supported systems only through Tahoe. MailMate documentation is rich, but this research does not establish 27 runtime compatibility for either application or any classic binary. [MailMaven help](https://mailmaven.app/support/gtk/main.html), [MailMate manual](https://manual.mailmate-app.com/)
- **Migration fidelity:** importing EML is insufficient if a user loses tags, rules, identities, projects, draft revisions, or saved views. Obtain consented real-world exports and establish supported import boundaries. Reconstructing proprietary historical formats is a separate engineering investigation.
- **Bundles and attention safety:** misclassification can conceal urgent mail. Test mixed-purpose senders, overlapping projects, unresolved threads, changing list addresses, and time-sensitive messages. Allow straightforward inspection and correction.
- **Snooze while the Mac sleeps:** decide whether deadlines are local notifications, server-side actions, or cross-device workflow state. Do not promise always-on scheduling without an execution mechanism.
- **Feature overload:** the strongest product risk is an omnibus client whose expert features crowd out everyday reading and writing. PocoMail’s firsthand reviewer describes leaving once the complexity no longer matched their needs. [CRN account](https://www.crn.com/features/channel-programs/187202508/beyond-outlook-five-alternative-e-mail-apps)
- **Search superiority is still an open empirical question:** Apple’s 27 release notes and contradictory user reports make a measured corpus comparison necessary. Evaluate exact retrieval, indexing coverage, ranking, latency, and multilingual behavior separately. [Apple’s changes](https://support.apple.com/en-us/127257), [2026 user reports](https://talk.macpowerusers.com/t/apple-mail-siri-ai-search-is-it-really-this-bad/47013)

## 6. Sources

All URLs below were consulted on **2026-10-11**. Mulberry, Mutt, and NeXT documentation required HTTP fallback. Becky’s Japanese page was also fetched locally and decoded as Shift_JIS. Gnus score-file details were available through indexed primary-source text despite direct-fetch failures. Historical publication dates are identified in the report; a fetch date is not a release or compatibility date.

### Apple platforms and Mail history

- [Apple security release dates](https://support.apple.com/en-us/100100)
- [macOS 27 compatibility](https://support.apple.com/en-us/127455)
- [macOS 27 update changes](https://support.apple.com/en-us/127257)
- [Apple Mail 27 guide](https://support.apple.com/en-ie/guide/mail/welcome/mac)
- [Apple Smart Mailboxes/categories](https://support.apple.com/en-ie/guide/mail/mlhlp1190/16.0/mac/27)
- [Apple local mailboxes](https://support.apple.com/en-nz/guide/mail/mlhlp1021/mac)
- [Apple Remind Me](https://support.apple.com/en-ph/guide/mail/mlhl96dfe8ce/mac)
- [Apple MailKit](https://developer.apple.com/documentation/MailKit)
- [Apple Tiger preview, 2004](https://www.apple.com/newsroom/2004/06/28Apple-Previews-Mac-OS-X-Tiger/)
- [TidBITS: Tiger Mail coverage, 2005](https://tidbits.com/2005/12/05/take-control-news05-dec-05/)
- [TidBITS: Mac GTD workflow, 2006](https://tidbits.com/2006/07/31/getting-things-done-with-your-macintosh-part-2/)

### Client manuals, vendor material, and preserved documentation

- [Alpine roles](https://alpineapp.email/alpine/alpine-info/roles/)
- [Mulberry Mac reference manual](http://www.mulberrymail.com/downloads/Mulberry/docs/ReferenceMac.pdf)
- [NeXTSTEP user guide, 1994](http://bitsavers.informatik.uni-stuttgart.de/pdf/next/NeXTSTEP_User_Guide_1994.pdf)
- [Alpine configuration](https://alpineapp.email/alpine/UW/tech-notes/config.html)
- [Alpine interrupted/postponed composition](https://alpineapp.email/alpine/alpine-info/commands/compose.html)
- [Mutt manual](http://www.mutt.org/doc/manual/)
- [NeoMutt feature status](https://neomutt.org/feature)
- [NeoMutt feature documentation](https://docs.neomutt.org/reference/releases/features.html)
- [Gnus manual](https://www.gnu.org/software/emacs/manual/html_mono/gnus.html)
- [Gnus score-file format](https://www.gnu.org/s/emacs/manual/html_node/gnus/Score-File-Format.html)
- [Gnus command reference](https://www.gnu.org/software/emacs/refcards/pdf/gnus-booklet.pdf)
- [The Bat! features](https://ritlabs.com/en/products/thebat/features.php)
- [The Bat! virtual folders](https://www.ritlabs.com/en/support/help/63/)
- [The Bat! color groups](https://www.ritlabs.com/en/support/help/68/)
- [Pegasus Mail overview](https://www.pmail.com/overviews/ovw_winpmail.htm)
- [Becky English overview](https://www.rimarts.co.jp/becky.htm)
- [Becky Japanese feature documentation](https://www.rimarts.co.jp/becky-j.htm)
- [Microsoft IE5 guide: Outlook Express](https://download.microsoft.com/download/A/6/0/A609121D-93BF-4168-9887-7595A7B7D9A9/IE5_en-us.pdf)
- [Preserved Netscape help](https://archive.cs.rutgers.edu/archive/a/solaris/solaris/local/netscape-407/nethelp/netscape/home/home.htm)
- [Preserved Netscape offline help](https://www.cs.vu.nl/~eliens/imm/%40archive/online/archive/netscape-v4.05j/nethelp/netscape/navigatr/nav.htm)
- [Claws Mail manual](https://www.claws-mail.org/manual/claws-mail-manual.pdf)
- [PowerMail release notes](https://services.ctmdev.com/VersionChecking/PowerMail/rnotes.html)
- [IBM Notes offline/conflict documentation](https://www.ibm.com/docs/en/notes/8.5.3?topic=sidebar-working-offline)
- [HCL Notes local replicas](https://help.hcl-software.com/notes/14.5.1/client/rep_rep.html)
- [HCL Notes document links](https://help.hcl-software.com/traveler/12.0.2/ConvertingNotesDocumentLinksToWebLinks.html)
- [Opera archived version history](https://help.opera.com/en/operas-archived-history/)
- [Mail Act-On manual](https://smallcubed.com/support/mao/manual)
- [SmallCubed plug-in material](https://sc15.smallcubed.com/support/pk/presskit.html)
- [Haiku Mail workshop](https://i18n.haiku-os.org/userguide/data/export/docs/userguide/pl/workshop-email.html)
- [Haiku Mail application guide](https://i18n.haiku-os.org/userguide/data/export/docs/userguide/pl/applications/mail.html)
- [The Bat! rule categories](https://www.ritlabs.com/en/support/help/64/)
- [Mulberry Sieve instructions](https://nlp.stanford.edu/~manning/mobile/mulberry-sieve.shtml)

### Firsthand reviews and workflow accounts

- [TidBITS: readers’ automation stories, 2017](https://tidbits.com/2017/01/19/73-mac-automation-stories-from-tidbits-readers/)
- [TidBITS: Entourage 2004](https://tidbits.com/2004/06/14/entourage-2004-important-incremental-changes/)
- [Linux.com: Mulberry review, 2002](https://www.linux.com/news/mulberry-powerful-multi-platform-email-client/)
- [CRN: firsthand alternative-client reviews](https://www.crn.com/features/channel-programs/187202508/beyond-outlook-five-alternative-e-mail-apps)
- [TidBITS: PowerMail migration, 2000](https://tidbits.com/2000/05/08/migrating-to-new-climes-with-powermail/)
- [TidBITS: Entourage tour, 2000](https://tidbits.com/2000/10/09/entourage-the-grand-tour/)
- [Donald Jenkins: Sparrow firsthand account](https://www.donaldjenkins.com/sparrow-mail-the-most-elegant-powerful-and-minimalist-mac-email-client/)
- [AnandTech: Sparrow review](https://www.anandtech.com/show/4249/sparrow-an-os-x-imap-client-for-gmail-users/2)
- [TidBITS: Mailbox review, 2013](https://tidbits.com/2013/02/22/mailbox-for-iphone-eases-email-triage-but-lacks-key-features/)
- [TidBITS: Mail Act-On 2, 2008](https://tidbits.com/2008/10/19/mail-act-on-2-supercharges-mail-rules/)

### Product transitions and redesigned inboxes

- [Vivaldi Mail’s Opera-derived design](https://vivaldi.com/blog/vivaldi-mail-technical-preview/)
- [TidBITS: Sparrow acquired by Google](https://tidbits.com/2012/07/21/sparrow-bought-by-google/)
- [Postbox acquisition and support notice](https://support.postbox-inc.com/hc/en-us/articles/26988085310103-Postbox-has-been-acquired-by-eM-Client)
- [Dropbox: Mailbox shutdown](https://blog.dropbox.com/topics/company/saying-goodbye-to-carousel-and-mailbox)
- [Google: Inbox consolidation](https://blog.google/products-and-platforms/products/gmail/inbox-signing-find-your-favorite-features-new-gmail/)
- [SmallCubed legacy support](https://smallcubed.com/support/)
- [Preserved Opera M2 help](https://stuff.mit.edu/afs/sipb/project/opera/share/opera/help/m2.html)
- [Google: Inbox introduction](https://blog.google/products-and-platforms/products/gmail/an-inbox-that-works-for-you/)
- [HEY sender bundles](https://help.hey.com/article/765-bundle-emails)
- [Dropbox: Mailbox Auto-swipe, 2014](https://blog.dropbox.com/topics/company/a-home-for-life)
- [Postbox retained feature catalog](https://www.postbox-inc.com/features)
- [Postbox announcement archive](https://www.postbox-inc.com/blog/category/news)

### Documented modern equivalents

- [MailMate introduction](https://manual.mailmate-app.com/)
- [MailMaven processing](https://mailmaven.app/support/gtk/processing.html)
- [Gmail category limits](https://support.google.com/mail/answer/3094499?co=GENIE.Platform%3DDesktop&hl=en-NP)
- [MailMaven metadata](https://mailmaven.app/support/gtk/organization.html)
- [MailMate search/thread views](https://manual.mailmate-app.com/view.html)
- [Vivaldi search](https://help.vivaldi.com/mail/mail-get-started/search-messages-in-vivaldi-mail/)
- [MailMate custom key bindings](https://manual.mailmate-app.com/custom_key_bindings)
- [MailMate organization](https://manual.mailmate-app.com/organize.html)
- [Thunderbird Saved Searches](https://support.mozilla.org/en-US/kb/using-saved-searches)
- [Vivaldi filters](https://help.vivaldi.com/mail/mail-advanced/mail-filters/)
- [MailMate storage/composition preferences](https://manual.mailmate-app.com/preferences.html)
- [HEY feature catalog](https://www.hey.com/features/)
- [MailMaven viewer guide](https://mailmaven.app/support/gtk/main.html)
- [MailMate emate command](https://manual.mailmate-app.com/emate.html)

### User discussions

- [HN: favorite email clients, 2018](https://news.ycombinator.com/item?id=18057643)
- [HN: choosing email software, 2022](https://news.ycombinator.com/item?id=31238233)
- [HN: desktop email clients, 2019](https://news.ycombinator.com/item?id=19712437)
- [OSnews: opposing Notes experiences, 2026](https://www.osnews.com/story/145936/lotus-notes-and-the-dangers-of-starting-from-scratch/)
- [HN: Inbox replacement requests, 2019](https://news.ycombinator.com/item?id=19705440)
- [r/macapps: keyboard navigation/filing](https://www.reddit.com/r/macapps/comments/16dl70t)
- [MPU: Back to Email, 2020](https://talk.macpowerusers.com/t/539-back-to-email/17645)
- [MPU: dream GTD mail app, 2019](https://talk.macpowerusers.com/t/my-dream-gtd-email-app/11642)
- [HN: Opera M2 discussion, 2021](https://news.ycombinator.com/item?id=26798148)
- [r/macapps: simple mail apps, 2024](https://www.reddit.com/r/macapps/comments/1e41kfq/dearth_of_simple_email_apps/)
- [r/macapps: Postbox replacements, 2024](https://www.reddit.com/r/macapps/comments/1gax8wj/email_client_again/)
- [r/macapps: native Mail debate, 2025](https://www.reddit.com/r/macapps/comments/1j5kifd/genuine_question_whats_wrong_with_the_native_mail/)
- [MPU: Mail/Siri AI search, September 2026](https://talk.macpowerusers.com/t/apple-mail-siri-ai-search-is-it-really-this-bad/47013)

### Standards and implementation references

- [RFC 8621: JMAP Mail](https://datatracker.ietf.org/doc/html/rfc8621)
- [RFC 5228: Sieve](https://datatracker.ietf.org/doc/html/rfc5228)
- [RFC 5804: ManageSieve](https://datatracker.ietf.org/doc/html/rfc5804)
- [RFC 5322: message format/thread headers](https://datatracker.ietf.org/doc/html/rfc5322)
- [RFC 2919: List-Id](https://datatracker.ietf.org/doc/rfc2919/)
- [RFC 9051: IMAP](https://datatracker.ietf.org/doc/rfc9051/)
- [rusqlite documentation](https://docs.rs/rusqlite/latest/rusqlite/)
- [Tantivy documentation](https://docs.rs/tantivy/latest/tantivy/)
- [mail-parser documentation](https://docs.rs/mail-parser/latest/mail_parser/)
- [UniFFI Swift documentation](https://mozilla.github.io/uniffi-rs/latest/swift/overview.html)
- [SQLite durability pragmas](https://www.sqlite.org/pragma.html?skip_cache=true)
- [AppKit table/outline accessibility](https://developer.apple.com/documentation/appkit/table-view-and-outline-view-attributes)
- [Core Spotlight queries](https://developer.apple.com/documentation/corespotlight/searching-for-information-in-your-app?changes=lat_3_5)
- [App Intents Spotlight integration](https://developer.apple.com/documentation/appintents/spotlight)
