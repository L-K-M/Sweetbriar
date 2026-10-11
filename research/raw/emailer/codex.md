<!-- provenance: helper=codex model=gpt-6.1-sol started=2026-10-11T18:14:20+00:00 exit=0 brief=research/briefs/ -->

# Claris Emailer: research for Sweetbriar

Research date: **2026-10-11**. Historical dates below refer to the original publications or events; current product and API observations were checked on this research date. **Verified** means supported by the linked documentary source, not reproduced in an emulator. **Recollection** identifies later testimony. Recommendations are proposals for Sweetbriar.

## 1. TL;DR

- **Emailer's central achievement was bringing incompatible services into one Mac interface.** Its 1995 release combined AOL, CompuServe, eWorld, RadioMail, and Internet POP/SMTP accounts. That is a useful precedent for Sweetbriar's multiple backends. [TidBITS, 1995-07-31](https://tidbits.com/1995/07/31/claris-emailer-ships/)
- **Fog City developed it; Claris initially licensed its marketing.** The contemporary announcement explicitly says Fog City would continue development. “Claris invented it” and “Microsoft bought it” are inadequate histories. [TidBITS, 1995-04-03](https://tidbits.com/1995/04/03/emailer-licensed-by-claris/)
- **Revive its control over when mail moves.** Schedules selected accounts and separated sending from receiving. Revive that control with durable offline work. [User's Guide, chapter 6](https://archive.org/download/claris-emailer-lite-manual/Claris_Emailer_Lite_Manual.pdf)
- **Revive understandable Actions.** Emailer 2.0 combined tests, effects, and AppleScript; criticism shows the need for previews. [Macworld, July 1997, p. 52](https://vintageapple.org/macworld/pdf/Macworld_9707_July_1997.pdf)
- **Much of its celebrated power came from extensions.** Text cleanup, digest extraction, external editors, export, and FileMaker archiving belong in the inventory as scripts, not universal built-in features. [Fog City utilities](https://www.fogcity.com/em_utilities2.0.html)
- **Respect version boundaries.** Built-in redirect and automatically updated contact groups were 2.0v3 additions, reported in February 1998. June 9 was a later Apple updater distribution date. [TidBITS, 1998-02-16](https://tidbits.com/1998/02/16/emailers-last-gasp-update-2/), [Apple updater information](http://savagetaylor.com/til/TA37819.html)
- **Do not copy its storage weaknesses or assume its search was exceptional.** Later users documented expensive whole-database backups, slow searches, and fragile recovery. [Carlson, 2000-04-24](https://tidbits.com/2000/04/24/switching-from-emailer-to-eudora-in-leaps-and-bounds/), [McElhearn, 2000-05-08](https://tidbits.com/2000/05/08/migrating-to-new-climes-with-powermail/)
- **The opportunity is a coherent experience, not a list of vanished features.** Apple Mail still supports AppleScript actions; Thunderbird still has substantial filters. Current Outlook for Mac documents server rules only. [Apple](https://support.apple.com/en-ie/guide/mail/mlhlp1120/mac), [Mozilla](https://support.mozilla.org/hsb/kb/organize-your-messages-using-filters), [Microsoft](https://support.microsoft.com/en-us/outlook/create-a-rule-in-outlook-for-mac)
- **Recommendation:** build a native, keyboard-friendly Mac mail workbench over a durable Rust core. The strongest objection is that comprehensive local automation and heterogeneous providers create a large correctness burden before Sweetbriar has established demand.

## 2. Findings

### 2.1. History, business, and lineage

**Verified origin.** Emailer was developed by Fog City Software, associated with Guy Kawasaki. The April 1995 announcement describes a licensing arrangement with Claris, not an outright acquisition. The July shipping notice connects its design to Kawasaki's frustration with maintaining accounts on several services. Fog City's surviving staff page names Kawasaki, Will Mayall, Kathryn Henkens, Jud Spencer, and Tom Trefny; it does not establish identical roles or founding status for everyone pictured. [Licensing announcement](https://tidbits.com/1995/04/03/emailer-licensed-by-claris/), [shipping announcement](https://tidbits.com/1995/07/31/claris-emailer-ships/), [Fog City staff page](https://www.fogcity.com/fcs_us.html)

| Date | Verified event and significance |
|---|---|
| 1995-04-03 | TidBITS reports that Claris will market Emailer while Fog City continues development. [Announcement](https://tidbits.com/1995/04/03/emailer-licensed-by-claris/) |
| July 1995 | Emailer ships for Macintosh; the July 31 report says Claris announced shipment the previous week. An exact first shipment day is **(unverified)** here. [Shipping report](https://tidbits.com/1995/07/31/claris-emailer-ships/) |
| 1.1, before 2.0 | Adds OfficeMail support and local preferences; removes eWorld in 1.1v1. The 1.1v3 update addresses Open Transport 1.1.1 compatibility. [Claris release history](http://savagetaylor.com/til/TA43586.html) |
| April 1997 | Emailer 2.0 ships: database storage, hierarchical folders, and stronger Actions. April 14 coverage reports availability the preceding week. [TidBITS](https://tidbits.com/1997/04/14/emailer-2-0-available/) |
| 1997-05-14 | An Apple announcement lists Emailer **Lite** among Mac OS 8's Internet software. Bundled Lite and commercial Emailer 2.0 should be distinguished. [Preserved Apple announcement](https://www.mactech.com/1997/05/14/md1-apple-outlines-internet-developments/) |
| 1998-01-27 | Apple announces the Claris-to-FileMaker restructuring. President Dominique Goupil's letter identifies Emailer among the products moving to Apple. [Original announcement](https://web.archive.org/web/19981206205953/http://www.claris.com/news/docs/fminc-release.html), [president's letter](https://web.archive.org/web/19981205225904/http://www.claris.com/news/docs/fminc-letter.html) |
| February 1998 | 2.0v3 adds redirect, improved groups, contextual menus, folder dragging, and fixes. [February 16 report](https://tidbits.com/1998/02/16/emailers-last-gasp-update-2/) |
| 1998-06-09 | Apple posts the free 2.0v3 updater, without free support. This is not evidence that the complete commercial product became freeware. [Apple distribution readme](http://savagetaylor.com/til/TA37819.html) |
| October 1998 | TidBITS discusses Emailer's apparent demise, rescue petitions, possible sale, and a proposed organization for rescuing abandoned software. [October 12](https://tidbits.com/1998/10/12/emailer-and-the-death-of-software/), [October 19](https://tidbits.com/1998/10/19/electronic-phoenix-project/) |
| 2000–2002 | Users migrate because of the product's future and limitations; others continue using it in the Classic environment under OS X. [Carlson](https://tidbits.com/2000/04/24/switching-from-emailer-to-eudora-in-leaps-and-bounds/), [Knight](https://lowendmac.com/2002/the-joy-of-os-x-with-classic/) |

**Platforms.** The released product was a classic Macintosh application. A June 1997 review gives a 68020-or-newer Mac and System 7.1-or-later requirement. A later Entourage history says Windows editions were proposed but never shipped. No released Windows or native OS X edition was verified. That testimony does not establish how far a Windows prototype progressed. [TidBITS review](https://tidbits.com/1997/06/02/email-reliance-emailer-2-0/), [archived Entourage history](https://web.archive.org/web/20080624084550/http:/blog.entourage.mvps.org/2007/05/in_the_beginning.html)

**The business story was a reorganization, not the disappearance of the whole company.** Apple's January 27, 1998 announcement proposed concentrating Claris on FileMaker, cited $73 million in FileMaker sales for FY97, and announced approximately 300 layoffs. Goupil's letter retained Home Page within that focus and explicitly transferred Emailer to Apple. [Original press release](https://web.archive.org/web/19981206205953/http://www.claris.com/news/docs/fminc-release.html), [customer letter](https://web.archive.org/web/19981205225904/http://www.claris.com/news/docs/fminc-letter.html)

**Why it died: evidence versus explanation.** The corporate transfer is documented. Adam Engst's October 1998 analysis describes expensive modernization, competing free or inexpensive clients, and tension between Apple's software offerings and third-party developers. He identifies IMAP, HTML, and directory services as areas in which competitors had advanced. Those are informed contemporary explanations, **not Apple's published internal cancellation rationale**. The exact internal cancellation date remains **(unverified)**. [Corporate transfer](https://tidbits.com/1998/02/02/claris-to-restructure-as-filemaker-inc-2/), [contemporary analysis](https://tidbits.com/1998/10/12/emailer-and-the-death-of-software/)

**Team lineage is better established than code lineage.** The same October article says Emailer's lead developer was already working on Outlook Express. The 2007 Entourage history names Jud Spencer, Dan Crevier, Dave Cortright, and Dave Marandino as people who moved from Emailer. However, that retrospective incorrectly puts Emailer 1.0's launch in June 1996, contradicting the 1995 shipping report. Use it cautiously for personnel testimony, not as a reliable release chronology. [Contemporary evidence](https://tidbits.com/1998/10/12/emailer-and-the-death-of-software/), [2007 retrospective](https://web.archive.org/web/20080624084550/http:/blog.entourage.mvps.org/2007/05/in_the_beginning.html), [1995 shipment](https://tidbits.com/1995/07/31/claris-emailer-ships/)

Microsoft program manager Omar Shahine's 2004 account describes Outlook Express and Entourage as native Mac applications with development separate from Windows and Office code. It supports a distinct Mac product lineage; it does **not** demonstrate that Microsoft acquired Emailer's source. Fog City's surviving homepage says Apple owns Emailer. No source transfer agreement or public source release was verified. [Archived Shahine account](https://web.archive.org/web/20060503185524/http://blogs.msdn.com/omars/archive/2004/04/05/107468.aspx), [Fog City](https://www.fogcity.com/)

The source-code mystery is real but should not be embellished. In a 2005 submission to the US Copyright Office, user Dave Nathanson described unsuccessful efforts to locate the source and said it appeared lost. That verifies his experience, **not actual destruction of the code**. Engst's 1998 rescue proposal also identifies licensed AOL access code and the StuffIt engine as complications. [Nathanson submission](https://www.copyright.gov/orphan/comments/OW0433-Nathanson.pdf), [Electronic Phoenix proposal](https://tidbits.com/1998/10/19/electronic-phoenix-project/)

### 2.2. Feature inventory

**Evidence boundary.** The scanned guide is catalogued as “Lite” but describes multi-service Emailer; here it is the **1.x-era guide**, with uncertain Lite applicability. The listed 2.0 guide was not retrieved; its inventory uses readmes, FAQs, catalogs, and reviews. [Scanned guide](https://archive.org/download/claris-emailer-lite-manual/Claris_Emailer_Lite_Manual.pdf), [2.x preserved downloads](https://www.macintoshrepository.org/1283-claris-emailer-2-x)

The following inventory is documentary, with built-in behavior separated from optional extensions. It is not an exhaustive reconstruction of every dialog or localized build.

| Area | Concrete verified behavior | Version/source |
|---|---|---|
| Multiple services | One client and inbox for AOL, CompuServe, eWorld, RadioMail, and POP/SMTP Internet mail. BIX/GEnie appeared as possibilities, not shipped capabilities in this announcement. | 1995 shipment. [TidBITS](https://tidbits.com/1995/07/31/claris-emailer-ships/) |
| Later service list | 2.0 support material also lists AppleLink; do not assume the 1995 service list remained unchanged. | [Washington Apple Pi, p. 59](https://www.wap.org/journal/showcase/washingtonapplepijournal1997v19no6.pdf) |
| Schedules | `Setup > Schedules`; timed/repeating entries; account selection; separate Get/Send; “Disable all schedules.” Requires the running application. | 1.x guide, pp. 6-1–6-4. [Manual](https://archive.org/download/claris-emailer-lite-manual/Claris_Emailer_Lite_Manual.pdf) |
| Travel and routing | Named Locations hold dial-up settings; Destinations select services and gateway mappings. | 1.x guide, chapter 6. [Manual](https://archive.org/download/claris-emailer-lite-manual/Claris_Emailer_Lite_Manual.pdf) |
| Address completion | Recipient name plus Tab completes an address or offers a choice. | 1.x guide. [Manual](https://archive.org/download/claris-emailer-lite-manual/Claris_Emailer_Lite_Manual.pdf) |
| Early Actions | Named, enabled/disabled, ordered rules; sender/subject/body tests and address-book membership; first matching rule stops further processing. | 1.x guide, chapter 5. [Manual](https://archive.org/download/claris-emailer-lite-manual/Claris_Emailer_Lite_Manual.pdf) |
| Expanded Actions | Multiple tests/effects; prioritize, file, add sender to contacts, run AppleScript. The original version allowed one effect per Action. | 2.0. [Macworld, p. 52](https://vintageapple.org/macworld/pdf/Macworld_9707_July_1997.pdf) |
| Additional effects | Automatic response, forwarding, printing, and adding/removing addresses in groups. | 2.0 review. [TidBITS](https://tidbits.com/1997/06/02/email-reliance-emailer-2-0/) |
| Offline composition | Compose disconnected; queued Out Box messages send later; `Do Not Schedule` holds them. | 1.x guide, composition/troubleshooting. [Manual](https://archive.org/download/claris-emailer-lite-manual/Claris_Emailer_Lite_Manual.pdf) |
| Handling preferences | Automatic filing; read/sent/deleted retention; sound/menu-bar notifications; startup/download preferences. | 1.x guide, appendix A. [Manual](https://archive.org/download/claris-emailer-lite-manual/Claris_Emailer_Lite_Manual.pdf) |
| Mail Browser | Folder tree, hierarchical folders, sortable message columns; separate folder/message windows and floating toolbar/status windows. | 2.0. [TidBITS](https://tidbits.com/1997/06/02/email-reliance-emailer-2-0/) |
| Keyboard filing | `Command-Option-F` opens a folder chooser; Return confirms. Filing an open message leaves it open. `Go to New Mail` locates folders containing unread messages. | 2.0. [TidBITS](https://tidbits.com/1997/06/02/email-reliance-emailer-2-0/) |
| Priority vocabulary | Nineteen user-defined priorities with names/colors, plus reserved Alert. | 2.0. [TidBITS](https://tidbits.com/1997/06/02/email-reliance-emailer-2-0/) |
| Setup/accounts/network | Easy Setup wizard; unlimited accounts per service/ISP, replacing the earlier five-per-service limit; FreePPP/Open Transport PPP connection and disconnection control. | 2.0. [Macworld](https://vintageapple.org/macworld/pdf/Macworld_9707_July_1997.pdf) |
| Contact browsing | Live Address Book Filter narrows entries as you type. | 2.0. [TidBITS](https://tidbits.com/1997/06/02/email-reliance-emailer-2-0/) |
| Composer conveniences | Multiple/random signatures, spell checking, message-size download limits; toolbar help and Command-key shortcut hints. | 2.0; reproduced August 1997 MacAddict review. [Preserved review](https://www.macintoshrepository.org/1283-claris-emailer-2-x) |
| Message navigation | `Command-[` and `Command-]` move to previous/next message; account option to retrieve all server messages; individual recipients can be marked for resending. | Vendor Important Information. [Readme](http://savagetaylor.com/til/TA43668.html) |
| Search | Database-wide results in separate windows; multiple searches possible. Improvement over 1.x did not make it universally fast. | 2.0 reviews. [Preserved MacAddict review](https://www.macintoshrepository.org/1283-claris-emailer-2-x), [TidBITS](https://tidbits.com/1997/06/02/email-reliance-emailer-2-0/) |
| Drag and drop | Enclosures, addresses, text clippings, and filing. | Early coverage. [MacTech, October 1995](http://preserve.mactech.com/articles/mactech/Vol.11/11.10/Oct95Newsbits/index.html) |
| Enclosure encodings | BinHex, UUencode, Base64, and service-specific formats. `Copy Entire Message` includes headers; date sorting has a direction control. | 2.0 support material reproduced in the November/December 1997 journal, p. 59. [Washington Apple Pi](https://www.wap.org/journal/showcase/washingtonapplepijournal1997v19no6.pdf) |
| Folder attachments and local subject changes | Attach whole folders; rename messages; assign account-specific default signatures. These are explicitly contrasted with Outlook Express shortcomings. | Retrospective comparison, May 1999. [ATPM](https://www.atpm.com/5.05/oe.shtml) |
| Storage | 1.x individual message files become a Mail Database plus Mail Index in 2.0. Attachments/downloads require separate consideration. | [2.0 release](https://tidbits.com/1997/04/14/emailer-2-0-available/), [Important Information](http://savagetaylor.com/til/TA43668.html) |
| Recovery | Option-launch recovery; Typical Rebuild compacts database/index; Advanced Rebuild regenerates the index and can recover deleted messages; adequate free disk space required. | 2.0v2 vendor readme. [Readme](http://savagetaylor.com/til/TA43702.html) |
| Scripting | Script menu; properties; `filter` applies Actions; window/unread fixes; 255-character subject limit in this readme. | 2.0/v2. [Macworld](https://vintageapple.org/macworld/pdf/Macworld_9707_July_1997.pdf), [v2 readme](http://savagetaylor.com/til/TA43702.html) |
| Interrupting connections | Stop cancels the current connection; Option-Stop cancels pending connections too. | 2.0v2. [Readme](http://savagetaylor.com/til/TA43702.html) |

**Additional built-in 1.x features, verified.** The Claris readme distinguishes changes introduced in 1.0v2/v3 and 1.1:

- `File > Save As Text`; an XTND translator lets other applications, including ClarisWorks, open saved Filing Cabinet messages.
- Schedules execute AppleScript documents; Connect Again reuses connection settings; Connection Status shows subject, throughput estimate, and empty-account notices.
- Native Open Transport networking; shared, read-only Internet Config settings; default Internet enclosure encoding.
- Local preferences beside the application support removable disks and multiple instances, with identifying Browser titles.
- Text-file drops into message bodies; Find Address chooses To/CC/BCC; Command-clicking email addresses works without Internet Config.
- Deleted Filing Cabinet messages move to Deleted Mail; selectable arrival sounds; folder enclosures added by dragging, rather than enclosure-selection buttons.

The readme was incorporated into Apple's library on **1998-03-16**; that is not the release date of each earlier feature. [Claris 1.x release history](http://savagetaylor.com/til/TA43586.html)

**Address book details, verified.** The vendor FAQ documents importing Eudora, AOL, Navigator, and CIS Information Manager contacts; a `+` beside the sender; dragging addresses out of message bodies; descriptions; default addresses; and export limitations, including groups omitted from ordinary export. It says groups cannot nest and describes copied group addresses. Crucially, those group descriptions are stale relative to 2.0v3's later changes; this FAQ's March 1998 incorporation date does not make every sentence accurate for v3. [Vendor address-book FAQ](http://savagetaylor.com/til/TA43296.html), [v3 report](https://tidbits.com/1998/02/16/emailers-last-gasp-update-2/)

**2.0v3 additions, verified.** Redirect keeps the original From identity so the recipient can reply to the original author. Groups follow contact changes; individual entries can be excluded from completion. The update adds Mac OS 8 contextual menus and dragging to reorganize folders. It fixes password extraction through desktop text clippings. These resolve several complaints about initial 2.0. [TidBITS, 1998-02-16](https://tidbits.com/1998/02/16/emailers-last-gasp-update-2/)

**Optional extension inventory.** Fog City's 2.0 catalog documents:

- BBEdit/Tex-Edit integration; word count, unwrap, quote reduction, dequote, text cleanup.
- RFC 934/1153 digest creation/extraction; selected-text/multiple-message forwarding.
- Eudora import/export, NewsWatcher interchange, text export, FileMaker archives/statistics.
- Templates, glossaries, mail merge, signature replacement, subject editing, address-book switching.
- Read/replied/queued/sent/schedule toggles; selective sounds, bulk forwarding, attachment stripping, shredding.
- One-way Organizer synchronization, external PGP, browser HTML, unsubscribe helpers, ASCII-text effects.

These optional downloads had differing prerequisites and support. Listings establish availability, not reliability or universal installation. [Fog City 2.0 utilities](https://www.fogcity.com/em_utilities2.0.html)

The general utilities page also documents Internet Config integration, StuffIt automation, NotifyMail-triggered mail checks, and PPP scripts that preserve an existing connection. That last detail matters: automation was expected to cooperate with other Mac applications. [Fog City general utilities](https://www.fogcity.com/em_utilities.html)

### 2.3. What users loved

**Contemporary praise concerned workflow, not just feature counts.** John Kawakami's October 1995 MacTech item emphasizes consolidation and Mac interaction conventions. Tom Negrino's July 1997 Macworld verdict calls it “the best e-mail client currently available for the Macintosh” after praising its straightforward approach. These are reviewers' judgments, not comparative performance measurements. [MacTech](http://preserve.mactech.com/articles/mactech/Vol.11/11.10/Oct95Newsbits/index.html), [Macworld, p. 52](https://vintageapple.org/macworld/pdf/Macworld_9707_July_1997.pdf)

In May 2000, Kirk McElhearn still called it “one of the most user-friendly applications I have ever used,” even while explaining his migration. In July 2002, Daniel Knight called it “the finest email client ever devised for the Mac OS” and described using an AppleScript to archive mailing-list digests into FileMaker. The affection concerned a working environment people had adapted to their own lives. [McElhearn](https://tidbits.com/2000/05/08/migrating-to-new-climes-with-powermail/), [Knight](https://lowendmac.com/2002/the-joy-of-os-x-with-classic/)

**Later recollection.** Michael Tsai's March 2023 post remembers Emailer's developers making Outlook Express and Entourage into excellent native Mac applications and criticizes the newer Outlook's controls and keyboard navigation. This supports the importance of platform feel; it is a dated personal assessment, not a test of October 2026 Outlook. A later Reddit discussion contains “Emailer was the GOAT.” That establishes continuing nostalgia, not which specific behavior deserves revival. [Tsai](https://mjtsai.com/blog/2023/03/07/microsoft-adds-free-outlook-tier/), [Reddit discussion](https://www.reddit.com/r/mac/comments/1hxr64l/what_is_the_best_email_client_to_use/)

**Interpretation for Sweetbriar:** the recurring appeal is agency. Users could process mail in batches, file it quickly, customize its meaning, and combine their mail tool with their text editor or database. Small consistent behaviors could become habits. This interpretation is stronger than concluding that an old interface or a particular database format was intrinsically superior.

### 2.4. What users disliked, and limits

- **The redesign divided reviewers.** John Rizzo's August 1997 MacUser review found 2.0 more complex and too wide for a 640 × 480 display. It reported AOL dialing failures with two specific modem setups that worked with 1.1.3. These are reported regressions, not evidence that all AOL connections failed. [MacUser, pp. 35–37](https://vintageapple.org/macuser/pdf/MacUser_9708_August_1997.pdf)
- **Rules could be difficult to reason about.** Macworld criticized multi-test logic and said scripts could trigger Actions but could not create or modify them. ATPM later contrasted Emailer's fixed effect sequence with Outlook Express's greater ordering freedom. [Macworld](https://vintageapple.org/macworld/pdf/Macworld_9707_July_1997.pdf), [ATPM, May 1999](https://www.atpm.com/5.05/oe.shtml)
- **Plain text was both a preference and a constraint.** MacTech reported lost AOL/eWorld styling and no network directory lookup. By October 1998, IMAP, HTML, and LDAP were documented competitive gaps. External “Show HTML” scripts do not prove a native HTML renderer. [MacTech](http://preserve.mactech.com/articles/mactech/Vol.11/11.10/Oct95Newsbits/index.html), [TidBITS](https://tidbits.com/1998/10/12/emailer-and-the-death-of-software/), [script catalog](https://www.fogcity.com/em_utilities2.0.html)
- **Separate environments did not secure downloaded mail.** Support material describes duplicated application/preferences folders and optional password prompts, but warns that downloaded messages remain accessible to other users of the machine. [Washington Apple Pi, p. 59](https://www.wap.org/journal/showcase/washingtonapplepijournal1997v19no6.pdf)
- **Recovery imposed work and risk.** Carlson's June 1997 follow-up describes false unread counts after a crash and an eleven-minute rebuild of a roughly 26 MB database. Vendor information warns that Norton Disk Doctor running while Emailer is open can damage the database and lose data. [Follow-up](https://tidbits.com/1997/06/09/emailer-followup/), [vendor warning](http://savagetaylor.com/til/TA43668.html)
- **Storage improvements introduced new costs.** Carlson's 2000 migration account describes copying an entire 87 MB database for backup after even one change, slow searches, and time-consuming scripted export. A locally owned archive is valuable; making it opaque and expensive to move is avoidable. [Migration account](https://tidbits.com/2000/04/24/switching-from-emailer-to-eudora-in-leaps-and-bounds/)
- **The connection model eventually felt slow.** McElhearn reports sequential account checks and messages becoming available after the connection finished. His personal comparison found PowerMail much quicker at a particular search. Neither observation is a controlled general benchmark. [PowerMail migration review](https://tidbits.com/2000/05/08/migrating-to-new-climes-with-powermail/)
- **Unsupported software creates eventual incompatibility.** McElhearn reported rebuild crashes on his Mac OS 9.0.4 machine. An Apple support note separately documents 1.1/Lite interpreting four-digit 1999 dates as 1919 and says 2.0.x is unaffected. Do not merge this into an unsupported assertion that only v3 was year-2000 safe. [McElhearn](https://tidbits.com/2000/05/08/migrating-to-new-climes-with-powermail/), [Apple date-parsing note](http://savagetaylor.com/til/TA38796.html)

Emailer also had limitations corrected within its lifetime: initial 2.0 lacked built-in redirect and live-linked groups; v3 added them. Calling these final-version deficiencies would be misleading. [1997 review](https://tidbits.com/1997/06/02/email-reliance-emailer-2-0/), [1998 update](https://tidbits.com/1998/02/16/emailers-last-gasp-update-2/)

### 2.5. Lost features: what is actually missing or weaker today?

Current documentation was checked **2026-10-11**. This is a documentary comparison, not a hands-on audit of every build, extension, or tier. Mozilla's article was last updated **2024-12-28**; its English body was accessible through another locale. A capability absent from a help page is **not automatically absent from the product**.

| Current client | What survives | Verified limitation or relevant distinction |
|---|---|---|
| Apple Mail | Rules, multiple effects, AppleScript actions, and other Mac automation remain documented. | Rules can travel through iCloud, but associated scripts and sound files do not. Sweetbriar must solve portability rather than claim that Mac mail automation vanished. [Rule settings](https://support.apple.com/en-lamr/guide/mail/cpmlprefrulesadd/mac), [automation](https://support.apple.com/en-ie/guide/mail/mlhlp1120/mac) |
| Thunderbird | Per-account filters with multiple actions, manual execution, and several automatic triggers. | Ordinary automatic filtering applies to the inbox; manual runs can target another folder. A uniform cross-account rule model would be a different design, not an invention of filtering. [Mozilla filters documentation](https://support.mozilla.org/hsb/kb/organize-your-messages-using-filters) |
| Gmail web | Filters can label, archive, delete, star, or forward; XML import/export exists. Offline mode supports reading, searching, and queued replies. | Offline mode is documented for Chrome, with configurable retained mail and per-account setup. Its filters concern Gmail's service, rather than a local automation environment spanning providers. [Filters](https://support.google.com/mail/answer/6579/create-rules-to-filter-your-emails), [offline](https://support.google.com/mail/answer/1306849?hl=en-3) |
| Outlook for Mac | Server rules with conditions, effects, ordering, stopping, and manual runs. | The current tab explicitly says client rules are no longer supported; legacy instructions appear separately. This is a concrete reduction relative to local Emailer Actions. [Microsoft documentation](https://support.microsoft.com/en-us/outlook/create-a-rule-in-outlook-for-mac) |
| Spark | Gmail labels are synchronized. | Its help explicitly says Spark cannot automatically assign Gmail labels itself and directs users to Gmail-side filters. This finding is limited to that operation, not all Spark automation. [Spark help](https://sparkmailapp.com/help/tips-tricks/gmail-labels) |
| Mimestream | A native client can edit Gmail server filters, which run while the Mac is offline. | It supports Gmail/Google Workspace accounts, rather than heterogeneous services. Server filters are useful but distinct from arbitrary local Actions. [Filters](https://mimestream.com/help/user-guide/filters), [supported accounts](https://mimestream.com/help/user-guide/supported-accounts) |

**Assessment:** three promising areas are a common automation language across services, explicit scheduling of mail movement, and first-class text-processing tools integrated with durable local data. Their universal absence from all six clients is **(unverified)**; the table establishes narrower gaps. The differentiator should be their combination and usability.

Ordinary folders, signatures, address completion, offline reading, and rules should not be marketed as resurrected inventions. Nor should “today's clients lost redirect” be asserted without separate product tests. The relevant question is whether Sweetbriar makes these operations discoverable, consistent, fast, and safe together.

### 2.6. Reimagining the worthwhile features for 2026

**The following are design proposals, not claims about Emailer or finished Sweetbriar functionality.** “Local” means implementable without a special server feature; remote changes still require the relevant backend's permissions and successful synchronization.

| Feature to revive | Proposed native macOS behavior | Local versus server requirements |
|---|---|---|
| One workbench for multiple services | Unified inbox and searches, with visible account/identity; account-specific views remain one keystroke away. Preserve service-specific operations instead of hiding differences. | Aggregation is local. Each provider needs its own backend and capability discovery. Moving across accounts is a staged copy/delete operation requiring separate success checks. |
| Schedules and Locations | Named profiles such as Work, Travel, and Offline. Separate synchronization frequency, attachment-download policy, notification windows, and sending policy. Include a visible “Check now” and temporary pause. | Scheduling is local while an authorized background process is available. Server-side scheduled sending requires provider support; notification schedules do not imply stopped synchronization. |
| Offline Out Box | A persisted queue shows Draft, Held, Ready, Submitting, Accepted, or Needs attention. Closing a window or losing connectivity must preserve body, recipients, identity, and attachment snapshots. | Local storage and queueing are client-side. Submission and draft mirroring depend on each backend. Display uncertainty after an interrupted submission rather than automatically sending another copy. |
| Actions | A readable condition builder with Any/All groups and explicitly ordered effects. Add preview on selected mail, a “why this matched” explanation, and reversible filing/tagging. | Local Actions can work across providers. Server rules require separate Sieve/Gmail/Graph adapters and cannot necessarily express scripts or local text operations. |
| Actions tied to contacts/groups | Conditions such as “sender is in Clients”; effects that propose adding a contact or group member. Use live contact references and show the addresses an expanded group will receive. | Local contacts suffice. Shared directories and server-side group semantics need separate integrations. Avoid silently modifying an organization's directory. |
| Keyboard filing and message windows | A fast folder picker, remembered destinations, separate reader/composer windows, and reliable focus after moving a message. Offer a compact table without requiring a preview pane. | Client-side interaction; actual remote filing uses backend operations. Keep the open reader attached to a stable internal message identity. |
| Named priorities | User-defined labels such as Action, Waiting, Reference, or Family, with text plus optional color. Keep personal classification distinct from outgoing importance headers. | Local metadata always possible. Synchronize through supported keywords/categories where available, with an explicit local fallback. |
| Text workbench | Commands for quote/unquote, rewrap, unwrap, selected-text reply, whitespace cleanup, and plain-text composition. Preview transformations and support Undo. | Client-side. Preserve received originals; transformed text belongs to a reply or derived view. MIME generation is a core responsibility. |
| External editors, templates, glossaries | “Edit draft in BBEdit” exchanges a temporary text document with a versioned draft. Templates declare placeholders; insertion preserves selection and Undo. | Client-side. Conflicting edits need reconciliation, and attachments/recipient fields must survive editor round trips. |
| Redirect | A discoverable handoff command with explicit original-author and sending-account information; offer forwarding the original as an attachment when true redirect is unavailable. | It requires a compatible submission path and permitted identities. Do not promise uniform behavior merely because local headers can be constructed. |
| Size-aware downloads | Headers and preview first when appropriate; per-profile body/attachment caching; a visible offline-availability indicator and explicit “Download fully.” | The local cache policy is client-side. Selective fetching depends on backend support; unavailable cached content must never look like missing mail. |
| Digests and archives | Expand a digest into an inspectable derived view; export original messages and attachments, with stable IDs, checksums, and a readable manifest. Offer database/automation integrations later. | Parsing and export are client-side. Preserve the digest source so imperfect boundary detection cannot destroy information. |
| Scripting and selective notifications | AppleScript/Shortcuts adapters plus a portable core command API; rules may call bounded extensions and choose notification behavior. | Mac adapters are platform-specific; the Rust operation model is reusable. Local scripts cannot run on a server just because a rule has been synchronized. |
| Contact completion and identity routing | Type-ahead completion with an explicit choice for ambiguous addresses; hide obsolete addresses without deleting history. Show From before sending and allow account-specific signature defaults. | Primarily local; remote directories and sending aliases depend on provider permissions. Prefer an intelligible message-level identity over hidden per-recipient routing. |

**Protocol facts constraining these proposals, checked 2026-10-11:** IMAP's mailbox-scoped UIDs and UIDVALIDITY must be represented correctly; JMAP provides a different object model, including stable Email IDs across mailbox changes. A shared UI is feasible, but a single naïve remote-ID model is not. [IMAP4rev2, RFC 9051](https://www.rfc-editor.org/rfc/rfc9051.html), [JMAP Mail, RFC 8621](https://www.rfc-editor.org/rfc/rfc8621.html)

Sieve supplies a server-side filtering language, including filing and redirect. It is a separate capability, not something every IMAP service automatically offers. JMAP's mail/submission capabilities also do not imply a universal server-rule editor. Sweetbriar should compile only supported rule subsets and retain a visible distinction between “runs on this Mac” and “runs on the server.” [Sieve, RFC 5228](https://www.rfc-editor.org/rfc/rfc5228.html), [JMAP capabilities](https://www.rfc-editor.org/rfc/rfc8621.html)

JMAP advertises `maxDelayedSend`; zero means delayed sending is unavailable. Microsoft Graph's `sendMail` returns `202 Accepted`, which explicitly does not confirm completed processing or delivery. A modern Out Box should communicate those realities instead of collapsing queueing, acceptance, and delivery into one reassuring checkmark. [JMAP submission](https://www.rfc-editor.org/rfc/rfc8621.html), [Graph sendMail](https://learn.microsoft.com/en-us/graph/api/user-sendmail?view=graph-rest-1.0)

## 3. Options compared

These are architectural judgments for Sweetbriar, based on the preceding evidence.

| Approach | Benefit | Cost or failure mode | Judgment |
|---|---|---|---|
| Recreate Emailer closely | Familiar workflow for former users; coherent interaction reference. | Recreates dial-up assumptions and inherited complexity; nostalgia is not demand evidence. | Use interaction examples, not a replica specification. |
| Provider-first client with mostly server rules | Rules continue running when every client is closed; fewer local/server conflicts. | Provider-specific semantics; limited local integrations; difficult unified automation. | Useful backend capability, insufficient as the whole product. |
| Durable local workbench with optional server rules | Consistent offline operation, portable automation, local search, heterogeneous sources. | Most engineering work: reconciliation, storage, duplicate prevention, and rule execution. | Recommended, with deliberately small initial scope. |
| Thin UI over server search and remote drafts | Smaller local store and easier initial implementation. | Weak offline guarantees; variable query behavior; network latency affects everyday work. | Conflicts with Sweetbriar's stated priorities. |

| Implementation choice | Proposed choice | Strongest argument against it |
|---|---|---|
| Native Mac UI | Swift with AppKit for message tables, editing, menus, and windows; SwiftUI where it earns its place. | More platform-specific work; two UI toolkits increase integration effort. |
| Core/UI boundary | Rust owns persistence, synchronization, MIME, search, and rule semantics; UI submits commands and observes state. | A rich boundary can become cumbersome, especially around text editing and cancellation. |
| Message storage | Immutable original blobs plus transactional metadata and independent search indexes. | Consistent backups and garbage collection span more than one file/store. |
| Automation | One portable local rule representation, capability-checked server translations, platform adapters. | Users may misunderstand where rules run; translations can lose semantics. |
| Search | Local index with precise filters and visible cache coverage; optional server-assisted searches. | Indexing, attachment extraction, and completeness reporting become substantial subsystems. |

## 4. Recommendations for Sweetbriar

### 4.1. Make durability the first revived feature

**Recommendation:** establish durable originals, versioned drafts, and recoverable settings before sophisticated Actions. Emailer's repair tools were useful, but Sweetbriar's acceptance criteria should concern preservation and restoration, not the existence of a Rebuild button.

A concrete candidate design is SQLite metadata through **`rusqlite`**, original RFC-message blobs stored independently, and a rebuildable search index. SQLite documents atomic transactions and an online backup API; its durability still rests on filesystem/hardware assumptions. These are mechanisms to evaluate, not a proof that Sweetbriar can never lose data. Documentation checked **2026-10-11**. [rusqlite](https://docs.rs/rusqlite/latest/rusqlite/), [SQLite atomic commit](https://www.sqlite.org/atomiccommit.html), [backup API](https://www.sqlite.org/backup.html)

Persist a downloaded original before committing metadata that references it. Persist draft revisions and attachment snapshots before acknowledging “Saved.” Record remote-operation intent and outcomes durably. Delay blob garbage collection until references and backup retention permit deletion. Produce a consistent backup snapshot containing metadata, required blobs, settings, and a manifest; copying an arbitrary live folder is not the proposed backup mechanism.

Define the safety promise operationally: every acknowledged save survives a process crash; interrupted migration leaves the old archive intact; failed remote filing leaves a recoverable copy; settings edits can be restored. Test disk-full errors, power-loss simulations, crashes between blob and metadata commits, damaged indexes, and restore onto a fresh installation. Physical loss of all copies requires independent backups; no database selection eliminates that requirement.

**Strongest counterargument:** separate blobs, metadata, and indexes create their own consistency problems. A single transactional database is easier to reason about initially. Start with the smallest proven storage design; choose independent blobs because recovery, large-message handling, and export justify them, not because Emailer's monolithic store was unpopular.

### 4.2. Build a narrow but complete native workflow

The first usable slice should support receiving, searching, reading, replying, saving a draft, queueing a send, keyboard filing, and restoration from backup. Every one should remain responsive while synchronization or indexing is busy. Defer digest tools, mail merge, FileMaker-style integration, and broad script execution until that slice is dependable.

Use a real Mac menu structure, clear shortcuts, independent windows, adjustable information density, and an editor that respects selection, Undo, and focus. Prototype the keyboard filing interaction early. A good practical test is whether someone can triage twenty messages, file several, and draft a reply without losing their place or waiting for the network.

For the Rust/Swift bridge, **UniFFI** is a concrete candidate: its documentation describes generated bindings for Rust libraries, including Swift. Check cancellation, threading, large-payload copying, and event delivery in a prototype before adopting it. Documentation checked **2026-10-11**. [UniFFI guide](https://mozilla.github.io/uniffi-rs/latest/)

**Strongest counterargument:** AppKit expertise and a Rust bridge add effort before the project has a product. The justification is Sweetbriar's explicit native-platform goal; keep core semantics portable while allowing the Mac and later Linux interfaces to differ.

### 4.3. Make Actions explainable, reversible, and portable

Start with sender/domain, recipient, subject, account, and contact-group conditions; file, tag, mark read, and choose notification effects. Give rules stable IDs and versions. Show a preview against actual selected messages, then record which conditions matched and which effects succeeded.

Use per-message execution records so a resync does not unintentionally repeat an auto-response or external side effect. Distinguish once-on-arrival actions from explicit manual reruns. Cross-account filing should be an inspectable operation, not an instant disappearance from the inbox. Later, add reply/forward and scripting with explicit authority and bounded execution.

Do not translate unsupported local operations into vaguely similar server rules. Explain which effects remain local, and avoid running equivalent server and client effects twice. This is a product responsibility as much as a serialization problem.

**Strongest counterargument:** predictable automation across different providers may consume years. A smaller provider-specific rules editor is easier to ship. Keep the first local Actions small and deterministic; add server editing independently.

### 4.4. Modernize Schedules around attention and transport

Offer separate controls for synchronization, notifications, attachment prefetch, and submission. A user who silences Work mail after 18:00 may still want a complete offline archive. A user on an expensive connection may want headers without attachments. An unsent draft should remain explicitly held regardless of the synchronization profile.

Show upcoming local sending times and whether the Mac must be awake. If a server can hold a scheduled submission, identify that clearly. Handle changed time zones, daylight-saving transitions, sleep, and missed runs explicitly. Batch attention without making mail arrival mysterious.

**Strongest counterargument:** a detailed scheduler can become a preferences maze. Ship a few named profiles and a temporary pause first; require actual user evidence before adding elaborate recurrence editing.

### 4.5. Restore text control while preserving interoperability

Prefer an excellent plain-text workflow, but support reading modern MIME/HTML. Keep original bytes separate from displayed text and derived transformations. A “Rename subject” operation should default to a local display title, retaining the original header and export fidelity. Quote cleanup should never rewrite a received original silently.

Evaluate **`mail-parser`** for MIME and message parsing; its current documentation describes RFC 5322/MIME handling and legacy character decoding. As checked **2026-10-11**, docs.rs lists **0.11.9**, released **2026-09-09**. Treat these as project claims to validate against a hostile and historical message corpus. [mail-parser documentation](https://docs.rs/crate/mail-parser/latest)

Offer raw `.eml` exports and a documented archive manifest before complex proprietary interchange. Add mbox import/export with an explicit dialect and round-trip tests. Preserve original attachments, not merely decoded body text. An external editor integration should exchange text without owning the full message state.

**Strongest counterargument:** plain text preferences can exclude ordinary business workflows. The recommendation is strong text tools alongside interoperable rendering and composition, with a deliberate default rather than a forced limitation.

### 4.6. Exceed Emailer at search and provider longevity

Evaluate **`tantivy`** as a Rust full-text index, with structured metadata filters alongside it. Its documentation establishes it as a search library, not an email-specific completeness or durability solution. Documentation checked **2026-10-11**. [Tantivy](https://docs.rs/tantivy/latest/tantivy/)

Proposed acceptance targets, **not measured results**: first useful local results within 100 ms for common queries in a warm 100,000-message archive on a specified test Mac; cancellation remains immediate; phrase, address, date-range, attachment-name, and account filters have reproducible semantics. Search should report whether it covers cached bodies, all known metadata, or additional server results. Rebuilding an index must not require rewriting originals.

Choose backend contracts around capabilities: remote identity, incremental changes, body fetching, submission, draft mirroring, filing, keywords, and server rules. Reuse parsing and storage across IMAP, JMAP, Gmail API, and Microsoft 365 without flattening their differences.

For Microsoft 365, make **Graph** the strategic integration. Microsoft's **2026-10-01** announcement, updated **2026-10-02**, says Exchange Online EWS retirement has begun in phases; its worldwide-cloud allow-list enforcement starts **October 10**. Do not describe this as every EWS endpoint shutting off simultaneously on October 1. On-premises Exchange is a separate compatibility decision. Checked **2026-10-11**. [Current Exchange Team announcement](https://techcommunity.microsoft.com/blog/exchange/ews-deprecation-is-here-%E2%80%93-what-this-means-to-you/4561431), [retirement scope](https://techcommunity.microsoft.com/blog/exchange/exchange-online-ews-your-time-is-almost-up/4492361)

**Strongest counterargument:** first-class support for many providers multiplies authentication, synchronization, and test obligations. Design the boundary now, prove two materially different backends, and expand after preservation and reconciliation are demonstrated.

## 5. Risks and open questions

1. **Incomplete primary documentation.** The 1.x guide was read; the full 2.0 guide remains an acquisition target. Obtain the 2.0v3 help/readme set and AppleScript dictionary, then reproduce the most important interactions in a lawful classic Mac environment. No emulator verification was performed for this report.
2. **Uncertain legal/code history.** No verified Microsoft purchase of Emailer code, public source release, or proof of source destruction was found. The 2005 user's report and surviving Fog City ownership statement are evidence of their respective claims, not a current rights audit. [Copyright Office submission](https://www.copyright.gov/orphan/comments/OW0433-Nathanson.pdf), [Fog City](https://www.fogcity.com/)
3. **Version-specific folklore.** Initial 2.0 group and redirect complaints were later addressed. Lite, full 1.x, full 2.0, and optional scripts must stay separate when planning feature revival. [v3 coverage](https://tidbits.com/1998/02/16/emailers-last-gasp-update-2/)
4. **Sentiment is selected evidence.** Enthusiasts and reviewers explain why a workflow mattered, but do not establish market size. Interview former users alongside people who prefer present clients. Test whether scripting, scheduling, or text handling actually changes their choice.
5. **Side effects and ambiguous submission.** Determine a recovery policy for interrupted sends and multi-step Actions. Graph acceptance is not delivery confirmation. A reliable client must preserve evidence and expose uncertainty rather than erase the draft or blindly resend. [Graph contract](https://learn.microsoft.com/en-us/graph/api/user-sendmail?view=graph-rest-1.0)
6. **Local versus server semantics.** Which rules must run when the Mac is shut down? How should another device see local labels, custom subject titles, and group changes? Define unsupported mappings before promising universal synchronization.
7. **Archive migration.** Test legacy charsets, BinHex/UUencode, service-specific addresses, digests, deleted-message recovery, and detached enclosures. Download archives and script catalogs alone do not prove complete export fidelity. [Encoding/attachment evidence](https://www.wap.org/journal/showcase/washingtonapplepijournal1997v19no6.pdf), [export catalog](https://www.fogcity.com/em_utilities2.0.html)
8. **Search scope.** Decide how much mail is guaranteed available offline and whether attachment contents are indexed. Define completeness independently of speed; the proposed latency target needs a fixed dataset and hardware specification.
9. **Provider policy changes.** Proton, Gmail API approvals, enterprise consent, and on-premises Exchange deserve separate current research. This report verifies the EWS transition but does not establish access arrangements for every Sweetbriar backend. [EWS transition](https://techcommunity.microsoft.com/blog/exchange/ews-deprecation-is-here-%E2%80%93-what-this-means-to-you/4561431)
10. **Nostalgia-driven scope.** The project's strongest historical lesson is coherent user control. A long checklist of scripts, old modem settings, and specialized service quirks could delay that experience rather than deliver it.

## 6. Sources consulted

The following URLs were fetched or consulted through search results, archived pages, or downloaded documents. HTTP was used where HTTPS fetching failed. The saved research includes the original manual, magazine PDFs, and extracted text. No claim relies on an unconsulted download merely because its filename was listed.

**Original documentation and developer material**

- [1.x-era User's Guide, PDF](https://archive.org/download/claris-emailer-lite-manual/Claris_Emailer_Lite_Manual.pdf); [archive catalog](https://archive.org/details/claris-emailer-lite-manual).
- [Claris Important Information](http://savagetaylor.com/til/TA43668.html).
- [2.0v2 updater readme](http://savagetaylor.com/til/TA43702.html).
- [Apple 2.0v3 distribution information](http://savagetaylor.com/til/TA37819.html).
- [Address-book FAQ](http://savagetaylor.com/til/TA43296.html).
- [Claris 1.x release history](http://savagetaylor.com/til/TA43586.html); [1.1 year-1999 issue](http://savagetaylor.com/til/TA38796.html).
- [Fog City homepage](https://www.fogcity.com/), [staff](https://www.fogcity.com/fcs_us.html), [general utilities](https://www.fogcity.com/em_utilities.html), [2.0 utilities](https://www.fogcity.com/em_utilities2.0.html).
- [Preserved Emailer 2.x downloads and MacAddict review](https://www.macintoshrepository.org/1283-claris-emailer-2-x).
- [Original restructuring press release, 1998-01-27](https://web.archive.org/web/19981206205953/http://www.claris.com/news/docs/fminc-release.html); [Goupil customer letter](https://web.archive.org/web/19981205225904/http://www.claris.com/news/docs/fminc-letter.html).
- [Apple Internet developments announcement, 1997-05-14](https://www.mactech.com/1997/05/14/md1-apple-outlines-internet-developments/).

**Contemporary reporting and reviews**

- TidBITS: [license, 1995-04-03](https://tidbits.com/1995/04/03/emailer-licensed-by-claris/); [shipment, 1995-07-31](https://tidbits.com/1995/07/31/claris-emailer-ships/); [2.0 availability, 1997-04-14](https://tidbits.com/1997/04/14/emailer-2-0-available/).
- [Jeff Carlson's 2.0 review, 1997-06-02](https://tidbits.com/1997/06/02/email-reliance-emailer-2-0/); [follow-up, 1997-06-09](https://tidbits.com/1997/06/09/emailer-followup/).
- [Claris restructuring, 1998-02-02](https://tidbits.com/1998/02/02/claris-to-restructure-as-filemaker-inc-2/); [v3 update, 1998-02-16](https://tidbits.com/1998/02/16/emailers-last-gasp-update-2/).
- [Emailer and the Death of Software, 1998-10-12](https://tidbits.com/1998/10/12/emailer-and-the-death-of-software/); [Electronic Phoenix Project, 1998-10-19](https://tidbits.com/1998/10/19/electronic-phoenix-project/).
- [MacTech, John Kawakami, October 1995](http://preserve.mactech.com/articles/mactech/Vol.11/11.10/Oct95Newsbits/index.html).
- [Macworld, Tom Negrino, July 1997, p. 52](https://vintageapple.org/macworld/pdf/Macworld_9707_July_1997.pdf).
- [MacUser, John Rizzo, August 1997, pp. 35–37](https://vintageapple.org/macuser/pdf/MacUser_9708_August_1997.pdf).
- [Washington Apple Pi Journal, November/December 1997, pp. 32–34 and 59](https://www.wap.org/journal/showcase/washingtonapplepijournal1997v19no6.pdf).
- [William Lovett, Outlook Express 4.5 comparison, May 1999](https://www.atpm.com/5.05/oe.shtml).
- [Carlson's migration to Eudora, 2000-04-24](https://tidbits.com/2000/04/24/switching-from-emailer-to-eudora-in-leaps-and-bounds/); [McElhearn's migration to PowerMail, 2000-05-08](https://tidbits.com/2000/05/08/migrating-to-new-climes-with-powermail/).

**Retrospectives and testimony**

- [Daniel Knight, 2002-07-05](https://lowendmac.com/2002/the-joy-of-os-x-with-classic/).
- [Omar Shahine, 2004 account, archived](https://web.archive.org/web/20060503185524/http://blogs.msdn.com/omars/archive/2004/04/05/107468.aspx).
- [Entourage Help Blog history, 2007, archived](https://web.archive.org/web/20080624084550/http:/blog.entourage.mvps.org/2007/05/in_the_beginning.html).
- [Dave Nathanson, Copyright Office orphan-works submission, 2005](https://www.copyright.gov/orphan/comments/OW0433-Nathanson.pdf).
- [Michael Tsai, 2023-03-07](https://mjtsai.com/blog/2023/03/07/microsoft-adds-free-outlook-tier/).
- [Reddit discussion containing Emailer reminiscence](https://www.reddit.com/r/mac/comments/1hxr64l/what_is_the_best_email_client_to_use/).

**Current clients, standards, and implementation references — checked 2026-10-11**

- Apple Mail: [automation](https://support.apple.com/en-ie/guide/mail/mlhlp1120/mac), [rule settings](https://support.apple.com/en-lamr/guide/mail/cpmlprefrulesadd/mac).
- [Thunderbird filters](https://support.mozilla.org/hsb/kb/organize-your-messages-using-filters).
- Gmail: [filters](https://support.google.com/mail/answer/6579/create-rules-to-filter-your-emails), [offline](https://support.google.com/mail/answer/1306849?hl=en-3).
- [Outlook for Mac rules](https://support.microsoft.com/en-us/outlook/create-a-rule-in-outlook-for-mac).
- [Spark Gmail labels](https://sparkmailapp.com/help/tips-tricks/gmail-labels).
- Mimestream: [filters](https://mimestream.com/help/user-guide/filters), [accounts](https://mimestream.com/help/user-guide/supported-accounts).
- [RFC 9051, IMAP4rev2](https://www.rfc-editor.org/rfc/rfc9051.html), [RFC 8621, JMAP Mail](https://www.rfc-editor.org/rfc/rfc8621.html), [RFC 5228, Sieve](https://www.rfc-editor.org/rfc/rfc5228.html).
- [Graph sendMail](https://learn.microsoft.com/en-us/graph/api/user-sendmail?view=graph-rest-1.0).
- Exchange Team: [October 2026 EWS changes](https://techcommunity.microsoft.com/blog/exchange/ews-deprecation-is-here-%E2%80%93-what-this-means-to-you/4561431), [retirement scope and phased plan](https://techcommunity.microsoft.com/blog/exchange/exchange-online-ews-your-time-is-almost-up/4492361).
- [SQLite atomic commit](https://www.sqlite.org/atomiccommit.html), [online backup](https://www.sqlite.org/backup.html), [rusqlite](https://docs.rs/rusqlite/latest/rusqlite/).
- [Tantivy](https://docs.rs/tantivy/latest/tantivy/), [mail-parser](https://docs.rs/crate/mail-parser/latest), [UniFFI](https://mozilla.github.io/uniffi-rs/latest/).
