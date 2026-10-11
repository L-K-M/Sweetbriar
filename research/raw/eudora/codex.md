<!-- provenance: helper=codex model=gpt-6.1-sol started=2026-10-11T18:14:20+00:00 exit=0 brief=research/briefs/ -->

# Eudora: research for Sweetbriar

**Research date: 2026-10-11.** This report separates documented features, reviewers’ judgments, users’ recollections, and proposed Sweetbriar behavior. Historical claims apply to the version named; Mac and Windows Eudora were separate implementations. Current documentation was checked on the research date. This is documentary and source research, without running historical clients or testing current-client compatibility.

## 1. TL;DR

- **Revive the workflows.** Eudora made related-message selection, example-based filtering, identities, templates, and actionable search results work together. [Workflow examples](https://tidbits.com/1997/11/17/eudora-tips-tricks/), [search redesign](https://tidbits.com/1999/07/12/eudora-pro-4-2-continues-to-deliver-part-1/)
- **Its closure was a business decision.** Qualcomm’s spokesperson described Eudora as profitable but outside the wireless business when classic development ended on 2006-10-11. [Contemporary report](https://www.linux.com/news/eudora-dead-long-live-eudora/)
- **Classic Eudora and Eudora OSE are different projects.** OSE rebuilt features on Thunderbird; Mozilla now marks it unsupported. [Penelope record](https://wiki.mozilla.org/Penelope)
- **The 2018 source release is useful research material.** Retained notices are required, third-party components were removed, and Mac C and Windows C++ were separate codebases. [License](https://computerhistory.org/blogs/the-eudora-email-client-source-code-license-agreement/), [source description](https://computerhistory.org/blog/the-eudora-email-client-source-code/)
- **Several “lost” features survive.** Gmail has message-derived filters and templates; Thunderbird has saved searches and outgoing filters; Spark and Mimestream have templates. Sweetbriar’s opportunity is coherent behavior across account types. [Gmail](https://support.google.com/mail/answer/6579?hl=en_as), [Thunderbird](https://support.mozilla.org/en-US/kb/organize-your-messages-using-filters), [Spark](https://sparkmailapp.com/help/spark-ai/templates-ai), [Mimestream](https://mimestream.com/)
- **Readable mailboxes do not guarantee complete archives.** Eudora migration encountered attachment and metadata problems. Sweetbriar should preserve content, relationships, and provenance with auditable imports. [Migration account](https://tidbits.com/2011/09/06/converting-email-from-eudora-why-i-no-longer-live-at-the-p-o/)
- **Revival activity continues.** Checked 2026-10-11, HERMES advertises Windows Eudoramail and a Mac Eos release dated 2026-04-02, with substantial Mac platform restrictions. These are vendor claims. [HERMES](https://hermes.cx/)
- **Recommendation:** build the requested Rust core and native Swift interface; prioritize durable drafts, complete archives, exact search, and explicit sync state, then add Eudora-style automation. The strongest objection is rebuilding mature mail infrastructure while established clients already offer many individual features.

## 2. Findings

### 2.1 History, business, code, and people

Steve Dorner began Eudora in **1988** at the University of Illinois at Urbana-Champaign’s computer center. This is a development-start date, not a verified public-release date. [Dorner’s account](https://computerhistory.org/blog/the-eudora-email-client-source-code/)

Qualcomm licensed it in **1991** and released a consumer product in **1993**. It began on Macintosh and subsequently served Windows users. [CHM announcement, 2018-05-22](https://www.globenewswire.com/news-release/2018/05/22/1510151/28639/en/computer-history-museum-makes-the-eudora-email-client-source-code-available-to-the-public.html)

Qualcomm project manager John Noerenberg assigned Jeff Beckley and Jeff Gehlhaar to the PC implementation. Dorner continued remotely from Illinois. The name referred to Eudora Welty’s story *Why I Live at the P.O.* [CHM history](https://computerhistory.org/blog/the-eudora-email-client-source-code/)

The product family included free Eudora and commercial Eudora Pro. [Contemporary book chapter](https://www.tidbits.com/resources/iskm3html/pt4/ch21/ch21a.html) The September 2003 Mac 6 announcement describes **Paid, Sponsored, and Light** modes: advertising-supported Sponsored mode, reduced-feature Light mode, and built-in SpamWatch requiring Paid mode. Those restrictions must accompany feature lists. [Macworld, 2003-09-03](https://www.macworld.com/article/167814/eudora-12.html)

| Milestone | Verified significance |
| --- | --- |
| Mac Light 3.1, 1997 manual | Nicknames, filters, sending/server controls, and extensive shortcuts. [Manual](https://manualzilla.com/doc/5798190/version-3.1-for-macintosh-user-manual) |
| Pro 4.0, January 1998 coverage | Background transfers, improved filter creation, LDAP/ACAP; IMAP was still planned for 4.1. [TidBITS](https://tidbits.com/1998/01/19/eudora-pro-4-0-finalized-2/) |
| Mac Pro 4.2, July 1999 coverage | Search-result lists, inline spelling, previews, speech, quoted-text improvements. [Part 1](https://tidbits.com/1999/07/12/eudora-pro-4-2-continues-to-deliver-part-1/), [Part 2](https://tidbits.com/1999/07/19/eudora-pro-4-2-continues-to-deliver-part-2/) |
| 5.0, September 2000 coverage | MoodWatch, statistics, Scripts menu, Eudora Sharing Protocol. MoodWatch predates 6.0. [TidBITS](https://tidbits.com/2000/09/11/eudora-5-0-reads-your-mind/) |
| Mac 6.0, 2003–2004 coverage | SpamWatch, Content Concentrator, contextual filing, mailbox drawer. [Announcement](https://www.macworld.com/article/167814/eudora-12.html), [review](https://www.macworld.com/article/169149/eudora6.html) |
| Windows 7.0 documentation | Paid-mode X1 search, BossWatch, offline IMAP improvements, S/MIME. [Vendor guide](https://www.stordito.it/web/tutorial/whatsnew_v7.0.pdf) |
| Final Qualcomm releases, 2006-10-11 | Windows **7.1.0.9** and Mac **6.2.4**. [CHM](https://computerhistory.org/blog/the-eudora-email-client-source-code/) |

On **2006-10-11**, Qualcomm and Mozilla announced a Thunderbird-based successor and the final commercial releases. The transition price was **$19.95 with six months’ support**, a historical offer. Dorner welcomed shared infrastructure and an improved display engine. [Original announcement](https://blog.mozilla.org/press/2006/10/qualcomm-launches-project-in-collaboration-with-mozilla-foundation-to-develop-open-source-version-of-eudora-email-program/)

**Why it ended:** Qualcomm spokesperson Jeremy James said Eudora was profitable but did not fit the wireless business. The contemporary report also explains the replacement strategy: incompatible codebases and third-party licensing complicated reuse. Competition and changing customer needs are plausible additional explanations; assigning them quantified causal importance would be speculation. [Linux.com, 2006-10-11](https://www.linux.com/news/eudora-dead-long-live-eudora/)

Original engineers, including Dorner and Beckley, continued in the successor effort; a beta appeared in **August 2007**. [CHM](https://computerhistory.org/blog/the-eudora-email-client-source-code/)

Qualcomm dates OSE’s launch to **September 2010**. [Qualcomm](https://www.qualcomm.com/eudora) Penelope was the Thunderbird-based feature reimplementation, distinct from the later publication of classic source. Mozilla marks OSE unsupported, checked **2026-10-11**. [Mozilla](https://wiki.mozilla.org/Penelope) Every team member’s subsequent career was not established.

CHM announced the classic source release on **2018-05-22**, after five years of negotiations. Qualcomm transferred the code, copyrights, trademarks, and domains to the museum. [Announcement](https://www.globenewswire.com/news-release/2018/05/22/1510151/28639/en/computer-history-museum-makes-the-eudora-email-client-source-code-available-to-the-public.html)

The published license requires retained notices and disclaimers and prohibits unauthorized endorsement. [License](https://computerhistory.org/blogs/the-eudora-email-client-source-code-license-agreement/) CHM separately says the code license grants no Eudora trademark or domain use. [Clarification](https://computerhistory.org/blog/the-eudora-email-client-source-code/)

**Revival status, checked 2026-10-11:** HERMES advertises Windows Eudoramail 8.0 and dates Mac Eos 6.3 to **2026-04-02**. It specifies Xcode 9.4.1 on High Sierra for building and says Eos cannot run on Arm Macs or Catalina and later. These are vendor statements, not independent tests. The Eos repository advertises GPL-3.0 at its root, while a sampled filter header retains CHM’s 2017 notice: inspect rights per component before reuse. [HERMES](https://hermes.cx/), [repository](https://github.com/nmatavka/HERMES-Eos), [header](https://raw.githubusercontent.com/nmatavka/HERMES-Eos/main/V624/Include/filters.h)

The CHM ZIP links were located, but archive retrieval was blocked here, including an HTTP fallback. Mac manuals were downloaded; Windows manuals were read through transcribed mirrors. Eos files were inspected as a later derivative, not an untouched 2006 source release.

### 2.2 Feature inventory

This is a broad documented inventory, not a claim that every release or mode had every feature. Earlier manuals establish historical behavior without proving it remained unchanged later.

#### Mailboxes, selection, state, and storage

| Feature | Documented behavior and boundary |
| --- | --- |
| In / Out / Trash | Incoming mail, composition/queue/sent copies, and discarded messages. Out mixed lifecycle states; keeping sent copies was configurable. [1997 tips](https://tidbits.com/1997/11/17/eudora-tips-tricks/) |
| Message summaries | Mac columns exposed status, priority, attachment, label, correspondent, date, size, server state, subject; several were directly editable. [Mac 6.2 manual](https://web.stanford.edu/dept/its/support/ess/doc_archive/ess/mac/docs/eudora6/eudora62_manual.pdf) |
| Lifecycle and importance | Read/unread, replied, forwarded, redirected, queued/scheduled, sent/error states; five priorities and colored labels. [Mac 6.2 manual](https://web.stanford.edu/dept/its/support/ess/doc_archive/ess/mac/docs/eudora6/eudora62_manual.pdf) |
| Select similar | Mac 3.1 Option-click selected and grouped messages from/to the same correspondent. [Mac Light 3.1](https://manualzilla.com/doc/5798190/version-3.1-for-macintosh-user-manual) |
| Working search results | Results could be sorted and acted on like mailbox contents; scope could include selected mailboxes. [Mac 4.2 review](https://tidbits.com/1999/07/12/eudora-pro-4-2-continues-to-deliver-part-1/) |
| POP server state | Per-message fetch, delete, fetch-and-delete, or no-action intentions; IMAP deletion differed. [Mac 6.2 manual](https://web.stanford.edu/dept/its/support/ess/doc_archive/ess/mac/docs/eudora6/eudora62_manual.pdf) |
| Windows disk layout | Unix-style `.mbx`, `.toc` indexes, `.fol` directories, `lmos.dat` POP state, `filters.pce`, `Eudora.ini`, `nndbase.txt` nicknames, separate attachment/embedded directories. [Windows 7.1, “Eudora Files”](https://manualzilla.com/doc/6879944/eudora%C2%AE-email-7.1-user-guide-for-windows) |
| Mac disk layout | Unix-spool mailboxes; `.mailrc` nicknames; separate settings/backup, filters, signatures, stationery, attachments, MIME parts, IMAP caches. [Mac 4.2, “Eudora Folders”](https://perso.uclouvain.be/jean-pierre.kuypers/logicielTraduit/Eudora/4/manuals/mac42usermanual.pdf) |
| Compaction | Deleted space could remain until compaction; Mac shortcuts compacted one or all mailboxes. [Light 3.1 appendix](https://manualzilla.com/doc/5798190/version-3.1-for-macintosh-user-manual) |

#### Filters, addresses, identities, and composition

| Feature | Documented behavior and boundary |
| --- | --- |
| Make Filter | Created a rule from selected messages: sender, recipient, or subject plus destination. [Mac 6.2 manual](https://web.stanford.edu/dept/its/support/ess/doc_archive/ess/mac/docs/eudora6/eudora62_manual.pdf) |
| Ordered rules | Incoming/outgoing/manual; two terms with AND/OR/UNLESS or an ignored second term; five actions. [Mac 6.2 manual](https://web.stanford.edu/dept/its/support/ess/doc_archive/ess/mac/docs/eudora6/eudora62_manual.pdf) |
| Action vocabulary | Change status, priority, label, identity, subject; sound/speech, open, print, notify, forward, redirect, template reply, server fetch/delete, copy/transfer, move attachments, stop. [Mac 4.2 filter actions](https://perso.uclouvain.be/jean-pierre.kuypers/logicielTraduit/Eudora/4/manuals/mac42usermanual.pdf) |
| Mac stopping behavior | Transfer implicitly stopped filtering. Outgoing rules ran after sending: identity changes affected the stored copy, not transmitted mail. [Mac 6.2 manual](https://web.stanford.edu/dept/its/support/ess/doc_archive/ess/mac/docs/eudora6/eudora62_manual.pdf) |
| Filter maintenance | Mac 6.2 marked rules unused for 30 days with a dinosaur icon. [Mac 6.2](https://web.stanford.edu/dept/its/support/ess/doc_archive/ess/mac/docs/eudora6/eudora62_manual.pdf) |
| Windows stopping behavior | Skip Rest stopped later rules, but remaining actions in the current rule still ran. Notifying an external application was also documented. [Windows 7.1 guide](https://manualzilla.com/doc/6879944/eudora%C2%AE-email-7.1-user-guide-for-windows) |
| Nicknames / Make Nickname | Person/group aliases. The 3.1 command, Make Address Book Entry, gathered addresses from messages or selected text. [Light 3.1](https://manualzilla.com/doc/5798190/version-3.1-for-macintosh-user-manual) |
| Quick recipients | Named recipients in compose/forward/redirect/insertion menus; explicit nickname expansion. [Light 3.1](https://manualzilla.com/doc/5798190/version-3.1-for-macintosh-user-manual) |
| Personalities | Multiple accounts/identities, selected checks, associated signatures/stationery; dragging messages onto a personality initiated a reply as it. [Windows 5.1](https://doczz.net/doc/93993/eudora-5.1-user-manual) |
| Stationery | Saved headers/body for New Message With and Reply With, account defaults, filter-triggered replies; Windows `.sta` files. [Windows 5.1](https://doczz.net/doc/93993/eudora-5.1-user-manual) |
| Send Again | Incoming or outgoing stored mail became a fresh editable composition, useful after a bounce or as a template. [Mac 4.2](https://perso.uclouvain.be/jean-pierre.kuypers/logicielTraduit/Eudora/4/manuals/mac42usermanual.pdf) |
| Incoming-message editing | The Pencil control enabled body editing; the displayed subject could also be changed. [Mac Light 3.1](https://manualzilla.com/doc/5798190/version-3.1-for-macintosh-user-manual) |
| Folder carbon copy | Fcc copied outgoing mail into chosen mailboxes; a preference could file replies beside their originals, excluding In. [Mac 4.2](https://perso.uclouvain.be/jean-pierre.kuypers/logicielTraduit/Eudora/4/manuals/mac42usermanual.pdf) |
| Redirect / Turbo Redirect | Preserved author/reply routing, identified intermediary; Turbo combined recipient choice, redirection, closing, and trashing the source. [1997 tips](https://tidbits.com/1997/11/17/eudora-tips-tricks/) |
| Quoted text | Paste as quotation, quote/unquote, excerpt bars, reflow, and insertion between quoted paragraphs. [Mac 4.2 Part 2](https://tidbits.com/1999/07/19/eudora-pro-4-2-continues-to-deliver-part-2/) |
| Glossary | Command-Option-Comma expanded nicknames into body text, with address-syntax limitations. [1997 tips](https://tidbits.com/1997/11/17/eudora-tips-tricks/) |
| Text / attachments | Plain/rich composition, wrapping, MIME/BinHex/uuencode choices in early releases, signatures, drag-and-drop. [Contemporary book chapter](https://www.tidbits.com/resources/iskm3html/pt4/ch21/ch21a.html) |

#### Search, junk, customization, and dial-up

| Feature | Documented behavior and boundary |
| --- | --- |
| Find versus Search | Within-message finding versus cross-message search. Mac 4.2 added result lists, all/any criteria, regex, multiple search windows, search within results. [1999 Part 1](https://tidbits.com/1999/07/12/eudora-pro-4-2-continues-to-deliver-part-1/) |
| Windows indexed search | 7.0’s X1 Ultra-Fast Search was Paid-only; BossWatch warned about configured recipients; S/MIME handled signing/encryption. [Vendor guide](https://www.stordito.it/web/tutorial/whatsnew_v7.0.pdf) |
| Windows 7.1 additions | Saved indexed searches, recent mailboxes, alerts; searching covered body/headers, attachment names, mailbox, date, status, label, personality. [7.1 guide](https://manualzilla.com/doc/6879944/eudora%C2%AE-email-7.1-user-guide-for-windows) |
| SpamWatch | Server-filter integration and trainable content classification; Junk/Not Junk feedback; plug-in integration for SpamSieve. [Macworld 6 review](https://www.macworld.com/article/169149/eudora6.html) |
| Junk decisions | Adjustable numeric threshold and score conditions; Mac 6.2 processed junk before user rules and documented deletion after 30 days by default. [Mac 6.2](https://web.stanford.edu/dept/its/support/ess/doc_archive/ess/mac/docs/eudora6/eudora62_manual.pdf) |
| MoodWatch | One to three chili peppers for offensive language; Windows 5.1 offered optional ten-minute cooling-off and incoming/stored scanning. [5.1 guide](https://doczz.net/doc/93993/eudora-5.1-user-manual) |
| Content Concentrator | Removed repeated headers and thread clutter from previews. [Macworld](https://www.macworld.com/article/169149/eudora6.html) |
| Contextual filing | Selected text helped choose a matching mailbox name. [Mac 6 announcement](https://www.macworld.com/article/167814/eudora-12.html) |
| Custom toolbar | Command/mailbox buttons, message drops onto mailbox buttons, configurable appearance/function keys. [Mac 4.2](https://perso.uclouvain.be/jean-pierre.kuypers/logicielTraduit/Eudora/4/manuals/mac42usermanual.pdf) |
| EMSAPI plug-ins | Message/resource plug-ins, text conversion, voice attachments; Word Services editor integration. [Mac 4.2](https://perso.uclouvain.be/jean-pierre.kuypers/logicielTraduit/Eudora/4/manuals/mac42usermanual.pdf) |
| Scripts / statistics / ESP | 5.0 added a Scripts menu, usage graphs, and email-based folder-update sharing. ESP exceeded ordinary attachment sending. [2000 review](https://tidbits.com/2000/09/11/eudora-5-0-reads-your-mind/) |
| Advanced preferences | `x-eudora-setting:` links exposed obscure settings; Mac 4.2 autosave existed but defaulted off. [1999 Part 2](https://tidbits.com/1999/07/19/eudora-pro-4-2-continues-to-deliver-part-2/) |
| Offline batches | Immediate send off plus send-on-check grouped outgoing mail; scheduling waited for a connection after the chosen time. [1997 tips](https://tidbits.com/1997/11/17/eudora-tips-tricks/) |
| Download control | POP size limits/partial retrieval and IMAP header/body/attachment choices meant caches could be incomplete. [Windows 7.1](https://manualzilla.com/doc/6879944/eudora%C2%AE-email-7.1-user-guide-for-windows) |
| Connection control | MacSLIP/OT/PPP could disconnect a connection Eudora opened; this was conditional. [1997 tips](https://tidbits.com/1997/11/17/eudora-tips-tricks/) |

Keyboard behavior made these operations repeatable. The Mac Light 3.1 appendix documents **Command-J** for manual filters, **Command-K** for address-book creation, **Command-M** for checking mail, and **Space** for opening/closing or paging messages. Scheduling used **Option-click Queue / Command-Option-E**. Sweetbriar should offer an optional compatible keymap and visible menu equivalents. [3.1 appendix](https://manualzilla.com/doc/5798190/version-3.1-for-macintosh-user-manual), [scheduling](https://tidbits.com/1997/11/17/eudora-tips-tricks/)

Windows 7.1 documented **Ctrl-N** for new mail, **Ctrl-R** for reply, **Ctrl-T** for sending queued mail, and **Shift-Space** for read/unread. **Ctrl-J** meant manual filtering or junking, depending on a preference. [Windows shortcut appendix](https://manualzilla.com/doc/6879944/eudora%C2%AE-email-7.1-user-guide-for-windows)

### 2.3 What users loved

**Contemporary judgment:** Adam Engst’s 1999 review described a preference for “efficiency over appearance.” It disclosed his extensive Eudora use and book-writing involvement: informed advocacy, not independent measurement. [TidBITS, 1999-07-12](https://tidbits.com/1999/07/12/eudora-pro-4-2-continues-to-deliver-part-1/)

**My interpretation:** Eudora made relationships actionable. Sender, label, identity, and subject were visible information and handles for selecting, filing, replying, or automating. Preserve that short path from recognizing a pattern to acting on it.

Engst’s **2004-12-20** report describes searching **33 mailboxes** for unread or specially labeled messages in about **one second**, with Mac 6.2’s live incoming results and retained sorting supporting his workflow. This is a self-reported workload, not a benchmark against 2026 software. Its appeal was a personal attention queue spanning a filing system. [A New Way to Use Eudora](https://tidbits.com/2004/12/20/a-new-way-to-use-eudora/)

Tonya Engst’s **2008-04-13** migration to Apple Mail misses selection by shared metadata and particular search/filing habits. She also explains leaving: her spam-control setup had become frustrating. Preference for the workflow could coexist with a need for better maintenance. [Reluctantly Switching](https://tidbits.com/2008/04/13/reluctantly-switching-from-eudora-to-apple-mail/)

**Later recollection:** on **2017-10-02**, Hacker News user wanderr remembered Eudora as “fast as hell” and praised selection by matching sender or subject; another participant valued its multiwindow spatial workflow. [2017 discussion](https://news.ycombinator.com/item?id=15371882)

The **2018-05-22** discussion includes positive dial-up help-desk recollections, while user psychometry remembered it as “universally reviled” at another help desk. These individual memories establish conflicting experiences, not statistical performance or reliability. [2018 discussion](https://news.ycombinator.com/item?id=17127486)

The attachment is also practical. In a **2023-12-01** HERMES forum post, Peter Donahue describes retaining Windows Eudora to avoid migration risk to historic mail and attachments, while reporting SSL trouble and Windows 10 glitches. His hope for Windows 11 support is not proof of compatibility. Another participant reports mailbox loss and frustration with revival communication. [HERMES discussion](https://sourceforge.net/p/hermesmail/discussion/general/thread/91a33b7b87/)

**Recommendation derived from these accounts:** reproduce the control people miss, and remove the preservation burden that makes them afraid to leave.

### 2.4 Dislikes and limits

Jason Snell’s **2004-01-01** Macworld review praises archive searching but criticizes HTML rendering, dated interface elements, obscure preferences, and restrictive filtering. Its four-action description concerns reviewed 6.0 behavior; the later 6.2 manual documents five. [Review](https://www.macworld.com/article/169149/eudora6.html), [manual](https://web.stanford.edu/dept/its/support/ess/doc_archive/ess/mac/docs/eudora6/eudora62_manual.pdf)

The 1999 review welcomed replacing confusing linear searching with result lists. The 2000 review found the address book a limited contact manager and criticized filtering. These are reasons to study Eudora’s evolution, not assume one perfect release. [1999 review](https://tidbits.com/1999/07/12/eudora-pro-4-2-continues-to-deliver-part-1/), [2000 review](https://tidbits.com/2000/09/11/eudora-5-0-reads-your-mind/)

Adam Engst’s **2011-09-06** migration report describes an approximately million-message archive, attachment and status/label difficulties, and tools reporting **970,283 versus 849,206** converted messages. Those counts do not establish which tool was correct; they show why import needs reconciliation. He also judged Eudora’s IMAP support poor. [Migration report](https://tidbits.com/2011/09/06/converting-email-from-eudora-why-i-no-longer-live-at-the-p-o/)

Later repair tools address Windows UTF-8/HTML and other defects. Their existence establishes repair needs, not that every installation suffered the same failures. [Repair utility](https://github.com/LenShustek/Eudora_fix_mbx)

**Design judgment:** automatic junk deletion and partly downloaded mailboxes should never be mistaken for an archival guarantee. Sweetbriar needs separately specified durability and retention policies.

### 2.5 Lost features versus current clients

This comparison uses documentation checked **2026-10-11**, not hands-on testing of every build. **Unverified** means the research did not establish behavior, not that a product lacks it.

| Client | Verified overlap | Implication for Sweetbriar |
| --- | --- | --- |
| Apple Mail, macOS | Ordered incoming rules, editing/duplication, iCloud rule synchronization; attached script/sound files do not synchronize. A tutorial updated **2026-03-11** documents Send Again. [Apple](https://support.apple.com/en-ie/guide/mail/cpmlprefrules/mac), [tutorial](https://www.idownloadblog.com/2021/03/23/resend-email-mail-send-again/) | Rules and resending survive. Current Redirect availability is **unverified** here: an indexed Apple guide described it, but its live URL redirects to the welcome page. [Affected guide](https://support.apple.com/en-bw/guide/mail/mlhl8b91a034/mac) |
| Outlook | Microsoft documents resending in classic/new Windows Outlook and web workflows. New Outlook requires a primary Microsoft 365 work account; personal accounts lack the option. This source does not establish Mac parity. [Microsoft](https://support.microsoft.com/en-us/outlook/resend-an-email-message-in-outlook) | Avoid treating Outlook as one uniform application. Provide consistently named editable resending across supported accounts. |
| Gmail, desktop web | Filter messages like these, templates, filter-triggered template replies; cited templates are computer-only. [Filters](https://support.google.com/mail/answer/6579?hl=en_as), [templates](https://support.google.com/mail/answer/14864208?hl=en-S) | Make Filter and stationery have mainstream equivalents. Add provider-independent workflows and portable archives. |
| Thunderbird, desktop | Sender-derived rules, incoming/manual/after-sending execution, live saved searches across accounts/folders acting on originals; an extension offers redirection. [Filters](https://support.mozilla.org/en-US/kb/organize-your-messages-using-filters), [saved searches](https://support.mozilla.org/en-US/kb/using-saved-searches?redirectlocale=en-US&redirectslug=what-are-saved-searches), [extension](https://services.addons.thunderbird.net/eN-US/thunderbird/addon/simple-mail-redirection/) | A serious power-user reference. Saved searches, outgoing filters, and redirection did not universally disappear. |
| Spark | Paid templates include recipients, subject, attachments, variables, synchronization; AI filling has optional controls. [Spark](https://sparkmailapp.com/help/spark-ai/templates-ai) | Make templates durable and integrated with identities. Templates alone are insufficient differentiation. |
| Mimestream, macOS | Gmail API, server Gmail filters, aliases, variable templates, multiple windows, Send and Archive. [Vendor](https://mimestream.com/) | A concrete native-Mac reference. Sweetbriar’s broader account coverage is a proposed product distinction. |

**The defensible gap is integration.** Direct selection, visible state, reusable identities, templates, and ordered automation deserve a coherent modern implementation. It is not established that all six clients lack any individual piece. Exact Option-click parity, per-message POP controls, MoodWatch equivalents, and ESP equivalents across all current builds remain **unverified**.

For example: select similar messages; preview a rule; file them; assign a reply identity/template; pin an unanswered view; retain complete local copies. Sweetbriar should make the whole operation discoverable and auditable across providers. This is a recommendation, not a claim about competitors.

### 2.6 Re-imagining for 2026

**Everything in the following table is a design proposal.** The historical antecedents are documented above. “Local” means Rust can implement the interaction or retain the metadata without a provider-specific feature; remote mutations still need an account adapter and acknowledgement.

| Feature to revive | Proposed native macOS interaction | Local core and server requirements |
| --- | --- | --- |
| Summary columns and status | Compact, configurable table; separate badges for draft, queued, submitting, accepted, failed, answered, and locally archived. Explain a badge on demand. | Local lifecycle records; translate supported read/answer/flag states through each adapter. Keep unsupported metadata locally. |
| Select similar | Option-click sender, label, subject, or attachment type; show the selection count. Context menus and keyboard commands expose the same operation. | Local indexed equality/group operations. No provider feature needed until applying a remote action. |
| Working search / attention views | Results use the normal message table and bulk commands. Pin “unread or needs reply,” with remembered scope and sorting. | Local query engine and durable saved queries. Server search can supplement incomplete downloads, with coverage shown. |
| Make Filter and ordered rules | Create from a message or selection; preview matching examples, destination, actions, and stopping behavior before saving. | Rust interpreter. Optional compilation into a supported server-rule subset; never imply all local actions run while the Mac is asleep. |
| Personalities | Give every draft a visible identity chip, signature, and template; preview why a rule selected them. | Local identity policy. Sending authorization and available aliases are account-specific and must be checked. |
| Nicknames / Make Nickname | Create a named person/group from selected participants; show expanded addresses before sending. Maintain reusable recipient sets. | Local address/group resolver and export. System contacts/directory integration is optional; no mail-server extension required. |
| Stationery / Reply With | Templates store subject, recipients, body, identity hints, and attachment snapshots. Explicit variables open a small fill-in form. | Local, versioned templates and drafts. Server storage or team sharing is optional and should have a defined portability format. |
| Send Again | “Edit as New” creates a new draft linked to the source, with visible sender/recipient review and a new send operation. | Local duplication/composition. Ordinary submission through the selected backend; no recall or automatic redelivery promise. |
| Redirect / Turbo Redirect | Distinguish author-preserving redirection from forwarding. Offer “send, then file” as one operation with visible progress. | Construct a standards-aware resend only where the backend permits it. Retain the source until the send outcome is reconciled. |
| Queue / scheduled send | A persistent Outbox displays deadline, scheduling location, and cancellation status. Queued messages remain editable through explicit replacement. | Durable local scheduler; provider scheduling only after capability checks. Label local scheduling as dependent on this machine running. |
| Plain text and quotations | First-class plain-text editing, quote/unquote, selection-only reply, paragraph-aware wrapping, readable code, and optional rich text. | Local MIME composition/decoding; no special server feature. Test flowed-text quotation, space-stuffing, and DelSp handling against [RFC 3676](https://www.rfc-editor.org/rfc/rfc3676.html). |
| Editable received subject/body | Offer a personal title, annotations, or a corrected reading copy, with an obvious route back to the original. | Local overlays and provenance. Preserve original bytes; do not silently rewrite server mail or invalidate its evidence. |
| Contextual filing / Fcc / toolbar | Keyboard destination chooser; selected text seeds suggestions. Custom buttons can file or run a named action sequence. Send-and-file retains project context. | Local action definitions. Remote moves, labels, copies, and sent filing depend on adapter semantics; local archive destinations need none. |
| Content Concentrator | Collapse detected repeated quotations/signatures, mark omissions, and expand them instantly. | Deterministic local presentation derived from the original. No server dependency; a condensed view never replaces source content. |
| Numeric junk control | Show provider/local verdicts and their origins; explain threshold decisions and allow correction. Keep a reviewable quarantine. | Local classifier optional; reporting junk to providers requires adapter support. No silent destructive retention default. |
| MoodWatch / BossWatch | Optional local pre-send checks for configured recipients, missing attachments, or selected tone patterns; offer review or a cooling-off queue. | Local policy and delayed submission. Tone analysis needs explicit language support and evidence of usefulness; it should not block ordinary writing by default. |
| Speech, scripts, plug-ins | Accessible reading and named command hooks. Later, isolated extensions for transformations or account adapters. | Local services and a versioned extension contract. Separate viewing/transformation from permission to send or delete mail. |
| Portable complete archives | “Keep a complete local copy” includes attachments, metadata, and an inspectable completeness report. Disconnected archives remain searchable. | Rust storage and export. Fetching missing content requires backend access; retention after account removal is a separate user choice. |

#### Backend semantics the feature map must respect

**IMAP:** RFC 9051 defines standard flags including `\Seen`, `\Answered`, `\Flagged`, `\Deleted`, and `\Draft`; persistent custom keywords depend on server support. Message identity must account for mailbox UIDs and UIDVALIDITY. A shared Message-ID header is not a sufficient local primary key. [RFC 9051, flags and identifiers](https://www.rfc-editor.org/rfc/rfc9051)

**Proposed mapping:** retain a stable Sweetbriar object ID, separate server locations, and independent annotations. Map local labels to server keywords only when supported; record unsynchronized labels explicitly. Never turn a local subject correction into replacement of a signed or archived original.

**JMAP:** RFC 8621 supplies email queries, mailbox membership, keywords, identities, and EmailSubmission, with distinct capabilities. Availability must be discovered rather than assumed from “JMAP support.” [RFC 8621](https://datatracker.ietf.org/doc/html/rfc8621)

**Proposed mapping:** use JMAP objects directly in its adapter while preserving the same local query and outbox model. Discover submission/scheduling capabilities separately; a server lacking them still gets local templates, annotations, and saved views.

**Server filtering:** Sieve specifies server-side filtering and actions such as filing, redirecting, keeping, discarding, and stopping. Gmail’s API exposes a different filter model, including label changes and forwarding constraints. Neither is an arbitrary Eudora client-action interpreter. [RFC 5228](https://www.rfc-editor.org/rfc/rfc5228.html), [Gmail filter API](https://developers.google.com/workspace/gmail/api/guides/filter_settings)

**Proposed mapping:** show where each rule runs. Compile only a verified equivalent subset. Store server rule identifiers and report drift; do not run the same forwarding or auto-reply independently on the server and client.

**Microsoft 365, checked 2026-10-11:** Microsoft’s published plan starts controllable, phased Exchange Online EWS blocking on **2026-10-01**, with permanent shutdown starting **2027-04-01**. This does not mean every tenant was already blocked on the research date, and the announcement distinguishes on-premises Exchange. Prefer Microsoft Graph for a new Exchange Online adapter; evaluate on-premises Exchange separately. [Exchange team plan](https://techcommunity.microsoft.com/blog/exchange/exchange-online-ews-your-time-is-almost-up/4492361), [Microsoft Learn, updated 2026-09-04](https://learn.microsoft.com/en-us/exchange/clients-and-mobile-in-exchange-online/deprecation-of-ews-exchange-online)

Graph accepts JSON or MIME through `sendMail`; its **202 Accepted** response does not establish completion or delivery. [Graph documentation, checked 2026-10-11](https://learn.microsoft.com/en-us/graph/api/user-sendmail?view=graph-rest-1.0)

**Proton, checked 2026-10-11:** Proton documents Bridge for desktop IMAP/SMTP access on paid plans and does not support POP3. A Bridge-based adapter is the justified initial route; a direct proprietary API implementation is not established by this evidence. [Proton documentation](https://proton.me/support/imap-smtp-and-pop3-setup)

#### Two workflows requiring particular care

**Filters should become an inspectable program.** My recommendation is a typed rule model with named phases: after receipt, before submission, after confirmed submission, and manual. “Before submission” can choose identity or add content; later phases can file copies and update local state. Moving a message should not secretly terminate processing. A visible terminal action should make that choice explicit.

Provide nested conditions without Eudora’s two-term ceiling, but start the editor with one useful condition and one action. Include a simulation view: “These 42 existing messages match; 11 would move; none would send.” For a received message, a rule trace should identify the matching conditions, completed actions, pending remote operations, and the rule that stopped processing. Preserve previous rule revisions, and offer reversible manual actions. Auto-replies need deduplication, loop prevention, and a deliberately reviewed activation path.

**Strongest objection:** this can grow into an intimidating programming environment. Keep the ordinary rule editor simple; reserve traces and advanced conditions for disclosure panels. The differentiator is understandable behavior, not the largest list of actions.

**Redirect needs wire-level honesty.** RFC 5322 §3.6.6 defines `Resent-*` fields for reintroducing a message into transport while retaining original authorship. Authentication introduces additional constraints: DMARC evaluates alignment with the author domain, and indirect flows can fail validation. As checked **2026-10-11**, the current DMARC document is **RFC 9989, published May 2026**, obsoleting RFCs 7489 and 9091. [RFC 5322](https://www.rfc-editor.org/rfc/rfc5322), [RFC 9989](https://www.rfc-editor.org/info/rfc9989/), [indirect-flow analysis](https://www.rfc-editor.org/rfc/rfc7960.html)

**Proposed behavior:** test whether each submission backend preserves the required headers and original content, and disclose unsupported cases. Offer ordinary forwarding or forwarding the original as an attachment when faithful redirection is unavailable. Avoid claiming that preserving the From address guarantees delivery. A Turbo-style action must retain the source and a recoverable action record until the outbound result is known. An uncertain send must not quietly trigger both source deletion and a second send attempt.

**Strongest objection:** the faithful command may work on fewer accounts than the nostalgic interaction suggests. That is preferable to a familiar command name whose actual authorship and reply behavior vary silently.

## 3. Options compared

The assessments below are engineering judgments against Sweetbriar’s stated requirements, not measured implementation estimates.

| Approach | Attraction | Main trade-off | Assessment |
| --- | --- | --- | --- |
| Port classic Eudora / extend Eos | Closest source-level access to historical details and file handling. | Historical platform dependencies, separate codebases, removed components, and component-specific rights; substantial work before reaching the required Rust/native architecture. [CHM](https://computerhistory.org/blog/the-eudora-email-client-source-code/), [Eos](https://hermes.cx/) | Use for research and carefully selected import knowledge. |
| Thunderbird-derived application | Existing mail infrastructure and many verified power-user features; Penelope supplies a historical precedent. [Project record](https://wiki.mozilla.org/Penelope), [filters](https://support.mozilla.org/en-US/kb/organize-your-messages-using-filters) | Adapting it to the prescribed Rust core and Swift interface is a separate architectural undertaking. Familiar feature names alone do not settle interaction fidelity. | Strong counterproposal if architecture constraints change. |
| New Rust core + Swift UI | Fits the requested platform boundary; permits explicit durability, query, and operation models shared with Linux. | Sweetbriar assumes responsibility for interoperability, recovery, MIME edge cases, indexing, and long-term migrations. | Recommended, with narrow staged delivery. |
| Native client around provider-specific services | Can expose one provider’s capabilities naturally; Mimestream demonstrates the Gmail API direction. [Vendor](https://mimestream.com/) | Broad source support requires additional adapters and clear semantics for unsupported operations. | Adopt its adapter philosophy without making one provider the whole core. |

## 4. Recommendations for Sweetbriar

### 4.1 Give the Rust core ownership of meaning and durability

**Recommended architecture:** Rust owns message identity, MIME parsing, storage, synchronization, search, rules, templates, drafts, and durable operation state. Swift owns native presentation, interaction, accessibility, and platform services. Linux should later consume the same domain operations through its own native interface. UI code should not contain a second implementation of filter matching, attachment ownership, or send-state transitions.

Candidate building blocks were checked **2026-10-11**:

| Component | Candidate and verified basis | Evaluation needed |
| --- | --- | --- |
| MIME / historical encoding | `mail-parser` documentation shows **0.11.9**, RFC/MIME parsing and legacy charset decoding. [Documentation](https://docs.rs/mail-parser/latest/mail_parser/) | Malformed real-world archives, Mac encodings, detached attachments, signed/encrypted messages; preserve bytes independently of parse success. |
| Metadata / operations | `rusqlite` exposes SQLite to Rust. [Documentation](https://docs.rs/rusqlite/latest/rusqlite/) | Transaction boundaries, recovery, migration, and coordination with external blob files. |
| Full-text search | Tantivy documentation shows **0.26.2**, with field schemas, indexing, commits, and querying. [Documentation](https://docs.rs/tantivy/latest/tantivy/) | Exact versus tokenized matching, languages, regex strategy, large attachments, incremental updates, index rebuilds. |
| Swift interface | UniFFI documents Swift bindings. [Guide](https://mozilla.github.io/uniffi-rs/latest/) | Cancellation, lifetime ownership, asynchronous events, batching, and avoiding excessive per-row crossings. |

These versions are observed documentation snapshots, not pinned dependency recommendations. No crate was benchmarked or security-audited for this assignment.

Use AppKit for the demanding message table, text editing, menus, and multiwindow behavior where appropriate, with SwiftUI for suitable surrounding views. This is my implementation preference, not a claim that SwiftUI cannot handle those tasks. Prototype the actual workload before fixing the UI split. Keep message HTML display separate from the original content and from the plain-text editor.

**Trade-off:** a clear core boundary adds up-front API and event-model work. **Strongest counterargument:** integrating an established mail engine could avoid years of edge-case repair. The answer is selective reuse and a real interoperability corpus, not confidence that Rust alone makes email safe.

### 4.2 Define “never lose mail” as explicit, testable guarantees

SQLite’s atomic-commit explanation describes journaling, flushes, and assumptions about storage behavior. It does not make a separate attachment directory automatically part of a database transaction. [SQLite durability explanation](https://sqlite.org/atomiccommit.html)

**Proposed storage design:** immutable original message blobs plus transactionally managed metadata, draft revisions, settings revisions, attachment references, and operation records. Maintain staging and recovery rules for the boundary between the database and blob files. The search index is derived and replaceable. Preserve a failed-to-parse original instead of dropping it from the import.

For API sources, retain original MIME when the provider exposes it, alongside the source response and identifiers needed to interpret it. If the provider supplies only a representation, record that fact; do not describe a reconstructed message as a byte-identical original. Decoded attachment previews can be regenerated, but the only retained attachment must never live solely in a disposable cache.

Recommended invariants:

1. A draft or settings change is acknowledged as saved only after the defined durable commit. If storage fails, editing remains recoverable and the failure is visible.
2. A send intent and its exact content/recipients/identity are durably recorded before submission. Editing a queued item creates an explicit replacement revision.
3. Draft, queued, submitting, provider-accepted, uncertain, and failed are distinct states. A timeout does not justify guessing “unsent” and automatically replaying an ambiguous operation.
4. Local archival retention is separate from remote deletion. Removing credentials or an account does not silently erase retained archives or unresolved drafts.
5. Export includes message content and a versioned metadata manifest. Verification checks counts, hashes, attachment references, flags, labels, identities, and unresolved items.
6. Recovery can proceed without the full-text index. Missing content and corrupt content produce visible reports rather than disappearing rows.

For Eudora migration, retain an untouched copy of the complete source directory. Build a read-only importer with per-mailbox accounting and source offsets. Recover attachment/embedded-content references, preserve original names, and distinguish missing attachments from messages that never had them. Import labels, statuses, personalities, and useful local subject changes into explicit metadata. Never deduplicate solely by Message-ID; duplicate deliveries and missing identifiers require more careful provenance.

Offer `.eml` message export, conventional mbox interoperability, and a Sweetbriar archive with a documented JSON manifest. A simple mailbox export should state which annotations, drafts, templates, or settings it cannot carry. A complete export should reopen in a clean profile, not merely generate files.

**Trade-off:** retained originals, revisions, quarantine, and local archives consume disk. Provide storage accounting, explicit retention choices, and verified backups. **Strongest counterargument:** users may expect all deletions to free space immediately. Make “remove from server,” “remove local cached copy,” and “destroy retained archive copy” understandable operations with distinct consequences.

The absolute guarantee cannot include destruction of all storage or every possible hardware failure. Publish the actual guarantees and backup/recovery limits. This is part of defining the requested safety requirement, not a reason to weaken ordinary crash or synchronization guarantees.

### 4.3 Make search a working surface with a coverage contract

**Recommendation:** one typed query model should drive the visual builder, a textual syntax, saved views, and rule matching where their semantics overlap. Preserve literal addresses, complete headers, normalized display text, attachment names, message dates, receipt dates, identity, server location, local annotations, and draft/send state as distinct fields.

Default to deterministic filters and explainable ranking. A query such as “from this sender, with attachments, received last year” should show its interpreted constraints. Offer exact phrases, all/any/none groups, ranges, and expert regex without making regex the normal interface. Keep normalized subject grouping separate from standards-based reply relationships and manual grouping.

Every result view should state its coverage: fully local archive, headers only, bodies still downloading, attachments not indexed, or remote search added. Zero results in a partial cache must not look like proof that no matching mail exists. Updating an index must not interrupt reading or composing; cancellation should abandon work without losing the query or selection.

**Proposed acceptance targets, not measured results:** use a corpus of **one million messages**, with multilingual text, malformed MIME, duplicate Message-IDs, detached historical attachments, and several accounts. Aim for the first useful page of a warm local search within **200 ms at the 95th percentile** on a named reference Mac; report cold-start and concurrent-sync results separately. Include adversarial queries and a correctness oracle, not just stopwatch demonstrations.

**Trade-off:** exact fields, full offline content, and comprehensive attachment indexing cost storage and background work. **Strongest counterargument:** provider search may already be superior for a particular account. Use it as an additional source with explicit scope; preserve local search for disconnected and provider-independent archives.

### 4.4 Ship a coherent first slice and defer ornamental fidelity

The first slice should include durable drafts/settings, complete-copy archival controls, ordinary receive/send, visible lifecycle state, a compact native table, select-similar, actionable search/saved views, identities, templates, and audited import/export. Validate one backend end to end before adding the rest, while designing capability negotiation for all intended account types.

The next slice should add Make Filter with simulation and traces, contextual filing, Send Again, and send-and-file. Add true Redirect only after backend-specific tests. Extend the same rules/query model across IMAP, Gmail API, JMAP, Microsoft 365, and Proton Bridge rather than creating unrelated automation systems for each account.

Defer tone scoring, email-usage graphs, ESP-style folder synchronization, and broad third-party plug-ins until safety and core workflows are established. Keep configurable sounds and speech available when they serve attention or accessibility. Revive the idea behind a whimsical feature only when it solves a present problem.

For each release, test crash recovery around commits and sends; loss of connection during an operation; disk-full behavior; removal of credentials; conflicting draft edits; account deletion with retained archives; malformed imports; and reopening exports in an empty profile. Search correctness, keyboard access, accessible status descriptions, and reversible filing deserve acceptance tests alongside the backend.

**Strongest counterargument to the overall recommendation:** a new email core may spend years reaching the reliability and account coverage users already have, while Eudora-inspired affordances alone may not attract enough users. Sweetbriar should justify itself with complete archives, predictable operations, and useful cross-provider workflows before expanding its nostalgic feature inventory.

## 5. Risks and open questions

| Risk or unanswered question | Consequence and proposed resolution |
| --- | --- |
| Nostalgia and selection bias | Enthusiasts and surviving installations do not represent all users. Include people who abandoned Eudora and current-client power users in workflow studies. The conflicting support-worker memories above are useful counterevidence. |
| Historical platform/version drift | Two implementations, evolving modes, and changing behavior complicate compatibility. Obtain representative Mac and Windows archives and identify their originating versions before promising import fidelity. |
| Detached attachments and metadata | An mbox parser alone is insufficient for complete historical import. Build a fixture corpus with `.toc` files, Mac metadata/resource-fork cases, local subject edits, and missing/renamed attachments. |
| Modern rights and dependencies | CHM rights, omitted components, and derivative-project licenses need component-level review before reuse. This report does not establish that a complete classic or Eos build can be commercially redistributed. [CHM license](https://computerhistory.org/blogs/the-eudora-email-client-source-code-license-agreement/), [Eos repository](https://github.com/nmatavka/HERMES-Eos) |
| Exchange coverage | Microsoft’s documentation lists generic public-folder and Microsoft 365 group-mailbox CRUD among capabilities not being added to Graph. Define shared/public/group mailbox scope separately from ordinary personal mail. Checked **2026-10-11**. [Microsoft Learn](https://learn.microsoft.com/en-us/exchange/clients-and-mobile-in-exchange-online/deprecation-of-ews-exchange-online) |
| Redirect and identity restrictions | The precise fidelity of SMTP, Gmail API, Graph MIME submission, and JMAP redirection is **unverified** here. Test headers, signatures, reply routing, permitted identities, and failure handling per backend. |
| Scheduling and duplicate sends | A local deadline cannot imply delivery while the machine is unavailable. Determine which providers offer the necessary scheduling and reconciliation capabilities; publish ambiguous-outcome behavior. |
| Offline completeness | What content may be evicted, what remains after account removal, and how much local archive storage is required need explicit product decisions. Make policies visible per account/folder. |
| Encrypted mail and private search | Define the treatment of decrypted content, index protection, attachments, and export. Proton Bridge support does not itself settle Sweetbriar’s local encryption policy. |
| Search portability | Decide tokenization languages, phrase normalization, regex limits, attachment extraction formats, and equivalence between local and server queries. Avoid promising identical results where capabilities differ. |
| Accessibility and discoverability | Option-only gestures and color-only labels exclude users. Every powerful shortcut needs an ordinary command and accessible state description. Test the native table and composer with keyboard and assistive technology. |
| Current-client comparison gaps | Current Apple Mail Redirect and exact interaction parity across all six clients remain **unverified**. Test specific versions/account types before marketing any feature as uniquely restored. |

The next research step should be a preservation-led Eudora import experiment with donated, anonymized fixtures and documented consent, plus a small interaction prototype for select-similar, search-result actions, identity-aware templates, and filter simulation. That would convert documentary lessons into evidence about Sweetbriar’s own implementation.

## 6. Sources

The following URLs were consulted. Historical publication dates are included where established; current documentation and project status were checked **2026-10-11**. Manual mirrors reproduce vendor material but are not evidence that an old download is currently supported. The CHM ZIP archives themselves could not be retrieved successfully.

### History, source, and preservation

- [CHM history and source-release discussion — 2018-05-22; later update labeled September 2024](https://computerhistory.org/blog/the-eudora-email-client-source-code/)
- [CHM release announcement — 2018-05-22](https://www.globenewswire.com/news-release/2018/05/22/1510151/28639/en/computer-history-museum-makes-the-eudora-email-client-source-code-available-to-the-public.html)
- [CHM source license and download links](https://computerhistory.org/blogs/the-eudora-email-client-source-code-license-agreement/)
- [Qualcomm/Mozilla announcement — 2006-10-11](https://blog.mozilla.org/press/2006/10/qualcomm-launches-project-in-collaboration-with-mozilla-foundation-to-develop-open-source-version-of-eudora-email-program/)
- [Qualcomm Eudora history page](https://www.qualcomm.com/eudora)
- [Linux.com contemporary business reporting — 2006-10-11](https://www.linux.com/news/eudora-dead-long-live-eudora/)
- [Mozilla Penelope/OSE project record](https://wiki.mozilla.org/Penelope)
- [HERMES revival site](https://hermes.cx/)
- [HERMES Eos repository](https://github.com/nmatavka/HERMES-Eos) and [inspected filter header](https://raw.githubusercontent.com/nmatavka/HERMES-Eos/main/V624/Include/filters.h)
- [Eudora repair utility and documentation](https://github.com/LenShustek/Eudora_fix_mbx)

### Manuals and contemporary reviews

- [Mac Light 3.1 manual — 1997, transcribed mirror](https://manualzilla.com/doc/5798190/version-3.1-for-macintosh-user-manual)
- [Mac Pro 4.2 manual, PDF](https://perso.uclouvain.be/jean-pierre.kuypers/logicielTraduit/Eudora/4/manuals/mac42usermanual.pdf)
- [Mac 6.2 manual — 2004, PDF](https://web.stanford.edu/dept/its/support/ess/doc_archive/ess/mac/docs/eudora6/eudora62_manual.pdf)
- [Windows 5.1 manual — 2001, transcribed mirror](https://doczz.net/doc/93993/eudora-5.1-user-manual)
- [Windows 7.1 guide — 2006, transcribed mirror](https://manualzilla.com/doc/6879944/eudora%C2%AE-email-7.1-user-guide-for-windows)
- [Windows 7.0 vendor new-features guide, PDF](https://www.stordito.it/web/tutorial/whatsnew_v7.0.pdf)
- [Internet Starter Kit, Eudora chapter](https://www.tidbits.com/resources/iskm3html/pt4/ch21/ch21a.html)
- [TidBITS tips — 1997-11-17](https://tidbits.com/1997/11/17/eudora-tips-tricks/)
- [TidBITS Pro 4.0 coverage — 1998-01-19](https://tidbits.com/1998/01/19/eudora-pro-4-0-finalized-2/)
- [TidBITS Pro 4.2 Part 1 — 1999-07-12](https://tidbits.com/1999/07/12/eudora-pro-4-2-continues-to-deliver-part-1/)
- [TidBITS Pro 4.2 Part 2 — 1999-07-19](https://tidbits.com/1999/07/19/eudora-pro-4-2-continues-to-deliver-part-2/)
- [TidBITS Eudora 5.0 — 2000-09-11](https://tidbits.com/2000/09/11/eudora-5-0-reads-your-mind/)
- [Macworld Eudora 6 announcement — 2003-09-03](https://www.macworld.com/article/167814/eudora-12.html)
- [Macworld Eudora 6 review — 2004-01-01](https://www.macworld.com/article/169149/eudora6.html)
- [TidBITS working-search workflow — 2004-12-20](https://tidbits.com/2004/12/20/a-new-way-to-use-eudora/)

### User experience and migration

- [Tonya Engst’s switch to Apple Mail — 2008-04-13](https://tidbits.com/2008/04/13/reluctantly-switching-from-eudora-to-apple-mail/)
- [Adam Engst’s migration report — 2011-09-06](https://tidbits.com/2011/09/06/converting-email-from-eudora-why-i-no-longer-live-at-the-p-o/)
- [Hacker News discussion — 2017-10-02](https://news.ycombinator.com/item?id=15371882)
- [Hacker News discussion — 2018-05-22](https://news.ycombinator.com/item?id=17127486)
- [HERMES user/developer discussion — begun 2023-12-01](https://sourceforge.net/p/hermesmail/discussion/general/thread/91a33b7b87/)

### Current clients

- [Apple Mail rules](https://support.apple.com/en-ie/guide/mail/cpmlprefrules/mac)
- [Apple’s indexed Redirect guide, now redirecting](https://support.apple.com/en-bw/guide/mail/mlhl8b91a034/mac)
- [Apple Mail Send Again tutorial — updated 2026-03-11](https://www.idownloadblog.com/2021/03/23/resend-email-mail-send-again/)
- [Outlook resending and account restrictions](https://support.microsoft.com/en-us/outlook/resend-an-email-message-in-outlook)
- [Gmail filters](https://support.google.com/mail/answer/6579?hl=en_as) and [templates](https://support.google.com/mail/answer/14864208?hl=en-S)
- [Thunderbird filters](https://support.mozilla.org/en-US/kb/organize-your-messages-using-filters) and [saved searches](https://support.mozilla.org/en-US/kb/using-saved-searches?redirectlocale=en-US&redirectslug=what-are-saved-searches)
- [Simple Mail Redirection extension](https://services.addons.thunderbird.net/eN-US/thunderbird/addon/simple-mail-redirection/)
- [Spark templates](https://sparkmailapp.com/help/spark-ai/templates-ai)
- [Mimestream feature documentation](https://mimestream.com/)

### Protocols and implementation candidates

- [RFC 5322: Internet Message Format, including resending](https://www.rfc-editor.org/rfc/rfc5322)
- [RFC 9989: DMARC — May 2026](https://www.rfc-editor.org/info/rfc9989/)
- [RFC 7960: DMARC and indirect mail flows](https://www.rfc-editor.org/rfc/rfc7960.html)
- [RFC 9051: IMAP4rev2](https://www.rfc-editor.org/rfc/rfc9051)
- [RFC 8621: JMAP Mail](https://datatracker.ietf.org/doc/html/rfc8621)
- [RFC 5228: Sieve](https://www.rfc-editor.org/rfc/rfc5228.html)
- [RFC 3676: format=flowed plain text](https://www.rfc-editor.org/rfc/rfc3676.html)
- [Gmail filter API](https://developers.google.com/workspace/gmail/api/guides/filter_settings)
- [Graph sendMail](https://learn.microsoft.com/en-us/graph/api/user-sendmail?view=graph-rest-1.0)
- [Exchange team EWS retirement plan](https://techcommunity.microsoft.com/blog/exchange/exchange-online-ews-your-time-is-almost-up/4492361)
- [Microsoft Learn EWS retirement and capability gaps — updated 2026-09-04](https://learn.microsoft.com/en-us/exchange/clients-and-mobile-in-exchange-online/deprecation-of-ews-exchange-online)
- [Proton Bridge/IMAP/SMTP guidance](https://proton.me/support/imap-smtp-and-pop3-setup)
- [SQLite atomic commit](https://sqlite.org/atomiccommit.html)
- [mail-parser](https://docs.rs/mail-parser/latest/mail_parser/), [rusqlite](https://docs.rs/rusqlite/latest/rusqlite/), [Tantivy](https://docs.rs/tantivy/latest/tantivy/), [UniFFI](https://mozilla.github.io/uniffi-rs/latest/)
