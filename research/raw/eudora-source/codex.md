<!-- provenance: helper=codex model=gpt-6.1-sol started=2026-10-11T18:16:34+00:00 exit=0 brief=research/briefs/ -->

# Eudora for Mac: verified source inventory for Sweetbriar

Research date: **2026-10-11**. Primary evidence: the supplied `./src` checkout, mirrored at [pmachapman/eudora-mac](https://github.com/pmachapman/eudora-mac). Saved to [report.md](codex-artifacts/report.md).

## 1. TL;DR

- **Version qualification:** CHM dates the final Mac **6.2.4** release to **2006-10-11**, but this checkout identifies itself as **6.2.5**, build **9**. This report inventories the supplied source; exact equivalence to the released 6.2.4 binary is **unverified**. [CHM release account](https://computerhistory.org/blog/the-eudora-email-client-source-code/); [Include/conf.h:108][src/Include/conf.h], [Include/buildversion.h:22][src/Include/buildversion.h].
- Eudora's depth lies in composable operations: **15 filter operators**, nickname/address-book set membership, automatic stationery replies, multiple personalities, editable summaries, server dispositions, and keyboard modifiers. [common.rsrc.r:5823][src/common.rsrc.r], [FiltDefs.r:67][src/FiltDefs.r], [menu.c:1632][src/menu.c].
- The audit extracted **78 physical `MENU` definitions**, including conditional/debug variants, and **47 settings `DITL` definitions**: 29 standard, including the dialog shell and POP/IMAP variants, plus 18 Esoteric panels. These are definition counts, not simultaneously visible menus or distinct customer panels. [Common.r:539][src/Common.r], [SettingsTwo.rsrc.r:386][src/SettingsTwo.rsrc.r], [EsotericSettings60.r:45][src/EsotericSettings60.r]; [extraction script](codex-artifacts/audit/extract-resources.py).
- Standout reading features include the **Content Concentrator**, multiple-message preview/reply, quantitative junk scoring, MoodWatch, ScamWatch, speech rules, Link History, and local mail-use statistics. [SettingsTwo.rsrc.r:2813][src/SettingsTwo.rsrc.r], [anal.c:445][src/anal.c], [HelpText.r:35][src/HelpText.r], [speechutil.c:952][src/speechutil.c], [carbon.r:15][src/carbon.r].
- **Mac storage differs from Windows Eudora:** mailbox data and its TOC can occupy the same file's data/resource forks; separate `.toc` files are an alternative. [toc.c:285, 332][src/toc.c], [SettingsTwo.rsrc.r:1639][src/SettingsTwo.rsrc.r].
- Settings are unusually inspectable: the supplied catalogue contains **1,033 documented setting IDs**, including ordinary options, hidden controls, strings, and internal/obsolete entries; it must not be described as 1,033 active user preferences. [X-Eudora-Settings.txt:1][src/X-Eudora-Settings.txt], [prefs.xset:364][src/prefs.xset]; [complete extracted catalogue](codex-artifacts/audit/x-eudora-settings.csv).
- **Recommendation:** recreate the workflows in a platform-neutral Rust core with a native Mac interface, immutable message originals, durable local metadata and operation logs, and disposable search indexes. Reusing the historical UI/storage implementation would import its platform and recovery constraints. The relevant evidence and trade-offs follow.

## 2. Findings

### 2.1 Scope, provenance and evidence rules

CHM announced the source release on **2018-05-22** and says it added its licence/copyright, sanitized comments, and removed third-party material it could not distribute. The checked-in C files contain BSD-style redistribution conditions and an explicit absence of patent-rights grants. CHM separately excludes use of Eudora trademarks/domain names. These are observed licence statements, not a legal clearance of every bundled component. [CHM](https://computerhistory.org/blog/the-eudora-email-client-source-code/); [toc.c:1][src/toc.c].

**Evidence labels:** a menu or dialog declaration proves a resource exists; a handler proves an implementation path exists; neither proves an external plug-in was installed, a conditional build shipped, or the historical binary behaved correctly. `DEBUG`, `NEVER`, `WINTERTREE`, `DIAL`, `LIGHT`, Carbon/classic resources, and paid-feature gates are identified where material. No historical executable was run and no build was attempted.

Most text uses classic Mac **CR** line endings and MacRoman characters. For reproducibility, `audit/source/` contains UTF-8 transcriptions with CR normalized to LF; `src/` was not edited. Citations use original relative source paths and logical line numbers after that newline normalization. [manifest](codex-artifacts/audit/source-manifest.json), [resource extractor](codex-artifacts/audit/extract-resources.py).

### 2.2 Source-tree map

| Area | Main source and verified responsibility |
|---|---|
| Application/platform shell | `main.c`, `ends.c`, `mywindow.c`, `winutil.c`, `menu.c`, `carbonutil.c`, `appear_util.c`: lifecycle, events, windows, menu construction and Mac integration. Startup constructs fixed and dynamic menus in [ends.c:2280][src/ends.c]. `Include/` holds interfaces and shared structures; CodeWarrior project files and `makefile` describe historical builds. [makefile:1][src/makefile] |
| Mailboxes and messages | `mailbox.c`, `mbwin.c`, `boxact.c`, `message.c`, `messact.c`, `compact.c`, `comp.c`, `mailxfer.c`: mailbox views, summary/message actions, composition and transfers. Explicit selection-dependent command handling is in [menu.c:879][src/menu.c]. |
| Storage and recovery | `toc.c`, `buildtoc.c`, `squish.c`, `fileutil.c`, `spool.c`; TOC validation/read/write, rebuilding, append/delete/compaction and temporary attachment copies. [toc.c:26][src/toc.c], [buildtoc.c:1][src/buildtoc.c], [spool.c:1][src/spool.c]. `mstore.c`, `msmaildb.c`, `mstoc.c`, `msiddb.c` are a separate mail-store experiment; see storage qualification below. |
| POP/SMTP | `pop.c`, `sendmail.c`, `tcp.c`, `ssl.c`, `OpenSSL.cp`, `sslCerts.cp`, `sasl.c`, `dial.c`, `ctb.c`, `uupc.c`: receive/send, authentication, SSL and legacy connection methods. [sendmail.c:389][src/sendmail.c], [pop.c:1][src/pop.c]. |
| IMAP | `imapmailboxes.c`, `imapdownload.c`, `imapconnections.c`, `imapnetlib.c`, `imapauth.c` and related `Include/imap*.h`; `CrispinIMAP/mail.c`, `imap4r1.c`, `rfc822.c`, `auths.c`, `auth_log.c` implement the separate c-client protocol layer. [CrispinIMAP/imap4r1.c:1][src/CrispinIMAP/imap4r1.c], [imapconnections.c:1][src/imapconnections.c]. |
| Filtering/search | `FiltDefs.r/.c`, `filters.c`, `filtwin.c`, `filtrun.c`, `filtthread.c`, `filtmng.c`, `makefilter.c`; `search.c`, `searchwin.c`, `regexp.c`: rule serialization/UI/execution, message-derived rules, text and multi-mailbox search. [FiltDefs.r:1][src/FiltDefs.r], [common.rsrc.r:6257][src/common.rsrc.r]. |
| Nicknames/contacts | `nickmng.c`, `nickwin.c`, `nickexp.c`, `nickae.c`, `nick.r`, `AddressBookTabs/`, `vcard.c`, `osxabsync.cp`, `palmconduitae.c`: nickname books, expansion/history, contact tabs, vCards, one-way Mac Address Book import and PDA integration. [nickexp.c:80][src/nickexp.c], [osxabsync.cp:28][src/osxabsync.cp]. |
| Personalities/preferences | `schizo.c`, `personalitieswin.c`, `prefs.c`, `prefenable.c`, `prefaudit.c`, `preflimits.c`, `acap.c`: account identities, preference inheritance, panels, validation and configuration-service integration. [SettingsTwo.rsrc.r:1700][src/SettingsTwo.rsrc.r], [prefs.c:1][src/prefs.c]. |
| Editing/MIME/HTML | `Editor/Source/` is PETE, the native text editor; `peteglue.c`, `fmtbar.c`, `rich.c`, `mime.c`, `html.c`, `newhtml.c`, `unicode.c`, `binhex.c`, `uudecode.c` connect rich editing, mail parsing/encoding, inline graphics and character conversion. [two.rsrc.r:419][src/two.rsrc.r], [sendmail.c:1017][src/sendmail.c]. |
| MoodWatch/spelling | `anal.c`, `Include/anal.h` wrap an external TAE analyzer/dictionary; `spell.c` wraps Wintertree SSCE under `WINTERTREE`. These are integration code, not proof that the third-party engines/data are fully available in the release. [anal.c:331][src/anal.c], [spell.c:26][src/spell.c]. |
| Templates/signatures | `stationerywin.c`, `signaturewin.c`, composition handlers and menu builders manage separate reusable stationery and signatures. [stationerywin.c:74][src/stationerywin.c], [ends.c:3247][src/ends.c]. |
| Extensions/automation | `trans.c`, `TransStream.c`, `Include/emsapi-mac.h` implement EMSAPI translators, attachers, junk hooks and extension callbacks; `aete.r`, `appleevent.c`, `aeutil.c`, `nickae.c`, `scriptmenu.c`, `menusharing.c` cover Apple events, Scripts and shared menus. [Include/emsapi-mac.h:154][src/Include/emsapi-mac.h], [aete.r:148][src/aete.r]. |
| Productivity/status | `concentrator.c`, `linkmng.c`, `linkwin.c`, `statmng.c`, `statwin.c`, `graph.c`, `speechutil.c`, `emoticon.c`, `toolbar.c`, `tabmania.c`, `mbdrawer.c`, `taskProgress.c`: concentrated reading, URL history, statistics, speech, toolbar/tab/drawer UI and background task feedback. [concentrator.c:79][src/concentrator.c], [speechutil.c:511][src/speechutil.c], [carbon.r:15][src/carbon.r]. |
| Resources/help | `Common.r` assembles dictionaries, strings, address tabs and mappings; `common.rsrc.r`, `two.rsrc.r`, `SettingsTwo.rsrc.r`, `EsotericSettings60.r`, `FiltDefs.r`, `carbon.r`, `classic.r`, `help.r`, `HelpText.r`, `HelpTEXT/`, `HelpMenuDefs*`, `HelpDlogDefs*`, `PrefDefs*`, `StringDefs*` supply menus/dialogs/help/defaults. [Common.r:1][src/Common.r], [HelpText.r:14][src/HelpText.r]. Several apparent `.rsrc`/help files are zero-byte placeholders in this checkout: use the expanded text, not the placeholder as evidence of absence. |
| Commercial infrastructure | `adwin.c`, `adutil.c`, `audit.c`, `nag.c`, `register.c`, `paywin.c`, `light.c`: advertising, registration, usage audit and feature gating; relevant to explaining conditional UI, not a proposed Sweetbriar feature. [ends.c:2631][src/ends.c], [menu.c:1701][src/menu.c]. |

### 2.3 Complete menu-resource inventory

The following tables include **every `MENU` resource found in the supplied text**, including empty/dynamic menus and duplicate platform/role variants. Entries retain source order; **—** means a separator. Parent commands named Text, Find, Sort, Change, Attach, and Plug-in Settings open the correspondingly named submenu; account-, mailbox-, recipient- and plug-in-specific children are built at runtime. Resource IDs differ from the internal menu IDs: most fixed resources add 1,000. [ends.c:2280, 2331][src/ends.c].

**Menu bar**

| Resource ID / menu | Every declared entry, in order | Source |
|---|---|---|
| 1503 · Edit | Undo; —; Cut; Copy; Paste; Paste As Quotation; Clear; —; Text; —; Select All; Wrap Selection; Finish Nickname; Insert Recipient; Insert Emoticon; —; Message Plug–ins; Speak; Dial; Spelling; Add Word Service... | [Common.r:539][src/Common.r] |
| 1554 · Scripts | Open Scripts Folder; — | [Common.r:595][src/Common.r] |
| 515 · Help | (empty; contents supplied at runtime) | [Common.r:2873][src/Common.r] |
| 1514 · Window | Minimize Window; Bring All to Front; Send to Back; Tabs; Drawer; —; Address Book; Directory Services; Filters; Link History; Mailboxes; Personalities; Signatures; Stationery; Statistics; Task Progress; — | [carbon.r:15][src/carbon.r] |
| 1514 · Window | Send to Back; Tabs; —; Address Book; Directory Services; Filters; Link History; Mailboxes; Personalities; Signatures; Stationery; Statistics; Task Progress; — | [classic.r:4][src/classic.r] |
| 1504 · Mailbox | In; Out; Junk; Trash; — | [common.rsrc.r:5703][src/common.rsrc.r] |
| 1501 · Apple | About Eudora...; — | [common.rsrc.r:5773][src/common.rsrc.r] |
| 1506 · Transfer | –> In; –> Out; –> Junk; –> Trash; — | [common.rsrc.r:5921][src/common.rsrc.r] |
| 1507 · Special | Filter Messages; Make Address Book Entry...; Make Filter...; Find; Sort; —; Settings...; Plug–in Settings; —; Change Password...; Forget Password; Empty Trash | [common.rsrc.r:6121][src/common.rsrc.r] |
| 1502 · File | New Text Document; Open...; Open Selection; Open in Browser; —; Close; Save; Save As...; —; Import Mail...; Export...; —; Send Queued Messages; Check Mail; —; Page Setup...; Print...; Print One Copy; —; Quit | [two.rsrc.r:527][src/two.rsrc.r] |
| 1505 · Message | New Message; Reply; Forward; Redirect; Send Again; —; New Message To; Forward To; Redirect To; —; New Message With; Reply With; —; Attach Document...; Attach; Queue For Delivery; Change; —; Junk; Not Junk; —; Delete | [two.rsrc.r:577][src/two.rsrc.r] |

**Submenus, filter/search controls and auxiliary popups**

| Resource ID / menu | Every declared entry, in order | Source |
|---|---|---|
| 1553 · ImporterSelect | Search disk for all ... | [Common.r:1843][src/Common.r] |
| 1535 · FActions | None; —; Make Status; Make Priority; Make Label; Make Personality; Make Subject; —; Play Sound; Speak; Open; Print; Remember Sender; Notify User; —; Forward To; Redirect To; Reply With; —; Server Options; —; Copy To; Transfer To; Junk; Move Attachments; —; Skip Rest | [FiltDefs.r:67][src/FiltDefs.r] |
| 1201 · Find | Find...; Find Again; Enter Selection; —; Search; Search All; Search Mailbox; Search Mailfolder; —; Search Web for Selection | [common.rsrc.r:5723][src/common.rsrc.r] |
| 1512 · unnamed popup | (empty; contents supplied at runtime) | [common.rsrc.r:5753][src/common.rsrc.r] |
| 1513 · unnamed popup | (empty; contents supplied at runtime) | [common.rsrc.r:5763][src/common.rsrc.r] |
| 1210 · Priority | (Highest); (High); (Normal); (Low); (Lowest); —; Raise; Lower | [common.rsrc.r:5787][src/common.rsrc.r] |
| 1213 · Transliteration | (empty; contents supplied at runtime) | [common.rsrc.r:5813][src/common.rsrc.r] |
| 1516 · MT1 | contains; does not contain; is; is not; starts with; ends with; appears; does not appear; intersects nickname; doesn’t intersect nickname; intersects address book; doesn’t intersect address book; matches regular expression; is less than; is greater than | [common.rsrc.r:5823][src/common.rsrc.r] |
| 1517 · MT2 | contains; does not contain; is; is not; starts with; ends with; appears; does not appear; intersects nickname; doesn’t intersect nickname; intersects address book; doesn’t intersect address book; matches regular expression; is less than; is greater than | [common.rsrc.r:5863][src/common.rsrc.r] |
| 1518 · Cj | ignore; and; or; unless | [common.rsrc.r:5903][src/common.rsrc.r] |
| 1209 · State | Unread; Read; Replied; Forwarded; Redirected; Recovered; —; Unsendable; Sendable; Queued; Timed Queue; Sent; Unsent; Transfer Error; Being Sent | [common.rsrc.r:5941][src/common.rsrc.r] |
| 1214 · Signature Popup | None; Standard; Alternate | [common.rsrc.r:5981][src/common.rsrc.r] |
| 1521 · NickFile | (empty; contents supplied at runtime) | [common.rsrc.r:5997][src/common.rsrc.r] |
| 1523 · PhXlIn | None | [common.rsrc.r:6007][src/common.rsrc.r] |
| 1524 · PhXlOut | None | [common.rsrc.r:6019][src/common.rsrc.r] |
| 1526 · FiltSound | (empty; contents supplied at runtime) | [common.rsrc.r:6031][src/common.rsrc.r] |
| 1216 · Color | No Color; —; Red; Green; Blue; Yellow; Brown | [common.rsrc.r:6041][src/common.rsrc.r] |
| 1220 · Translation Services | (empty; contents supplied at runtime) | [common.rsrc.r:6065][src/common.rsrc.r] |
| 220 · Process | (empty; contents supplied at runtime) | [common.rsrc.r:6075][src/common.rsrc.r] |
| 1222 · Attach Translators | (empty; contents supplied at runtime) | [common.rsrc.r:6085][src/common.rsrc.r] |
| 1223 · Translator Settings | (empty; contents supplied at runtime) | [common.rsrc.r:6095][src/common.rsrc.r] |
| 1224 · Window Properties | Tabs; Floating; Dockable | [common.rsrc.r:6105][src/common.rsrc.r] |
| 1519 · Signature Popup | None; Standard; Alternate | [common.rsrc.r:6155][src/common.rsrc.r] |
| 1537 · Priority | (Highest); (High); (Normal); (Low); (Lowest); —; Raise; Lower | [common.rsrc.r:6171][src/common.rsrc.r] |
| 1536 · Filt Status | Unread; Read; Replied; Forwarded; Redirected; Recovered; —; Unsendable; Sendable; Queued; Timed Queue; Sent; Unsent; Transfer Error; Being Sent | [common.rsrc.r:6197][src/common.rsrc.r] |
| 1227 · InternalSpeller | Check Spelling; Find Next Misspelling; —; Add to Dictionary; Remove from Dictionary | [common.rsrc.r:6237][src/common.rsrc.r] |
| 1543 · SrchCategory | Anywhere; Headers; Body; Attachment Name(s); —; Summary; Status; Junk Score; Priority; Attachment Count; Label; Date; Size (K); Age; Personality; —; To:; From:; Subject:; Cc:; Bcc:; Any Recipient | [common.rsrc.r:6257][src/common.rsrc.r] |
| 1522 · CommonH | To:; Cc:; From:; Subject:; Reply-To:; —; «Any Recipient»; «Any Header»; «Junk Score»; «Body» | [common.rsrc.r:6311][src/common.rsrc.r] |
| 1544 · SrchEqual | is; is not | [common.rsrc.r:6341][src/common.rsrc.r] |
| 1545 · SrchCompare | is; is not; is greater than; is less than | [common.rsrc.r:6355][src/common.rsrc.r] |
| 1546 · SrchTextCompare | contains; contains word; does not contain; is; is not; starts with; ends with; matches regexp | [common.rsrc.r:6373][src/common.rsrc.r] |
| 1547 · Age units | Minutes; Hours; Days; Weeks; Months; Years | [common.rsrc.r:6399][src/common.rsrc.r] |
| 1548 · AndOr | Match all; Match any | [common.rsrc.r:6421][src/common.rsrc.r] |
| 1549 · SrchCompDate | is; is not; is after; is before | [common.rsrc.r:6435][src/common.rsrc.r] |
| 1550 · Filt Voice | (empty; contents supplied at runtime) | [common.rsrc.r:6453][src/common.rsrc.r] |
| 1225 · Server Opts | Do Nothing; Fetch from Server; Delete from Server; Fetch & Delete | [common.rsrc.r:6463][src/common.rsrc.r] |
| 1551 · Time | Day; Week; Month; Year | [common.rsrc.r:6481][src/common.rsrc.r] |
| 1208 · Sort | by Status; by Junk Score; by Priority; by Attachments; by Label; by Sender; by Date; by Size; by Mailbox; by Mood; by Subject; —; Group Subjects | [two.rsrc.r:327][src/two.rsrc.r] |
| 1211 · Label | None; — | [two.rsrc.r:363][src/two.rsrc.r] |
| 1212 · Change | Queueing...; Status; Priority; Label; Personality; Server Options | [two.rsrc.r:377][src/two.rsrc.r] |
| 1217 · Font | (empty; contents supplied at runtime) | [two.rsrc.r:399][src/two.rsrc.r] |
| 1218 · Margins | (empty; contents supplied at runtime) | [two.rsrc.r:409][src/two.rsrc.r] |
| 1219 · Text | Plain; Bold; Italic; Underline; —; Quote; Unquote; —; Left; Center; Right; —; Margins; Color; Font; Size; —; Insert Horizontal Rule; Insert Graphic...; Link to URL... | [two.rsrc.r:419][src/two.rsrc.r] |
| 1221 · Personalities | «Dominant»; — | [two.rsrc.r:469][src/two.rsrc.r] |
| 1556 · Address Books | (empty; contents supplied at runtime) | [two.rsrc.r:483][src/two.rsrc.r] |
| 1557 · Address Books | (empty; contents supplied at runtime) | [two.rsrc.r:493][src/two.rsrc.r] |
| 1226 · Size | Smallest; Smaller; Small; Normal; Big; Bigger; Biggest | [two.rsrc.r:503][src/two.rsrc.r] |
| 1520 · Attach | AppleDouble ("MIME"); AppleSingle; BinHex; Uuencode Data Fork | [two.rsrc.r:631][src/two.rsrc.r] |
| 1525 · LabelPop | None; — | [two.rsrc.r:649][src/two.rsrc.r] |
| 1539 · Filt Personalities | «Dominant»; — | [two.rsrc.r:663][src/two.rsrc.r] |
| 1540 · Filt Station | (empty; contents supplied at runtime) | [two.rsrc.r:677][src/two.rsrc.r] |
| 1541 · Network | Don’t check when not connected; Don’t check when on batteries; Offline (never check) | [two.rsrc.r:687][src/two.rsrc.r] |
| 1563 · Options | Resynchronize Mailbox; Resynchronize Mailfolder; Resynchronize on Mail Check; —; Refresh Mailbox List; —; Show Deleted Messages; Remove Deleted Messages; Compact Mailbox | [two.rsrc.r:703][src/two.rsrc.r] |
| 229 · ConConProfPreview | Single Message | [two.rsrc.r:731][src/two.rsrc.r] |

**Settings popups**

| Resource ID / menu | Every declared entry, in order | Source |
|---|---|---|
| 1510 · unnamed popup | (empty; contents supplied at runtime) | [SettingsTwo.rsrc.r:3799][src/SettingsTwo.rsrc.r] |
| 1511 · unnamed popup | (empty; contents supplied at runtime) | [SettingsTwo.rsrc.r:3809][src/SettingsTwo.rsrc.r] |
| 1530 · PersPopup | «Dominant»; — | [SettingsTwo.rsrc.r:3819][src/SettingsTwo.rsrc.r] |
| 1531 · Stationery Popup | None; — | [SettingsTwo.rsrc.r:3833][src/SettingsTwo.rsrc.r] |
| 1552 · Graph Type | Line; Area; Bar; Circle; Diamond; Square | [SettingsTwo.rsrc.r:3847][src/SettingsTwo.rsrc.r] |
| 1555 · SSL | None; Required (TLS); Optional (TLS); Required (Alternate Port) | [SettingsTwo.rsrc.r:3869][src/SettingsTwo.rsrc.r] |
| 1558 · unnamed popup | None; — | [SettingsTwo.rsrc.r:3887][src/SettingsTwo.rsrc.r] |
| 1559 · PreviewProfile | None; — | [SettingsTwo.rsrc.r:3901][src/SettingsTwo.rsrc.r] |
| 1560 · MessageProfile | None; — | [SettingsTwo.rsrc.r:3915][src/SettingsTwo.rsrc.r] |
| 1561 · MultiPreviewProfile | None; — | [SettingsTwo.rsrc.r:3929][src/SettingsTwo.rsrc.r] |
| 1562 · SMTPRelayPersonality | None; — | [SettingsTwo.rsrc.r:3943][src/SettingsTwo.rsrc.r] |

**Conditional debug menus**

| Resource ID / menu | Every declared entry, in order | Source |
|---|---|---|
| 1509 · Db | 0 No offscreen bm's; 1 PeteBusy; 2 Serial #'s; 3 Random Nicknames Events; 4 Fast ad scheduling; 5 No Double-buffs; 6 Repeat Mailchecks; 7 Show arrival times; 8 UseResFile; 9 No Temp Mem; 10 Menu Profile; 11 Facetime accelerator; 12 ports +10000; 13 4X TCP Buffer; 14 Visible composition; 15 Message fetch trace; —; Debugger!; Collect IMAP message flags; Reset Leaks; Ads; Assign a command key; Test spooler; Dump win->pte; Debug Nagging; Dump Control Hierarchy; Toggle ad window; Audit Now; Test Matching; Add email received stat; Add email sent stat | [Common.r:158][src/Common.r] |
| 1228 · Ad Select | Request new playlist; Reload playlist; Dump playlists; — | [Common.r:195][src/Common.r] |

`Dial` requires `DIAL`; `Spelling` requires `WINTERTREE`. The Db and Ad Select menus require `DEBUG` and nonproduction execution; they are included for completeness, not attributed to the shipping customer interface. [Common.r:158, 539][src/Common.r], [ends.c:2320][src/ends.c].

**Runtime additions and transformations.** Mailbox/Transfer add the actual folder hierarchy; recipient submenus use Quick Recipients; stationery supplies New Message With/Reply With; signatures and personalities add user entries; labels add configured/Finder label names; Font uses installed fonts; Margins adds configured presets and bullets; Scripts adds actual scripts; EMSAPI adds installed translators/attachers/special commands; Word Services and Frontier sharing can add further commands. These sets have no fixed finite command-name inventory. [ends.c:2331, 2367, 3247, 3350][src/ends.c], [menu.c:2811][src/menu.c], [ends.c:2678][src/ends.c].

**Modifier commands** are part of the inventory even though they are not separate resource items: Save All/Close All/Print Selection; Copy Unwrap, Copy Unwrap Plain and Copy Plain; Paste Plain; Unwrap Selection; expanded nickname completion/insertion; Speak Selection; Reply-to-All reversal and selected-text reply; Change Queueing/Send; Redirect Without Delete; permanent delete; descending sort; selected-text Make Nickname; special send/check dialogs; and selected-personality password/trash actions. Captions and availability change with Option/Shift and preferences. Saved searches also append named commands to Find. [menu.c:1632][src/menu.c], [messact.c:1129][src/messact.c], [searchwin.c:4708][src/searchwin.c].

**Fixed runtime menu additions:** Mailbox/Transfer insert New... and Other..., with This Mailbox for IMAP hierarchy nodes; Margins defaults to Normal, —, Indent, Indent 2, Indent 3, Indent 4, —, With Bullets. Transliteration can be appended to Change when custom tables are loaded. Named Sort presets can be appended, but the supplied `SortsStrn` list is empty. [ends.c:3141][src/ends.c], [Include/StringDefs.h:126][src/Include/StringDefs.h], [ends.c:3350][src/ends.c], [StrnDefs.strn:417][src/StrnDefs.strn], [ends.c:2401, 2484][src/ends.c], [StrnDefs.strn:1268][src/StrnDefs.strn].

### 2.4 Settings panels: complete options in the supplied definitions

This table lists every named standard settings `DITL`, including both POP/IMAP variants. It omits decorative labels and repeats shared controls where they occur. `°mPrs`, `°junk`, `°conc`, `°spel` are resource-name feature qualifiers, not words displayed to users; `•244` selects the IMAP variant. The outer dialog **2000** supplies OK, Cancel, Help and the panel selector. Numeric `§§` markers bind controls to preferences, bits, resources or panel callbacks; some negative markers invert the stored meaning. [SettingsTwo.rsrc.r:386][src/SettingsTwo.rsrc.r], [prefs.c:1][src/prefs.c].

| Panel · resource | All configurable options/controls | Source |
|---|---|---|
| Getting Started · 2001 | User name; mail server; real name; SMTP server; email address; make Eudora default mailer. | [SettingsTwo.rsrc.r:2891][src/SettingsTwo.rsrc.r] |
| Checking Mail, POP · 2002 | User/server; POP or IMAP; passwords/Kerberos/APOP; overlap commands; enable automatic check and minutes; suppress battery checks; send on check; leave on server and days; delete from server when emptied from Trash; skip messages over K. | [SettingsTwo.rsrc.r:420][src/SettingsTwo.rsrc.r] |
| Checking Mail, IMAP · 3003 | User/host; POP or IMAP; location prefix; automatic check and minutes; suppress battery checks; send on check; minimal headers, full message except attachments over K, or full message including attachments. | [SettingsTwo.rsrc.r:2348][src/SettingsTwo.rsrc.r] |
| Sending Mail · 2003 | Email address; default domain; SMTP server; submission port 587; allow authorization; SMTP relay personality; immediate send; send on check; separate sending thread; fix curly quotes; keep copies; automatically Fcc to original mailbox. | [SettingsTwo.rsrc.r:588][src/SettingsTwo.rsrc.r] |
| Composing Mail · 2004 | Email address; default domain; autocomplete/expand nicknames; stationery; signature; word wrap; permit quoted-printable; keep outgoing copies. | [SettingsTwo.rsrc.r:729][src/SettingsTwo.rsrc.r] |
| Junk Mail · 2005 | Threshold slider **0–100**; address-book sender exemption; add Not Junk senders to address book; hold junk in Junk; never mark Junk unread; automatic age trimming and days; warn before trimming; use Cmd-J for Junk. | [SettingsTwo.rsrc.r:2702][src/SettingsTwo.rsrc.r] |
| Attachments · 2006 | AppleDouble/AppleSingle/BinHex/uuencode-data-fork; always include Macintosh information; attachment folder; application ownership of TEXT files; trash attachments with messages; receive MIME digests as attachments. | [SettingsTwo.rsrc.r:3102][src/SettingsTwo.rsrc.r] |
| Replying · 2007 | Reply-to-all default versus Option-key reversal; include self; put original To recipients in Cc; treat mailing lists specially; inherit original priority; generate multiple replies to multiple messages. | [SettingsTwo.rsrc.r:3191][src/SettingsTwo.rsrc.r] |
| Fonts & Display · 2008 | Screen/fixed-width/print fonts and sizes; Quartz rendering; text/quote/background colors; message-window width/height; zoom on opening; display graphics; picture emoticons; formatting toolbar; live scrollbars; automatic HTML-graphic download; animated GIFs; live resizing. | [SettingsTwo.rsrc.r:2486][src/SettingsTwo.rsrc.r] |
| Mailbox Display · 2009 | Show status, priority, attachments, label, who, date, size, server, mood, junk columns; subject always shown; horizontal/vertical separators; selected-message count; default previews; mark-read delay; mark read on clicking/tabbing/scrolling, leaving via next-message, or deletion; single/multiple preview profiles. | [SettingsTwo.rsrc.r:845][src/SettingsTwo.rsrc.r] |
| Date Display · 2010 | Age-sensitive/fixed format; local/sender timezone. | [SettingsTwo.rsrc.r:3277][src/SettingsTwo.rsrc.r] |
| Eudora Labels · 2011 | **Eight** label names and corresponding color controls. | [SettingsTwo.rsrc.r:1026][src/SettingsTwo.rsrc.r] |
| Styled Text · 2012 | Send both plain/styled, styled only, or plain only; ask each time; respect incoming bold, italic, underline, color, size, small sizes, font, margins, excerpts, left/right/center alignment; formatting toolbar. | [SettingsTwo.rsrc.r:3318][src/SettingsTwo.rsrc.r] |
| Moving Around · 2013 | Arrow-key message-switch modifiers: Command/Shift/Caps Lock/Option/Control; after finishing open nothing/next/next unread/next if unread/next if unread and same subject; Tab switches fields versus inserts tab; Return switches header fields; exchange Find/Search shortcuts. | [SettingsTwo.rsrc.r:2988][src/SettingsTwo.rsrc.r] |
| Toolbar · 2014 | Show toolbar/search field; horizontal/vertical orientation; big icons with names/big icons/small icons with names/small icons/names only; map function keys to buttons; show function-key labels. | [SettingsTwo.rsrc.r:1133][src/SettingsTwo.rsrc.r] |
| Getting Attention · 2015 | Alert; bounce Dock icon; open new-mail mailbox; sound; unread badge considers recent-only/all open mailboxes versus In; task-progress window; open incoming message with error; new-mail and attention sounds. | [SettingsTwo.rsrc.r:1228][src/SettingsTwo.rsrc.r] |
| Hosts · 2016 | Incoming and SMTP hosts; overlap POP commands; use MX records; Ph/LDAP and Finger hosts; live directory queries; DNS load balancing; offline mode. | [SettingsTwo.rsrc.r:1332][src/SettingsTwo.rsrc.r] |
| Extra Warnings · 2017 | Deleting unread/queued/any unsent mail; queueing subjectless/styled mail; quitting with queued messages; size warning threshold K; changed-message read-only toggle; empty Trash; excessive memory; following advertisement link offline. | [SettingsTwo.rsrc.r:1447][src/SettingsTwo.rsrc.r] |
| Internet Dialup · 2019 | Do not auto-check unless dial-up software connected; disconnect after checking only if Eudora connected it. The resource explicitly says **OS X does not support these settings**. | [SettingsTwo.rsrc.r:1572][src/SettingsTwo.rsrc.r] |
| Miscellaneous · 2020 | Close messages with mailbox; empty Trash on quit; Turbo Redirect default; re-sort less often; old-style `.toc`; obsolete Return-Receipt-To header; international sorting; filter reports; prompt for self-requested receipts; exchange Find/Search shortcuts; system Keychain; alternate Check Mail/Minimize and Attach/Hide shortcuts. | [SettingsTwo.rsrc.r:1605][src/SettingsTwo.rsrc.r] |
| Personalities · 2021 | Select/new/remove/rename; user/server; auto-check interval; participate in manual checks; real name/SMTP/email address; participate in send; submission port; authorization; Test Settings. | [SettingsTwo.rsrc.r:1700][src/SettingsTwo.rsrc.r] |
| Personality Extras, POP · 2022 | Personality; POP/IMAP; passwords/Kerberos/APOP; large-message threshold; leave-on-server days; stationery/signature; default domain; use relay personality. | [SettingsTwo.rsrc.r:1867][src/SettingsTwo.rsrc.r] |
| Personality Extras, IMAP · 3021 | Personality; POP/IMAP; prefix; minimal headers/full except large attachments/full including attachments; remove deleted messages always/never/above percentage; percentage threshold; server Trash mailbox; stationery/signature/domain; use relay personality. | [SettingsTwo.rsrc.r:3522][src/SettingsTwo.rsrc.r] |
| Statistics · 2024 | Current/previous/average graph-series colors and graph types; Line/Area/Bar/Circle/Diamond/Square; count all IMAP mailboxes versus Inbox; Reset Statistics. | [SettingsTwo.rsrc.r:2022, 3847][src/SettingsTwo.rsrc.r] |
| Address Book · 2025 | History visibility; nickname text in value-field sorting; rename in list; backups; Fast Save; expand books on nickname drags; PDA sync metadata; vCard support; hide personal vCards/button; reject suspicious formatting; default Home tab; show OS X Address Book. | [SettingsTwo.rsrc.r:2118][src/SettingsTwo.rsrc.r] |
| SSL · 2026 | Personality; per-POP/SMTP/IMAP None/Required TLS/Optional TLS/Required alternate port; standard-port and alternate-port negotiation use maximum compatibility/older SSL/TLS 1.0, with a historical Cyrus label on standard TLS. These are historical choices, not acceptable modern security defaults. | [SettingsTwo.rsrc.r:2233, 3869][src/SettingsTwo.rsrc.r] |
| Spell Checking · 2027 | Automatic-as-you-type/on request; sending/queueing warning; ignore initial capitals/mixed capitals/numbers/all capitals; suggestions resembling spelling/sound/either/none. | [SettingsTwo.rsrc.r:3432][src/SettingsTwo.rsrc.r] |
| Content Concentrator · 2028 | Single-message preview and window profiles; multiple-message profile; maximum message count; require matching subjects/senders; multiple replies. | [SettingsTwo.rsrc.r:2813][src/SettingsTwo.rsrc.r] |

There is no standard panel numbered 2018 or 2023 in these definitions. `MoodWatchSettings.rsrc` is a zero-byte placeholder, so **a complete visible MoodWatch panel layout is unverified**. Its actual controls can be reconstructed from analyzer flags: enable/disable, incoming/existing scanning, phrase marking/covering/speaking, outgoing delay and quote scanning, plus warning/delay thresholds and whitelist settings. [Include/anal.h:29][src/Include/anal.h], [Include/StringDefs.h:1405, 1452][src/Include/StringDefs.h].

**Every Esoteric Settings panel** follows. This is an additional resource package, not a separate account backend.

| Panel · resource | All options/controls | Source |
|---|---|---|
| Color Tuning · 2100 | Area lightening%; text darkening%; maximum/minimum lightness%; colors for URL, misspelling, potentially inflammatory and inflammatory text. | [EsotericSettings60.r:45][src/EsotericSettings60.r] |
| Logging · 2102 | Sent/received messages; serial navigation; alerts; progress; all bytes; throughput; leave-on-server decisions; startup plug-ins; filter matches; moved messages; protocol details; TOC handling; SSL data; internals; rollover size K. | [EsotericSettings60.r:158][src/EsotericSettings60.r] |
| Timeouts · 2103 | Connection open; server response; alerts; serial navigation without `\e`; Apple events. | [EsotericSettings60.r:277][src/EsotericSettings60.r] |
| Ports & Protocols · 2104 | POP3/SMTP/IMAP/Finger/Ph/LDAP ports; unread detection via LAST/normal/Status header; XTND XMIT; `%` routing hack; hostname in POP USER; 8-bit SMTP if 8BITMIME. | [EsotericSettings60.r:348][src/EsotericSettings60.r] |
| Desktop Margins · 2105 | Left/right/top/bottom screen margins in pixels. | [EsotericSettings60.r:480][src/EsotericSettings60.r] |
| Junk Extras · 2107 | Minimum trim score; trim destination mailbox; transfer-to-Junk score; address book for whitelisted senders; delete Junk from server; distrust junk Date; always enable Junk/Not Junk commands. | [EsotericSettings60.r:1469][src/EsotericSettings60.r] |
| Compaction · 2108 | Waste percentage of mailbox size; waste percentage of free volume space. | [EsotericSettings60.r:545][src/EsotericSettings60.r] |
| Toolbar on Steroids · 2109 | Show/orientation; interbutton/intergroup spacing; buttons per group; five icon/name styles and associated H/V sizing fields; function-key mapping and labels. | [EsotericSettings60.r:604][src/EsotericSettings60.r] |
| Really Miscellaneous · 2110 | Nickname backups; faster typing; duplicate-message deletion; resource-fork TOC backups; smart paste; dropping messages onto nickname buttons forwards or redirects. | [EsotericSettings60.r:771][src/EsotericSettings60.r] |
| Boring Headers · 2111 | Editable incoming-header suppression list. | [EsotericSettings60.r:833][src/EsotericSettings60.r] |
| Nickname Expansion · 2112 | Immediate expansion; autocomplete; highlighting; immediate/time-delay/key-delay processing; history maintenance; cache size; history display; add reply and typed addresses. | [EsotericSettings60.r:850][src/EsotericSettings60.r] |
| Make Filter Defaults · 2113 | Default new-mailbox locations for Subject, From and Any Recipient generated filters. | [EsotericSettings60.r:952][src/EsotericSettings60.r] |
| Threading · 2114 | Background threading; separate send thread; task-progress display; delivery delay; foreground/background allowed run time. | [EsotericSettings60.r:1002][src/EsotericSettings60.r] |
| LDAP · 2115 | Host; word-wise/full-query filter templates; live queries; raw attribute names; require Base Object. | [EsotericSettings60.r:1068][src/EsotericSettings60.r] |
| Settings Icons · 2117 | Big icons with names/big icons/small icons with names/names only. | [EsotericSettings60.r:1133][src/EsotericSettings60.r] |
| IMAP · 2118 | Personality; message size with minimal headers; fetch everything on open; ignore ALERT; incoming Inbox filters; maximum simultaneous connections; idle close seconds; task progress for all tasks; cleartext-password-only authentication; top-level Inbox; preserve attachments after transfer; resync open mailboxes on check; smarter connection management; junk plug-ins; drop trashed cache; UIDPLUS; create Junk/Trash; execute queued commands on quit. | [EsotericSettings60.r:1168][src/EsotericSettings60.r] |
| Speak · 2119 | Intermessage phrase; special mail speech rules; mixed-case word splitting; pronounce periods as dot; italic emphasis; acronym expansion; suppress punctuation runs; URL processing/truncation; speak quotes, alternate quote voice, before/after quote phrases. | [EsotericSettings60.r:1316][src/EsotericSettings60.r] |
| HTTP Proxy · 2120 | Host:port; enable HTTP proxy. | [EsotericSettings60.r:1440][src/EsotericSettings60.r] |

### 2.5 `x-eudora-setting`: hidden power and the full catalogue

`x-eudora-setting:ID` opens a settings editor showing current/default values and help; `x-eudora-setting:ID=VALUE` supplies a candidate value; `ID@Personality` selects an existing personality. The handler checks the personality, rejects password-storage IDs, presents a confirmation dialog, offers Revert, and only then applies the chosen value. Ordinary links are therefore **not silent configuration writes**. Separate startup prefill processing reads a text file of setting links and renames it after processing. [url.c:454, 465, 503, 537][src/url.c], [ends.c:3927][src/ends.c].

The exhaustive documentation is [X-Eudora-Settings.txt:1][src/X-Eudora-Settings.txt]; its 1,033 leading links comprise **394** ordinary preference entries in `prefs.xset` and **639** string/resource entries in `StringDefs.xset`. The [CSV catalogue](codex-artifacts/audit/x-eudora-settings.csv) preserves **every ID, full description and original source line**; [panel controls](codex-artifacts/audit/panel-controls.md) preserve every resource binding. A description may be stale: IDs 384/386 are described as reserved although the enum names POP-before-SMTP state. Many `UNUSED_*` entries and UI strings are not active hidden features. [prefs.xset:364][src/prefs.xset], [StringDefs.xset:1][src/StringDefs.xset], [Include/prefdefs.h:409][src/Include/prefdefs.h].

| Hidden capability | Concrete IDs/symbols and significance | Evidence |
|---|---|---|
| Editing/copy/export control | Paragraph recognition/unwrap mask **17**; headers on save **41**; header-navigation/tab behavior; plain-text paste **199**; replacement shortcuts; signature and quoting templates. | [prefs.xset:17, 41][src/prefs.xset], [EsotericSettings60.r:805][src/EsotericSettings60.r], [menu.c:2983][src/menu.c] |
| Nonstandard server compatibility | POP command overlap **34**; LAST/Status detection **69**; XTND XMIT **87**; `%` routing **196**; POP host-qualified USER **195**; 8BITMIME **242**; custom ports. | [EsotericSettings60.r:348][src/EsotericSettings60.r] |
| IMAP interoperability | UID ranges **304**; exclusive expunge **307**; polite logout **310**; foreground filtering **363**; UIDPLUS **373**; orphaned-attachment/cache retention **280/374/397**; queued command execution **389**. | [PrefDefs:280, 304, 307, 310, 397][src/PrefDefs] |
| Precision navigation/presentation | Message-finish modes; preview-read delay/events; custom boring-header list **3200**; date formats/timezones; toolbar geometry/function keys; window/Dock badge bits **396**. | [SettingsTwo.rsrc.r:2988, 845][src/SettingsTwo.rsrc.r], [EsotericSettings60.r:833][src/EsotericSettings60.r], [prefs.xset:391][src/prefs.xset] |
| Dynamic search | **395 / PREF_NO_LIVE_SEARCHES** controls whether open search windows incorporate new messages. | [prefs.xset:390][src/prefs.xset], [Include/prefdefs.h:420][src/Include/prefdefs.h] |
| MoodWatch customization | **319 / PREF_NO_COURIC** bit mask; warning level **14302**; delay level/minutes **14709/14710**; phrase whitelist; `EuSc` resource **128** custom scoring. | [Include/anal.h:29][src/Include/anal.h], [Include/StringDefs.h:1405, 1452][src/Include/StringDefs.h], [anal.c:480, 504][src/anal.c] |
| Quantitative junk control | Threshold **32470**, retention **32471**, destination **32472**, trim score **32473**, transfer score **32477**, whitelist book **32491**, per-account/plugin bits **362**. | [Include/StringDefs.h:1553][src/Include/StringDefs.h], [EsotericSettings60.r:1469][src/EsotericSettings60.r] |
| Content Concentrator | Preview profile **32496**, multi-preview **32601**, count limit **32602**, window profile **32607**, multi-reply **32621**; quote/forward/digest recognizers and ellipsis strings. | [Include/StringDefs.h:1579, 1603][src/Include/StringDefs.h] |
| Storage/latency tuning | TOC storage **126**; backups **60/193**; duplicate deletion **177**; nickname-processing delays/cache; TOC memory/disk-read parameters; thread run time and delivery delay; autosave interval **11520** (seconds). | [EsotericSettings60.r:771, 850, 1002][src/EsotericSettings60.r], [SettingsTwo.rsrc.r:1639][src/SettingsTwo.rsrc.r], [Include/StringDefs.h:1143][src/Include/StringDefs.h] |
| Diagnostics and administrators | Logging bitmask **71**; log rollover **10004**; network/event timeouts; ACAP provisioning; custom directory templates; URL helper applications; settings prefill. | [EsotericSettings60.r:158, 277, 1068][src/EsotericSettings60.r], [acap.r:2][src/acap.r], [ends.c:3927][src/ends.c] |

**Recommendation:** retain inspectable advanced settings but use typed names, constraints, documented defaults and import previews. Eudora's reversed booleans, raw numeric IDs and bit masks are useful migration evidence, but a poor new public configuration format. This is a design judgement grounded in the resource bindings and documentation above.

### 2.6 Filters, message operations and search

**Rule model:** each rule has Incoming, Outgoing and Manual enable flags, **two** match terms, one conjunction, a name, last-match bookkeeping and up to **five UI action slots**. The executor walks pre-filters, the user's filters and post-filters, in that order. Rules are persisted as textual keywords, not compiled scripts. Plug-in filter files have their own discovery path. [Include/filters.h:70, 119][src/Include/filters.h], [filtrun.c:1067][src/filtrun.c], [filtmng.c:198, 702][src/filtmng.c], [Include/StringDefs.h:998][src/Include/StringDefs.h].

| Match target | Complete scope and qualifications | Evidence |
|---|---|---|
| Named headers | Presets To, Cc, From, Subject, Reply-To; a typed header name is also supported. Multiple occurrences are inspected. | [common.rsrc.r:6311][src/common.rsrc.r], [filtrun.c:1230][src/filtrun.c] |
| Any Recipient | Recipient pseudo-field, with nickname expansion for outgoing recipient fields. | [Include/filters.h:103][src/Include/filters.h], [filtrun.c:1285][src/filtrun.c] |
| Any Header / Body | Header region or body region, rather than just the summary shown in the mailbox. | [common.rsrc.r:6331][src/common.rsrc.r], [filtrun.c:1325][src/filtrun.c] |
| Junk Score | Numeric cached score, using equality, inequality, less-than or greater-than. Feature-gated. | [filtrun.c:1234, 1631][src/filtrun.c] |
| Date / Personality | Additional handler-recognized names: Date compares the formatted local summary date as text; `«Personality»` compares the account name, using Dominant for the default. These are not extra numeric/date filter operators. | [filtrun.c:1232, 1602, 1616][src/filtrun.c], [StrnDefs:1089][src/StrnDefs] |

| All 15 filter operators | Meaning | Evidence |
|---|---|---|
| contains; does not contain | Substring test and its negation. | [common.rsrc.r:5823][src/common.rsrc.r], [filtrun.c:1510][src/filtrun.c] |
| is; is not | Whole-value equality/inequality. | [filtrun.c:1516][src/filtrun.c] |
| starts with; ends with | Prefix/suffix. | [filtrun.c:1524][src/filtrun.c] |
| appears; does not appear | Header presence/absence; a value is unnecessary. | [filtrun.c:1263][src/filtrun.c] |
| intersects nickname; does not intersect nickname | Address-set intersection after expanding a nickname/address expression. | [filtrun.c:1543][src/filtrun.c] |
| intersects address book; does not intersect address book | Membership against a nickname file; optimized sender matching uses cached address hashes. | [filtrun.c:1582, 1243][src/filtrun.c] |
| matches regexp | Compiled regular expression, cached on the term. | [filtrun.c:1497][src/filtrun.c] |
| is less than; is greater than | Numeric junk-score comparisons. | [filtrun.c:1631][src/filtrun.c] |

Conjunctions are **ignore**, **and**, **or**, **unless** (`A AND NOT B`), with short-circuit evaluation. Filtering sets its shared case-sensitivity flag false; whitespace normalization can be disabled by `PREF_NO_FILT_LWSP`. This describes the historical engine, not guaranteed Unicode matching. There is a `TermPriorMatch` function, but no caller was found in the audited tree: an independent numeric Priority filter criterion is **unverified**. [filtrun.c:1036, 1512, 1765, 1650][src/filtrun.c].

| Every action | Parameters/effect | Execution pass | Evidence |
|---|---|---:|---|
| None | Empty action slot. | 0 | [FiltDefs.r:67][src/FiltDefs.r] |
| Make Status | Set message state. | 2 | [filtwin.c:4003][src/filtwin.c] |
| Make Priority | Fixed priority or raise/lower. | 0 | [FiltDefs.r:67][src/FiltDefs.r], [filtwin.c:4835][src/filtwin.c] |
| Make Label | Set local color label. | 0 | [filtwin.c:4236][src/filtwin.c] |
| Make Personality | Assign identity/account personality. | 0 | [FiltDefs.r:67][src/FiltDefs.r] |
| Make Subject | Change **summary only**; `&` inserts the original summary subject. | 1 | [FiltDefs.r:147][src/FiltDefs.r], [filtwin.c:4226][src/filtwin.c] |
| Play Sound | Select a sound; post-processing collects sounds. | 0 | [FiltDefs.r:67][src/FiltDefs.r], [filtrun.c:234][src/filtrun.c] |
| Speak | Voice plus sender and/or subject. | 0 | [FiltDefs.r:153][src/FiltDefs.r] |
| Open | Open destination mailbox and/or message. | 0 | [FiltDefs.r:156][src/FiltDefs.r], [filtrun.c:110][src/filtrun.c] |
| Print | Print message. | 0 | [filtrun.c:145][src/filtrun.c] |
| Remember Sender | Add sender to address History. | 0 | [FiltDefs.r:67][src/FiltDefs.r] |
| Notify User | Independently enable/suppress normal notification and Filter Report entry. | 0 | [FiltDefs.r:140][src/FiltDefs.r], [filtrun.c:127][src/filtrun.c] |
| Forward To | Comma-separated destination addresses; repeatable. | 1 | [FiltDefs.r:142][src/FiltDefs.r], [filtwin.c:5056][src/filtwin.c] |
| Redirect To | Comma-separated destinations; repeatable. | 1 | [FiltDefs.r:143][src/FiltDefs.r], [filtwin.c:5074][src/filtwin.c] |
| Reply With | Select stationery for automatic reply. | 1 | [FiltDefs.r:144][src/FiltDefs.r], [filtwin.c:5211][src/filtwin.c] |
| Server Options | Fetch a partial message in full and/or delete its server copy. | 0 | [FiltDefs.r:158][src/FiltDefs.r] |
| Copy To | Copy into another mailbox; repeatable. | 3 | [filtwin.c:3980][src/filtwin.c] |
| Transfer To | Move into another mailbox; successful transfer stops subsequent filtering of that message. | 4 | [filtwin.c:3508, 3545][src/filtwin.c] |
| Junk | Apply junk action and move to Junk. | 4 | [filtwin.c:3234][src/filtwin.c] |
| Move Attachments | Move extracted attachment files to selected folder. | 3 | [filtwin.c:3575][src/filtwin.c] |
| Skip Rest | Stop further rules for this message. | 9 | [filtwin.c:3195][src/filtwin.c] |

Passes come from `FAPass`; the dispatcher executes pass order before action-list order. Thus visual action order alone does **not** determine execution. None, Forward, Redirect and Copy are the only repeatable action types in `FAMultiple`. Twelve action types are marked Pro-only in the generated table, with additional runtime feature checks. Auto-forward/redirect/reply suppress messages marked `OPT_BULK` unless `PREF_BOMBS_AWAY` overrides that guard; this is a loop-avoidance heuristic, not a proof against mail loops. [FiltDefs.c:73, 103, 132][src/FiltDefs.c], [filtrun.c:1965][src/filtrun.c], [filtwin.c:5060, 5211][src/filtwin.c].

For an undownloaded IMAP message, filtering can fetch just headers and use an IMAP matching path for other tests. IMAP copy/move, expunge and resynchronization have explicit filtering integration. Local rules consequently depend on availability and client activity; they are not automatically server-resident rules. [filtrun.c:1158, 1247][src/filtrun.c], [filtwin.c:3535][src/filtwin.c], [filtrun.c:176, 241][src/filtrun.c].

**Operations beyond menu labels:**

| Workflow | Verified behavior and useful distinction | Evidence |
|---|---|---|
| Reply / Reply All / Reply With | Recipient-selection controls, selected-text reply, inherited priority, self/list handling, stationery and multiple-message reply. | [SettingsTwo.rsrc.r:3191][src/SettingsTwo.rsrc.r], [menu.c:1632][src/menu.c], [message.c:3321][src/message.c] |
| Forward | Compose a forward with configurable attribution/quoting. | [message.c:3954][src/message.c], [messact.c:1192][src/messact.c] |
| Redirect / Turbo Redirect | Copy original Subject, From and body into a new composition, add a “by way of” From annotation, preserve attachments and optionally lock the body. Turbo with a recipient queues the new message and can trash the original **at queueing**, with Shift preserving it. Do not equate this source path with byte-preserving RFC Resent redistribution. | [message.c:3780, 3887, 3866][src/message.c], [messact.c:1203, 1251][src/messact.c] |
| Send Again | Reconstitute an editable outgoing composition, including recovery from a bounced message's headers; not merely retry an existing network transaction. | [message.c:2693, 2701][src/message.c], [messact.c:1211][src/messact.c] |
| Queue / Change Queueing | Normal and timed queue states, date/time scheduling and immediate-send preference; MoodWatch can delay queueing. | [compact.c:237][src/compact.c], [messact.c:1129][src/messact.c], [aete.r:178][src/aete.r] |
| Transfer / Copy / Fcc | Hierarchical destination selection; Option modifies transfer/copy behavior; local-to-IMAP paths are explicit; automatic Fcc can retain a copy in the original mailbox. | [messact.c:149][src/messact.c], [filtwin.c:3535][src/filtwin.c], [SettingsTwo.rsrc.r:588][src/SettingsTwo.rsrc.r], [mailxfer.c:1454][src/mailxfer.c] |
| Make Filter | Generate an incoming/outgoing/manual rule from From, Any Recipient or Subject; target Trash, an existing mailbox or a newly created mailbox, then optionally Add Details; configurable default parent folders. | [makefilter.c:65][src/makefilter.c], [common.rsrc.r:1687][src/common.rsrc.r], [EsotericSettings60.r:952][src/EsotericSettings60.r] |
| Make Address Book Entry / Make Nickname | Derive reusable addresses/groups from message(s), headers, selected text or vCard. | [nickmng.c:2088][src/nickmng.c], [menu.c:2811][src/menu.c] |
| Summary edits | Sender/subject summary metadata can differ from original headers; useful local annotations. | [aete.r:286][src/aete.r], [filtwin.c:4226][src/filtwin.c] |
| Status / Priority / Label | 14 resource states; five display priorities plus raise/lower; None plus eight customizable labels, with Finder-label integration. Some states describe outgoing/error/recovery lifecycle rather than user flags. | [common.rsrc.r:5941, 5787][src/common.rsrc.r], [SettingsTwo.rsrc.r:1026][src/SettingsTwo.rsrc.r], [menu.c:1642][src/menu.c] |
| Server disposition | Leave, Fetch, Delete, Fetch & Delete choices; POP retention/deletion and IMAP cached/remote state are separate concerns. | [common.rsrc.r:6463][src/common.rsrc.r], [pop.c:2548][src/pop.c] |
| Junk / Not Junk | Plugin scoring/training hooks, whitelist rules, configurable scores for manual transfer, age/score-based trimming and optional archive destination. | [EsotericSettings60.r:1469][src/EsotericSettings60.r], [Include/emsapi-mac.h:1496][src/Include/emsapi-mac.h], [squish.c:219][src/squish.c] |
| Delete / Trash | Local and IMAP Trash paths, unread/queued/unsent warnings, selected-account trash operations; Option+Shift permanent-delete caption. | [menu.c:1632][src/menu.c], [SettingsTwo.rsrc.r:1447][src/SettingsTwo.rsrc.r], [messact.c:1218][src/messact.c] |
| Receipts / message options | Per-message receipt, keep-copy, wrapping, tab expansion, quoted-printable and Mac attachment encoding; return-receipt prompting/preferences. Receipt support should not be interpreted as guaranteed reading. | [aete.r:286][src/aete.r], [SettingsTwo.rsrc.r:1605][src/SettingsTwo.rsrc.r] |

**Find Messages** is a multi-mailbox search window with up to **16 criteria**, Match All/Any, saved searches and virtual-mailbox results. The complete field list is **Anywhere, Headers, Body, Attachment Name(s), Summary, Status, Junk Score, Priority, Attachment Count, Label, Date, Size (K), Age, Personality, To, From, Subject, Cc, Bcc, Any Recipient**. Text operators add whole-word matching to the filter text operators; enumerations use is/is not, numbers use equality/inequality/greater/less, dates use is/is not/after/before, and age units are minutes/hours/days/weeks/months/years. These are separate from the two-term filter editor. [searchwin.c:29][src/searchwin.c], [common.rsrc.r:6257, 6341, 6373, 6399, 6435][src/common.rsrc.r].

Saved search files preserve criteria, mode and mailbox selections, and populate the Find submenu. Open results can incorporate new matching messages when `PREF_NO_LIVE_SEARCHES` is false. Virtual summaries refer back to real messages rather than requiring duplicate message copies. Ordinary Find/Find Next/Find Selected Text/Enter Selection remain distinct commands. The inspected search engine scans mailbox data; an inverted full-text index was not found in these modules. [searchwin.c:4292, 4708, 4029][src/searchwin.c], [Include/mailbox.h:121][src/Include/mailbox.h], [filtrun.c:307][src/filtrun.c], [search.c:248][src/search.c], [common.rsrc.r:5723][src/common.rsrc.r].

### 2.7 Contacts, personalities, composition, extensions and automation

**Nicknames are programmable address groups.** Books contain named expansions, notes and structured fields; completion can use history as well as books; group expansions can contain other nicknames. Quick Recipients populate New Message To, Forward To, Redirect To and Insert Recipient. Make Address Book Entry can derive addresses from a message, multiple selected messages, composition fields or selected text; a vCard attachment can contribute details. [nickmng.c:2088, 2126, 2214, 2263][src/nickmng.c], [nickexp.c:280][src/nickexp.c], [menu.c:2811][src/menu.c].

| Address-book tab | Fields/actions visible in resources | Evidence |
|---|---|---|
| Personal | Full name, first name, last name, swap first/last, expansion address(es). | [AddressBookTabs/personalTab.r:14][src/AddressBookTabs/personalTab.r] |
| Home | Street, city, state/province, country, postal code; phone, fax, mobile, iChat, web. | [AddressBookTabs/homeTab.r:14][src/AddressBookTabs/homeTab.r] |
| Work | Title, company; street/city/state/country/postal code; phone/fax/mobile/iChat/web. | [AddressBookTabs/workTab.r:14][src/AddressBookTabs/workTab.r] |
| Other | Other email addresses, phone numbers, web pages. | [AddressBookTabs/otherTab.r:16][src/AddressBookTabs/otherTab.r] |
| Notes | Free-form notes. | [AddressBookTabs/notesTab.r:14][src/AddressBookTabs/notesTab.r] |
| Photo | Select Photo; picture field accepting PICT drags. | [AddressBookTabs/photoTab.r:6][src/AddressBookTabs/photoTab.r] |

PDA change tracking and vCard import/export have source paths; Mac Address Book integration explicitly describes **one-way** import into Eudora nickname files. Do not inflate this into complete two-way Contacts synchronization. The 6.2 help also documents launching iChat for a contact. [nickmng.c:1097][src/nickmng.c], [vcard.c:1][src/vcard.c], [palmconduitae.c:1][src/palmconduitae.c], [osxabsync.cp:28][src/osxabsync.cp], [HelpText.r:47][src/HelpText.r].

**Personalities** combine identity, servers, protocol, authentication, polling/send participation, signatures/stationery and overrides. “Dominant” is the default personality. Filters can assign a personality and a separate relay personality can send mail for participating identities. This separates identity selection from presentation and transport configuration. [SettingsTwo.rsrc.r:1700, 1867, 3522][src/SettingsTwo.rsrc.r], [FiltDefs.r:67][src/FiltDefs.r].

**Stationery** is richer than a text snippet: it reconstitutes a composition from stored text and a serialized message summary, applies headers/body and options, and can carry spooled attachments. It is selectable for new messages, replies, account defaults and filter auto-replies. Signatures are separately managed, with None/Standard/Alternate resource seeds and additional user signatures. [compact.c:647, 672, 695, 806, 1954, 3332][src/compact.c], [ends.c:3247][src/ends.c].

**Rich composition and message presentation** include styles, color/font/size/alignment/margins, quote/unquote, rules, graphics and URL links; plain-only/styled-only/both sending; MIME attachments and historical Mac metadata encodings; inline graphics, animated GIFs and picture emoticons. These are exactly the menus/settings listed above, not a claim of modern HTML/CSS conformance. [two.rsrc.r:419, 631][src/two.rsrc.r], [SettingsTwo.rsrc.r:2486, 3318][src/SettingsTwo.rsrc.r], [sendmail.c:1017][src/sendmail.c].

**MoodWatch** incrementally scans text using an external TAE engine and Flame Dictionary, marks detected phrases, scores message summaries, optionally warns at queueing and delays a qualifying send. The default delay constants shown in source are level **2** and **10 minutes**, but the queue handler tests **score greater than the threshold**, so the exact displayed-level correspondence needs the external engine. Quote scanning, phrase masking/speech and whitelists are configurable. Engine/dictionary availability and classification quality are **unverified**. [anal.c:61, 331, 404, 445, 480][src/anal.c], [Include/anal.h:29][src/Include/anal.h], [compact.c:237][src/compact.c], [Include/StringDefs.h:1452][src/Include/StringDefs.h].

**ScamWatch** checks a link's displayed hostname against its target, strips configured noise prefixes, and flags numeric or percent-encoded hosts; the click path can ask before following it, and rollover tips use the same checker. A TLD heuristic appears behind a conditional symbol and should not be assumed active. This is a concrete deceptive-link checker, not proof of general phishing detection. [url.c:1109, 2469, 2487, 2540, 2639][src/url.c].

**Content Concentrator** parses XML profiles, classifies message elements and applies display/remove/truncate operations with trimming, flattening, quote-level changes and ellipsis. It recognizes quote/forward attribution, signatures, attachments and digest separators, and handles combined previews of selected messages. This is deterministic presentation processing; it is not an LLM summary and does not need to discard the stored original. [concentrator.c:79, 400, 790, 1154, 1607, 1658][src/concentrator.c].

**Link History** records URLs and supports viewing/removing entries; its offline-link dialogs include bookmark/remind choices and later reminders. **Speech** handles selected text/messages, filters' sender/subject announcements, spoken alerts, quote voice differences, acronym/pronunciation dictionaries and URL cleanup. **Statistics** count received/sent mail and attachments, read/replied/forwarded/redirected messages, active reading/composing/other time, and junk/whitelist/false-positive/false-negative totals in day/week/month/year periods with current/previous/average series. [linkwin.c:98, 477][src/linkwin.c], [speechutil.c:511, 707, 952, 1575, 1602][src/speechutil.c], [Include/statmng.h:22][src/Include/statmng.h], [statmng.c:814][src/statmng.c].

**EMSAPI** in this checkout is version **7**, with lifecycle (`ems_plugin_version/init/finish/config`), translator discovery/eligibility/file translation, queued properties, attachers, special commands, idle callbacks, junk scoring and user training notifications. Translation contexts cover arrival, display, explicit request, completed composition and transmission; flags describe MIME/header requirements and toolbar presence. Junk hooks carry **0–100** scores plus a special **−1** result. Host callbacks expose progress, directories, mailbox/personality selection and related services. Private/importer/window/context extensions are explicitly guarded by internal macros and are not all the public plug-in contract. The presence of these hooks does **not** verify that a specific PGP, antivirus or junk plug-in ships. [Include/emsapi-mac.h:154, 180, 202, 878, 1187, 1496, 1519, 1587][src/Include/emsapi-mac.h].

**AppleScript dictionary inventory:**

| Suite / object family | Commands, parameters and exposed data | Evidence |
|---|---|---|
| Standard commands | close, count, delete, exists, get, make, move, duplicate, open, print, save, set. `make` can create an invisible message with initial data. | [aete.r:35][src/aete.r] |
| Application/windows | Application version and selected text; windows' bounds, position, zoomed/index, selected text; read-only name/id/modified/closeable/resizeable/zoomable. | [aete.r:111][src/aete.r] |
| Eudora commands | connect with checking/sending booleans; reply with all/self/stationery controls; forward; redirect; send again; queue for date/time; unqueue; attach documents with optional copied/spooled ownership; start/stop notifying; notice; compact selected/all mailboxes. | [aete.r:148][src/aete.r] |
| Mail folders/mailboxes | Nested folders/books/mailboxes; mailbox name, wasted/needed space, total/recent unread, data/TOC file locations and window; messages by index/unique ID. | [aete.r:242][src/aete.r] |
| Messages | Read/write body, whole text, priority, label, status, summary sender/subject, signature, quoted-printable, receipt, wrap, tab expansion, keep-copy, Mac metadata/attachment encoding, header visibility, editability, transliteration table, fetch/delete dispositions, destroy-attachments and personality; read-only date/time/size/outgoing/on-server/mailbox/window/id. Named header fields and attachments are elements. | [aete.r:291][src/aete.r] |
| Nickname books/nicknames | File; nickname, addresses, notes, Quick Recipient membership; expansion and raw notes; named structured fields. | [aete.r:341][src/aete.r] |
| Personalities/settings/helpers | Personality name/id and indexed settings; application settings; URL-helper protocol name and application. | [aete.r:398][src/aete.r] |
| Enumerations | Message statuses, None/Standard/Alternate signatures, four attachment encodings; notices for mail arrives/sent, will connect/has connected and manual filtering. Filter verb/conjunction enums under `NEVER` are excluded from implemented scripting claims. | [aete.r:434][src/aete.r] |
| URL suite | geturl and fetchurl for finger/ph/mailto, with attachment/spooling parameters on geturl. | [aete.r:481][src/aete.r] |

The implementation installs corresponding Apple-event handlers; scripts in the Scripts folder are loaded through the OSA component, including resource- or data-fork scripts. Dictionary declarations remain a contract inventory rather than a full conformance test. [appleevent.c:4656][src/appleevent.c], [scriptmenu.c:37, 169][src/scriptmenu.c].

**Help coverage:** About this Release; Creating Messages; Attaching a File; Using Signatures; Using Stationery; Using Personalities; Receiving Mail; Replying; Deleting; Transferring; Filtering; Using Mailboxes; Using the Address Book; Modifiers and Shortcuts; Changing Your Password; Questions or problems? The empty TEXT resources supply separators. Help then appends conditional Purchasing/Registration or Payment & Registration, Insert System Configuration, Make a Suggestion, Report a Bug. Balloon help lives in `HelpMenuDefs*`/`HelpDlogDefs*`. `LightHelp`/`SupportHelp` are outside the normal 2000-series menu scan. [HelpText.r:1, 874][src/HelpText.r], [ends.c:2603][src/ends.c], [Include/StringDefs.h:1618][src/Include/StringDefs.h].

**Spell checking** includes automatic marking, explicit checks, next-misspelling navigation, dynamic suggestions and add/remove operations on user lexicons, with a separate **User Anti-Dictionary** for explicitly rejected words and spelling/sound suggestion modes. Dictionary folders are discovered at runtime. Wintertree-dependent functionality remains conditional; language/dictionary coverage cannot be inferred from the interface alone. [spell.c:26, 50, 699, 631, 665][src/spell.c], [Include/StringDefs.h:1151][src/Include/StringDefs.h], [SettingsTwo.rsrc.r:3432][src/SettingsTwo.rsrc.r].

**Import/export** includes an importer plug-in contract for mail, account settings, addresses and signatures; exporting recursively traverses mailboxes and reserializes each message through `SpoolMessage`, adding Mozilla-specific status headers. This is explicit export conversion, not byte-identical copying of mailbox forks. Supported third-party importer packages are **unverified** here. [import.c:83, 90, 93, 98, 101][src/import.c], [export.c:149, 243, 296][src/export.c].

**Boundary cases:** PGP input recognizes/converts PGP material, but the historical built-in outgoing PGP implementation in `pgpout.c` is under `NEVER`; use the active EMS translation contract as extension evidence. ESP-associated mailbox/file-view hooks and registration-gated UI are present, but a complete installed Eudora Sharing Protocol plug-in is **unverified**. [pgpin.c:53][src/pgpin.c], [pgpout.c:28][src/pgpout.c], [Include/emsapi-mac.h:202][src/Include/emsapi-mac.h], [Include/mailbox.h:155][src/Include/mailbox.h], [fileview.c:1][src/fileview.c].

### 2.8 Storage, drafts and corruption recovery

| Stored object | On-disk representation and relationships | Evidence |
|---|---|---|
| Local mailbox | One data fork containing concatenated messages with sendmail-style `From ` envelope separators and classic-Mac CR newlines. Envelope-like body lines can be escaped with `>`. Mailboxes live under Mail Folder; a mailbox is not inherently a Windows `.mbx`/`.toc` pair. | [buildtoc.c:315, 743, 1270][src/buildtoc.c], [pop.c:2251][src/pop.c], [Include/StringDefs.h:947][src/Include/StringDefs.h] |
| Message summary / TOC | Raw `TOCType` plus contiguous `MSumType` records: offsets/lengths/body offsets, date/timezone, state, flags/options, serial ID, personality IDs, UID/Message-ID hashes, priority, label-related flags, junk/mood values, sender/subject and window/view state. Sender/subject buffers are **48/60 bytes**; message count is a `short`; current TOC version/minor are **1/9**. | [Include/mailbox.h:76, 130, 146][src/Include/mailbox.h] |
| TOC file/fork | `PREF_NEW_TOC` true writes `TOC_TYPE` resource **1001** in the mailbox's resource fork; false writes the raw handle into sibling `name.toc`. Resource reader tries **1002** if 1001 is absent. Successful resource-fork writes remove the external `.toc`. The preference's help wording is misleading; follow the code. | [toc.c:285, 332, 429, 439, 548][src/toc.c], [PrefDefs.strn:130][src/PrefDefs.strn] |
| Attachments / inline parts | Decoded files in Attachments Folder and related locations, with textual attachment notes referencing volume, filename, creator/type and file ID. MIME receiving can remove understood part headers and insert these notes; `SAVE_MIME` changes some paths. Therefore this storage is not a promised byte-for-byte original MIME archive. | [Include/StringDefs.h:450][src/Include/StringDefs.h], [pop.c:2076, 2137][src/pop.c], [mime.c:2824, 2857][src/mime.c] |
| Outgoing attachment ownership | Spooling duplicates an attachment into a per-message folder; other compositions can refer to an existing external file. Copies used for replies/redirects/stationery are explicit. A backup must include referenced files and owned spool data. | [message.c:4375, 4395][src/message.c], [aete.r:195][src/aete.r], [compact.c:3332][src/compact.c] |
| Draft / queued message | Stored as a message in the outgoing mailbox, with summary state/options. Saving appends a new serialized body, points the existing summary to it, then removes the extra appended summary; old bytes become waste. Timed send uses summary time. | [messact.c:957, 1021, 1030, 1064][src/messact.c], [buildtoc.c:143][src/buildtoc.c] |
| Settings / personalities | Eudora Settings is a Resource Manager file. String-list preferences and per-setting overrides coexist with other typed resources; non-Dominant personalities use overrides and personality-specific resource types. Password paths include system Keychain integration. | [Include/StringDefs.h:189][src/Include/StringDefs.h], [prefs.c:455, 1107, 1246][src/prefs.c], [schizo.c:284][src/schizo.c], [prefs.c:3977][src/prefs.c] |
| POP leave-on-server state | UIDL-based records/hashes, fallback header/`TOP` matching, timestamps and fetch/delete lists. `SavePOPD` writes personality-associated resources into settings, rather than assuming a standalone Windows LMOS file. | [pop.c:2506, 2575, 2590, 3034, 3287][src/pop.c] |
| IMAP cache / offline work | IMAP Folder, mailbox cache files and IMAP attachment locations. Mailbox nodes record server name/delimiter, `UIDVALIDITY`, pending flag changes and work; resources **`imap`, `inam`, `ifag`, ID 128** persist metadata/name/queued flags. Undownloaded summaries can have negative offsets; UIDVALIDITY changes trigger redownload decisions. | [Include/StringDefs.h:1123, 1139][src/Include/StringDefs.h], [Include/imapmailboxes.h:85][src/Include/imapmailboxes.h], [imapmailboxes.c:34, 810][src/imapmailboxes.c], [imapdownload.c:782][src/imapdownload.c] |
| Nicknames / stationery / signatures | Nickname text uses `alias` and `note` records, plus cached nickname TOCs and optional backups; photos are separate. Stationery stores text and serialized summary information; signatures and stationery have separate folders. | [nickmng.c:35, 1241, 2587, 3755][src/nickmng.c], [compact.c:3332][src/compact.c], [Include/StringDefs.h:386, 497][src/Include/StringDefs.h] |
| Search / ancillary data | Saved search resource files contain query and mailbox specifications; filters are keyword text files. Separate modules store Link History, statistics and UI state. These are additional migration objects, not recoverable from message bodies alone. | [searchwin.c:4292][src/searchwin.c], [filtmng.c:702][src/filtmng.c], [linkmng.c:1][src/linkmng.c], [statmng.c:1][src/statmng.c] |

**Recovery sequence.** TOC selection compares available fork/file timestamps; structural checks then validate record count/size, mailbox byte size, offset/length/body bounds and version. Modification-time validation was deliberately disabled because the comment says it breaks under HFS+. Runtime pointers, selections and window state are cleaned on load. Temporary delivery boxes receive an automatic rebuild path; normal mailboxes can rebuild, use the old TOC or cancel, according to error and preferences. [toc.c:122, 166, 215, 921, 963, 1039, 1069][src/toc.c].

A resource-fork TOC backup preserves the preceding resource as **1002**, unless disabled. Rebuilding scans the mailbox for recognizable message boundaries and reconstructs summaries. Salvage matches **offset and length**, copies old metadata, keeps newly parsed body offset/date, and deliberately preserves the old time for a timed queue. Unmatched new summaries become Recovered; additional Message-ID-hash salvage exists only under `RESYNC_MID`. There is also IMAP-specific salvage for minimal headers. [toc.c:55][src/toc.c], [buildtoc.c:38, 105, 120, 136, 149, 68][src/buildtoc.c].

**What recovery cannot promise:** local edited subjects, labels, status, scheduled-send time and identity can be lost when neither usable TOC nor matching old record exists. Since ordinary deletion removes summaries while compaction later removes unused bytes, a raw rebuild can rediscover deleted records or obsolete draft revisions; this is an inference from the deletion/append/rebuild paths. Extracted attachments can also be missing even when mailbox text survives. The TOC is therefore **part index, part irreplaceable user metadata**, not a disposable cache. [mailbox.c:1204][src/mailbox.c], [message.c:1949][src/message.c], [messact.c:1030][src/messact.c], [buildtoc.c:179][src/buildtoc.c], [squish.c:200][src/squish.c], [pop.c:2076][src/pop.c].

**Compaction** allocates a temporary mailbox on the same volume, combines contiguous live ranges, copies them, updates offsets, exchanges the old/new files and writes the TOC immediately. It checks available space and can run automatically based on waste thresholds. However, message bytes, extracted files and TOC are not one transaction. Its error path can clear a dirty message-window flag before closing the window to protect the mailbox, explicitly sacrificing work in progress. This code does not meet Sweetbriar's draft-safety requirement without replacement. [squish.c:200, 245, 317, 321, 334, 359][src/squish.c].

**Autosave exists but is configurable:** the idle loop reads an interval and saves dirty nickname/text/composition/filter windows and editable received messages. The documented default interval string is **0**, which disables that loop unless overridden. Incoming processing periodically writes the TOC and flushes the volume every fifth message unless the suppressing preference is set. These are useful safeguards, not crash-consistency proofs. [main.c:487, 850][src/main.c], [Include/StringDefs.h:1143][src/Include/StringDefs.h], [pop.c:1337][src/pop.c].

`mstore.c` and companion `ms*` modules describe an alternative store, but `MSFindMailbox` returns nil, `MSFixMailbox` is a no-op, and `MSFlushMailDB` leaves dirty-block flushing unfinished. The inspected active mailbox paths use TOCs. Treat the alternative as unfinished source, not evidence of a production transactional database. [mstore.c:350, 358][src/mstore.c], [msmaildb.c:98][src/msmaildb.c], [message.c:695][src/message.c].

**Lessons for Sweetbriar — recommendations:** keep the inexpensive summary projection, explicit attachment ownership and recoverable originals. Replace native-structure dumps, resource forks, mutable mailbox offsets and coupled caches with immutable RFC 5322/MIME blobs plus a transactional metadata database. Preserve annotations, drafts, tombstones and pending operations independently of rebuildable search/summary projections. An Eudora importer should first snapshot **both forks**, all attachments/spools, settings, nicknames and templates; retain the original archive and produce a reconciliation manifest rather than deleting it after conversion.

### 2.9 Protocols and performance techniques

| Area | Verified capabilities / limitation | Evidence |
|---|---|---|
| POP | `CAPA`, UIDL, `TOP` partial/header fetching, `RETR`, deletion and leave-on-server rules; fallback unread detection via LAST or Status headers; APOP, password and SASL paths. Command overlap depends on advertised pipelining or the preference fallback. | [pop.c:163, 181, 404, 459, 633, 2575][src/pop.c], [EsotericSettings60.r:348][src/EsotericSettings60.r] |
| SMTP | EHLO with HELO fallback paths, SIZE/8BITMIME, authentication, STARTTLS and alternate-port SSL; pipelined recipient commands; submission-port and relay-personality settings. MIME encoding includes base64/quoted-printable and historical Macintosh encodings. | [sendmail.c:305, 563, 798, 1017][src/sendmail.c], [SettingsTwo.rsrc.r:588][src/SettingsTwo.rsrc.r] |
| IMAP / CrispinIMAP | Separate c-client `imap4r1` driver plus Eudora adapters for mailbox trees, cached/minimal/full messages, attachment stubs, remote matching, copy/move, queued flag changes, expunge/resync, subscriptions and UIDPLUS responses. Subscription commands are in the driver; a separate user-facing Subscribe control is unverified. The inspected driver has no `IDLE` implementation identified; do not infer push mail from its idle-connection timeout. | [CrispinIMAP/imap4r1.c:1, 3874, 4135][src/CrispinIMAP/imap4r1.c], [imapdownload.c:2372][src/imapdownload.c], [Include/imapmailboxes.h:96][src/Include/imapmailboxes.h], [EsotericSettings60.r:1168][src/EsotericSettings60.r], [CrispinIMAP/imap4r1.c:350][src/CrispinIMAP/imap4r1.c] |
| Authentication / security | SASL GSSAPI, CRAM-MD5, PLAIN and LOGIN dispatch; Kerberos integration; certificate/SSL configuration and Keychain paths. This is historical interoperability evidence, not a modern security design. | [sasl.c:171][src/sasl.c], [imapauth.c:311][src/imapauth.c], [SettingsTwo.rsrc.r:2233][src/SettingsTwo.rsrc.r], [prefs.c:3977][src/prefs.c] |
| Legacy / directory access | UUPC file-maildrop reader; conditional Communications Toolbox transport; Ph, Finger and LDAP queries; MX/DNS routing controls; offline state and classic dial-up ownership tracking. | [uupc.c:38][src/uupc.c], [ctb.c:21][src/ctb.c], [EsotericSettings60.r:1068][src/EsotericSettings60.r], [SettingsTwo.rsrc.r:1332][src/SettingsTwo.rsrc.r], [main.c:881][src/main.c] |

**Summary-first UI:** the TOC keeps display/sort metadata adjacent in memory; message bodies load on demand, reuse an existing cache, and become purgeable under memory pressure. Loaded TOCs are reused by file specification, while flush/close logic protects active IMAP/background users. Mailbox redraw only invalidates visible affected rows, and optional deferred re-sorting avoids rearranging the list constantly. [Include/mailbox.h:76][src/Include/mailbox.h], [message.c:695][src/message.c], [toc.c:657, 688, 745][src/toc.c], [mailbox.c:1249][src/mailbox.c], [SettingsTwo.rsrc.r:1639][src/SettingsTwo.rsrc.r].

**Search and rule shortcuts:** bulk substring search uses **two** I/O buffers, each allocated in a **1–8 KiB** range, with synchronous/asynchronous read alternatives and cancellation. Filters cache compiled regexes, expanded nicknames and sender address-book hashes. Large nickname files consolidated many small allocations into a single handle with offsets, according to a **1996-07-17** implementation note. [search.c:248, 264, 300][src/search.c], [filtrun.c:1497, 1549, 1243][src/filtrun.c], [nickmng.c:27][src/nickmng.c].

**Background processing:** the source creates Mac **cooperative** threads, copies settings/account context for them, and separates receiving/sending from foreground filtering and batched delivery. Incremental IMAP filtering and idle-based notification keep expensive work out of immediate input handling. It limits concurrent check/send work, can queue a new send request, and exposes task progress and cancellation. This predates a modern preemptive Rust executor; copying its globals would defeat the intended separation. [threading.c:322, 331][src/threading.c], [mailxfer.c:96][src/mailxfer.c], [filtthread.c:25, 47, 106, 120][src/filtthread.c], [taskProgress.c:1][src/taskProgress.c].

**Connection economy:** POP overlap and SMTP recipient pipelining reduce request/response waits. IMAP connections are pooled by personality and checked for host/port/reuse suitability, then pinged before reuse; the default maximum setting is **5**, configurable. Idle-close and smaller-download options reduce cost on slow links. Classic PPP disconnection tests whether Eudora established the connection and whether active connections remain; the visible dial-up panel explicitly says those controls are unsupported on OS X. [pop.c:181][src/pop.c], [sendmail.c:759][src/sendmail.c], [imapconnections.c:184, 274][src/imapconnections.c], [Include/StringDefs.h:1253][src/Include/StringDefs.h], [EsotericSettings60.r:1168][src/EsotericSettings60.r], [main.c:881][src/main.c], [SettingsTwo.rsrc.r:1572][src/SettingsTwo.rsrc.r].

**Do expensive work only when needed:** spelling suggestions defer until mouse-down; previews and Content Concentrator are incremental; compaction combines contiguous copy ranges and tests space/waste thresholds. These are source-observed techniques, not measured throughput claims. No benchmark establishing historical performance or current superiority was run. [spell.c:740][src/spell.c], [Include/mailbox.h:153][src/Include/mailbox.h], [concentrator.c:1154][src/concentrator.c], [squish.c:245, 359][src/squish.c].

## 3. Options compared

The following are **design proposals**, not claims that Eudora already implements these backends. Standards and current documentation were checked on **2026-10-11**; provider deployment and account-specific capabilities still require live tests.

| Operation | IMAP + submission | JMAP Mail | Microsoft Graph |
|---|---|---|---|
| Folder membership / Transfer | Capability-tested `UID MOVE` or copy/delete sequence; confirm destination before destructive cleanup. | `Email/set` changes `mailboxIds`; copying between accounts uses `Email/copy`. | `POST /me/messages/{id}/move` moves within that user's mailbox, creating a destination copy and removing the original. |
| Read state / labels | System flags and advertised permanent keywords; local labels where unsupported. | Email `keywords`, plus local presentation metadata. | Adapter maps supported message properties; preserve unmatched local metadata. Exact category mapping needs a separate API audit. |
| Incremental synchronization | Track account/mailbox/UIDVALIDITY/UID identity and fetch changes. | Track state tokens and use the changes/query methods. | Message delta tracks additions, updates and deletions **within a folder**; retain returned opaque next/delta URLs. |
| Sending / timed queue | Separate submission transport; local durable queue and scheduler. | `EmailSubmission/set`; use only advertised scheduling capability, otherwise local queue. | `POST /me/sendMail` accepts JSON or MIME; **202 Accepted is not completed processing or delivery**. |
| True Redirect | Compose appropriate Resent fields only through a transport that accepts and preserves them. | Capability-test MIME import/submission and sender restrictions. | MIME input is documented; preservation/authorization of arbitrary Resent redistribution is **unverified**. |
| Rules | Local engine; optional portable Sieve subset where offered by the server. | Local engine; server rules require separately negotiated facilities. | Local engine; do not assume Graph message operations provide a complete Eudora rule service. |

The factual API basis is [IMAP4rev2, RFC 9051](https://www.rfc-editor.org/rfc/rfc9051.html) (**2021-08**), [IMAP MOVE, RFC 6851](https://www.rfc-editor.org/rfc/rfc6851.html) (**2013-01**), [JMAP Mail, RFC 8621](https://www.rfc-editor.org/rfc/rfc8621.html) (**2019-08**), [Graph move](https://learn.microsoft.com/en-us/graph/api/message-move?view=graph-rest-1.0), [Graph delta](https://learn.microsoft.com/en-us/graph/api/message-delta?view=graph-rest-1.0), [Graph sendMail](https://learn.microsoft.com/en-us/graph/api/user-sendmail?view=graph-rest-1.0), and [Resent fields in RFC 5322 §3.6.6](https://datatracker.ietf.org/doc/html/rfc5322#section-3.6.6) (**2008-10**). [Sieve, RFC 5228](https://www.rfc-editor.org/rfc/rfc5228.html) (**2008-01**) is a server filtering language; local UI, speech and attachment-file actions need client execution.

| Storage/search option | Advantage | Main cost / strongest argument against | Recommendation |
|---|---|---|---|
| Reuse Eudora mailbox/TOC format | Direct legacy compatibility; summary-first structure. | Fork dependence, native layout, mixed essential metadata/index and recovery ambiguities described above. | Import/export compatibility only. |
| SQLite metadata + immutable MIME blobs + SQLite FTS5 | One transactional metadata system; FTS5 has phrases, prefixes, proximity, Boolean queries, ranking and customizable tokenizers. | Correct blob/database ordering, garbage collection and query-tokenization behavior still require engineering. | Starting architecture; benchmark representative archives. |
| Same durable store + Tantivy index | Rust search engine separated from storage; attractive for richer search experiments. | Another lifecycle, commit boundary and index reconciliation mechanism. | Benchmark against FTS5; keep index replaceable. |
| Everything in a database, including large blobs | Potentially one commit boundary for content and metadata. | Backup/compaction and write-amplification trade-offs need measurements at mail-archive sizes. | Keep as a serious alternative, not an assumed loser. |

The candidate APIs are [rusqlite](https://docs.rs/rusqlite/latest/rusqlite/), [SQLite FTS5](https://www.sqlite.org/fts5.html) and [Tantivy](https://docs.rs/tantivy/latest/tantivy/), with documentation consulted **2026-10-11**. This is not a dependency-maintenance or performance certification. SQLite WAL permits concurrent readers/writers, but its durability depends on synchronization settings; WAL with `synchronous=NORMAL` can lose recent commits after power loss. Choose and verify a durable policy for drafts/settings. [SQLite WAL](https://www.sqlite.org/wal.html), [atomic-commit assumptions](https://www.sqlite.org/atomiccommit.html).

## 4. Recommendations for Sweetbriar

### 4.1 Distinctive workflows worth recreating

“Distinctive” means characteristic depth and composition in this source. Universal claims that modern clients lack a feature are **unverified**; current competing clients were not exhaustively audited.

| Eudora workflow and source evidence | Concrete modern reimplementation | Trade-off / strongest counter-argument |
|---|---|---|
| Make Filter + layered, address-aware rules. [makefilter.c:65][src/makefilter.c], [filtrun.c:1067][src/filtrun.c] | Rust typed rule AST; From/recipient/subject-generated preview; address-set/regex/junk predicates; explicit ordered actions and durable execution records. Map remote effects through backend capabilities; compile a supported subset to Sieve. | Arbitrary local actions will not run with the client closed. Server execution improves availability but cannot reproduce native UI actions. |
| Make Subject / editable summaries. [filtwin.c:4226][src/filtwin.c], [aete.r:286][src/aete.r] | Store original headers plus user `display_subject`/`display_sender`, notes and version history. Search both original and annotation; clearly mark local edits. | Annotations may not appear in other clients; rewriting original Subject risks confusing threads and archives. |
| Redirect, Turbo Redirect and Send Again. [message.c:3780, 2701][src/message.c] | Separate commands: redistribution, forward and new editable resend. Rust builder constructs new envelopes/Resent headers; confirmation names recipients and any original-message deletion. Preserve originals until confirmed remote effect. | Providers can restrict sender/redistribution semantics. A forward-as-attachment fallback is useful but must have its own name. |
| Stationery with identity/options/attachments and auto-replies. [compact.c:647][src/compact.c], [filtwin.c:5211][src/filtwin.c] | Versioned templates containing body, permitted header defaults, signature/identity and owned attachment blobs; placeholder expansion and preview. Shared core generates MIME using a reviewed builder. | Rich automatic templates can expose unintended recipients or attachments; simple snippets are easier to inspect. |
| Content Concentrator and multiple-message preview/reply. [concentrator.c:790, 1658][src/concentrator.c] | Rust quote/signature/forward-boundary parser produces reversible display ranges; native segmented preview shows omitted text on demand. Batch reply has explicit recipient and per-message choices. | Heuristics can hide relevant text. Preserve the full source and an immediate Expand All action. |
| Quantitative junk scoring, whitelist membership and correction training. [EsotericSettings60.r:1469][src/EsotericSettings60.r], [Include/emsapi-mac.h:1496][src/Include/emsapi-mac.h] | Record score, scorer/version, reasons and user override; declarative whitelist predicates; reversible Junk/Not Junk operations; score/age-based retention preview. | Provider junk judgments may disagree; local scoring should not silently fight server classification. |
| MoodWatch send pause and ScamWatch link checks. [anal.c:445][src/anal.c], [url.c:2469][src/url.c] | Optional local tone rules with visible explanations; durable send delay and cancel; compare displayed link and actual normalized destination before navigation. | Tone classification can be wrong; keep it advisory and optional. Link heuristics cannot guarantee phishing detection. |
| Nickname groups + Quick Recipients + speech rules. [nickexp.c:280][src/nickexp.c], [speechutil.c:952][src/speechutil.c] | Rust group/alias expansion with cycle detection; native completion, recipient menus and configurable spoken announcements. Use a platform adapter for contacts and speech. | Group expansion changes can surprise senders; show expanded recipients before sending. Speech can reveal mail aloud. |
| Saved live searches, 16-criterion builder and result actions. [searchwin.c:29, 4029][src/searchwin.c] | One query AST for text UI and native criteria builder; indexed body/headers/attachment names and metadata; saved queries subscribe to core changes. Actions target stable real-message IDs. | Arbitrary regex and unavailable remote bodies cannot always be indexed. Show coverage and unfinished downloads explicitly. |
| Keyboard-modifier depth and finish-message navigation. [menu.c:1632][src/menu.c], [SettingsTwo.rsrc.r:2988][src/SettingsTwo.rsrc.r] | Native command registry with menu validation, contextual shortcuts, next-unread/next-same-subject modes, customization and visible modifier alternatives. | Invisible modifiers impede discovery; expose equivalent named commands. |
| EMS translators and AppleScript-visible workflows. [Include/emsapi-mac.h:202][src/Include/emsapi-mac.h], [aete.r:148][src/aete.r] | Stable core operation API; Swift scripting bridge with opaque IDs; external translators in separate processes with declared input/output and permissions. Keep policy/core independent of Mac automation. | Plug-in ABI and scripting compatibility increase long-term support burden; start with a small documented contract. |
| Advanced settings, Link History, statistics and diagnostics. [url.c:465][src/url.c], [linkwin.c:477][src/linkwin.c], [statmng.c:814][src/statmng.c] | Typed searchable advanced preferences with defaults/help and validated import preview; optional local link/statistics store; structured task/operation diagnostics. | Hundreds of knobs create testing/support combinations. Promote recurring needs to coherent named policies. |

For the macOS implementation, use Swift/AppKit for the mailbox table, menus, editing, scripting and focus behavior; SwiftUI can host suitable settings surfaces. **Proposal:** bridge the Rust core through [UniFFI](https://mozilla.github.io/uniffi-rs/latest/) or a small C ABI, and evaluate [mail-parser](https://docs.rs/mail-parser/latest/mail_parser/) and [mail-builder](https://docs.rs/mail-builder/latest/mail_builder/) for MIME work. These documentation pages were checked **2026-10-11**; no claim is made that a crate eliminates malformed-mail testing.

Apple documentation checked **2026-10-11** confirms the relevant native APIs: [NSTableView](https://developer.apple.com/documentation/appkit/nstableview) for rows/columns, [NSSpellChecker](https://developer.apple.com/documentation/appkit/nsspellchecker) for Cocoa spelling/completion, [CNContactStore](https://developer.apple.com/documentation/contacts/cncontactstore) for Contacts access, and [WKWebView](https://developer.apple.com/documentation/webkit/wkwebview) for HTML content. **Proposal:** render sanitized mail HTML in a tightly controlled web view; retain native navigation, commands and composition ownership. Contacts authorization and HTML network behavior need explicit product design.

### 4.2 Data safety and implementation priorities

**Recommendation:** the Rust core should own immutable message originals, parsed projections, account identities, contact groups, rules, drafts, annotations, settings, synchronization state and a durable operation journal. Swift should own native presentation and platform services. This separation lets later Linux UI reuse the mail semantics while choosing its own native toolkit.

Treat search indexes and summary projections as rebuildable, but user-created state as primary data. Draft changes need durable revisions and recoverable attachment ownership. For separate blobs/database, persist and verify content before committing references; recover orphaned blobs conservatively. Keep explicit tombstones and retention periods, and reconcile interrupted remote copy/move/send before retrying. A send with an unknown outcome must not become an automatic duplicate submission. Graph's acceptance response and Eudora's compaction/error paths demonstrate why “accepted”, “saved locally” and “delivered” need distinct states. [Graph sendMail](https://learn.microsoft.com/en-us/graph/api/user-sendmail?view=graph-rest-1.0); [squish.c:317, 334][src/squish.c].

An absolute promise to never lose anything is not established by an architectural diagram. Make the requirement concrete: crash/power-loss recovery of committed drafts/settings; storage-error reporting before a false Saved indication; backup/restore reconciliation; stable identities despite backend moves; and tests of interrupted download, attachment import, send, rule execution and compaction/garbage collection. Proposed search acceptance tests should cover multilingual text, malformed MIME, local/remote coverage, cancellation and representative large archives.

Prioritize durable storage and import preservation, account/sync/submission boundaries, responsive summary views and indexed search; then restore generated filters, stationery, local annotations, redirect/resend, live searches and keyboard workflows. Add optional tone/speech/statistics and extension compatibility after those invariants are proven.

**Strongest argument against this recommendation:** a full Eudora-scale rule/template/automation system, immutable store and native interfaces on two platforms is a large maintenance commitment. A thin native interface over existing provider search/rules would ship sooner and benefit from server execution while offline. The case for the proposed core is Sweetbriar's stated cross-platform reuse, heterogeneous sources and durable local ownership; those goals justify the cost, but do not justify implementing every historical knob in the first release.

## 5. Risks and open questions

- **Release identity:** the checkout says 6.2.5 build 9. Exact 6.2.4 parity needs its binary/resources or an authenticated tag; currently **unverified**. [Include/conf.h:108][src/Include/conf.h], [Include/buildversion.h:22][src/Include/buildversion.h].
- **Missing proprietary/third-party pieces:** CHM removed non-distributable material; TAE, spelling dictionaries and installed translator/importer/ESP packages require separate verification. Visible declarations must not be treated as functioning bundled engines. [CHM](https://computerhistory.org/blog/the-eudora-email-client-source-code/); [anal.c:331][src/anal.c], [spell.c:26][src/spell.c], [pgpout.c:28][src/pgpout.c].
- **Conditional builds and stale descriptions:** runtime paid gates, `NEVER`, classic/Carbon alternatives and reversed preferences can change behavior. Historical UI labels are not reliable enough for an importer without testing actual resource values. [FiltDefs.c:132][src/FiltDefs.c], [PrefDefs.strn:130][src/PrefDefs.strn], [toc.c:429][src/toc.c].
- **Archive migration fidelity:** which fork survives the user's backups; whether external attachments/photos/spools exist; which annotations/queued times remain salvageable; whether deleted records should be imported as recovered rather than ordinary mail. Require a reversible import report. [buildtoc.c:136][src/buildtoc.c], [pop.c:2076][src/pop.c].
- **Remote capability gaps:** partial download and missing bodies make some predicates unknown; server keyword/redirect/scheduling constraints differ. IMAP/JMAP/Graph adapters need capability evidence and account-level tests, not a common lowest-level API assumed to fit all sources. [RFC 9051](https://www.rfc-editor.org/rfc/rfc9051.html), [RFC 8621](https://www.rfc-editor.org/rfc/rfc8621.html), [Graph sendMail](https://learn.microsoft.com/en-us/graph/api/user-sendmail?view=graph-rest-1.0).
- **Security compatibility:** retain historical charset/MIME decoding where useful, but do not reproduce old security negotiation. TLS 1.0/1.1 were deprecated by **RFC 8996, March 2021**, checked **2026-10-11**. Reading receipts are discretionary notifications, not guaranteed proof: [MDN RFC 8098](https://www.rfc-editor.org/rfc/rfc8098.html), **April 2017**. [RFC 8996](https://datatracker.ietf.org/doc/html/rfc8996); [SettingsTwo.rsrc.r:2233][src/SettingsTwo.rsrc.r].
- **Search semantics/performance:** whether FTS5 or Tantivy meets the target requires benchmarks; no historical or modern throughput was measured here. Ensure original and locally edited subjects remain separately searchable. [FTS5](https://www.sqlite.org/fts5.html), [Tantivy](https://docs.rs/tantivy/latest/tantivy/); [filtwin.c:4226][src/filtwin.c].

## 6. Sources and reproducibility

Primary evidence is the supplied checkout. All inline file citations point into `src/` using normalized logical line numbers. The audited excerpts are in `audit/source/`; inventory artifacts are [all menus](codex-artifacts/audit/menu-catalog.md), [all panel controls and bindings](codex-artifacts/audit/panel-controls.md), [all 1,033 documented setting IDs](codex-artifacts/audit/x-eudora-settings.csv), [source manifest](codex-artifacts/audit/source-manifest.json) and [resource extractor](codex-artifacts/audit/extract-resources.py). These supplement the complete menu/panel tables above and preserve long setting descriptions without misrepresenting every UI string as a feature.

External URLs actually consulted, all accessed **2026-10-11**:

- [CHM source-release account](https://computerhistory.org/blog/the-eudora-email-client-source-code/); [source mirror](https://github.com/pmachapman/eudora-mac).
- [RFC 9051 — IMAP4rev2](https://www.rfc-editor.org/rfc/rfc9051.html); [RFC 6851 — IMAP MOVE](https://www.rfc-editor.org/rfc/rfc6851.html); [RFC 8621 — JMAP Mail](https://www.rfc-editor.org/rfc/rfc8621.html); [RFC 5322 — Internet Message Format](https://datatracker.ietf.org/doc/html/rfc5322); [RFC 5228 — Sieve](https://www.rfc-editor.org/rfc/rfc5228.html); [RFC 8098 — MDN](https://www.rfc-editor.org/rfc/rfc8098.html); [RFC 8996 — TLS deprecation](https://datatracker.ietf.org/doc/html/rfc8996).
- [Graph sendMail](https://learn.microsoft.com/en-us/graph/api/user-sendmail?view=graph-rest-1.0); [Graph move](https://learn.microsoft.com/en-us/graph/api/message-move?view=graph-rest-1.0); [Graph delta](https://learn.microsoft.com/en-us/graph/api/message-delta?view=graph-rest-1.0).
- [mail-parser](https://docs.rs/mail-parser/latest/mail_parser/); [mail-builder](https://docs.rs/mail-builder/latest/mail_builder/); [rusqlite](https://docs.rs/rusqlite/latest/rusqlite/); [Tantivy](https://docs.rs/tantivy/latest/tantivy/); [UniFFI](https://mozilla.github.io/uniffi-rs/latest/).
- [SQLite FTS5](https://www.sqlite.org/fts5.html); [WAL](https://www.sqlite.org/wal.html); [atomic commit](https://www.sqlite.org/atomiccommit.html).
- Apple [NSTableView](https://developer.apple.com/documentation/appkit/nstableview), [NSSpellChecker](https://developer.apple.com/documentation/appkit/nsspellchecker), [CNContactStore](https://developer.apple.com/documentation/contacts/cncontactstore), [WKWebView](https://developer.apple.com/documentation/webkit/wkwebview). Their linked Markdown versions were also fetched because the HTML tool exposed only JavaScript shells: [table](https://developer.apple.com/documentation/appkit/nstableview.md), [spelling](https://developer.apple.com/documentation/appkit/nsspellchecker.md), [contacts](https://developer.apple.com/documentation/contacts/cncontactstore.md), [web view](https://developer.apple.com/documentation/webkit/wkwebview.md).

The report was saved incrementally. Verification consists of resource extraction, tracing relevant handlers and storage paths, checking source-link targets/line bounds, and checking the cited current documentation. It does not include execution of Eudora, a historical build, provider integration tests or a survey of every present-day email client.

[src/AddressBookTabs/homeTab.r]: https://github.com/pmachapman/eudora-mac/blob/77c41a09c29d36dd4ac18cd28e530c327746ba98/AddressBookTabs/homeTab.r
[src/AddressBookTabs/notesTab.r]: https://github.com/pmachapman/eudora-mac/blob/77c41a09c29d36dd4ac18cd28e530c327746ba98/AddressBookTabs/notesTab.r
[src/AddressBookTabs/otherTab.r]: https://github.com/pmachapman/eudora-mac/blob/77c41a09c29d36dd4ac18cd28e530c327746ba98/AddressBookTabs/otherTab.r
[src/AddressBookTabs/personalTab.r]: https://github.com/pmachapman/eudora-mac/blob/77c41a09c29d36dd4ac18cd28e530c327746ba98/AddressBookTabs/personalTab.r
[src/AddressBookTabs/photoTab.r]: https://github.com/pmachapman/eudora-mac/blob/77c41a09c29d36dd4ac18cd28e530c327746ba98/AddressBookTabs/photoTab.r
[src/AddressBookTabs/workTab.r]: https://github.com/pmachapman/eudora-mac/blob/77c41a09c29d36dd4ac18cd28e530c327746ba98/AddressBookTabs/workTab.r
[src/Common.r]: https://github.com/pmachapman/eudora-mac/blob/77c41a09c29d36dd4ac18cd28e530c327746ba98/Common.r
[src/CrispinIMAP/imap4r1.c]: https://github.com/pmachapman/eudora-mac/blob/77c41a09c29d36dd4ac18cd28e530c327746ba98/CrispinIMAP/imap4r1.c
[src/EsotericSettings60.r]: https://github.com/pmachapman/eudora-mac/blob/77c41a09c29d36dd4ac18cd28e530c327746ba98/EsotericSettings60.r
[src/FiltDefs.c]: https://github.com/pmachapman/eudora-mac/blob/77c41a09c29d36dd4ac18cd28e530c327746ba98/FiltDefs.c
[src/FiltDefs.r]: https://github.com/pmachapman/eudora-mac/blob/77c41a09c29d36dd4ac18cd28e530c327746ba98/FiltDefs.r
[src/HelpText.r]: https://github.com/pmachapman/eudora-mac/blob/77c41a09c29d36dd4ac18cd28e530c327746ba98/HelpText.r
[src/Include/StringDefs.h]: https://github.com/pmachapman/eudora-mac/blob/77c41a09c29d36dd4ac18cd28e530c327746ba98/Include/StringDefs.h
[src/Include/anal.h]: https://github.com/pmachapman/eudora-mac/blob/77c41a09c29d36dd4ac18cd28e530c327746ba98/Include/anal.h
[src/Include/buildversion.h]: https://github.com/pmachapman/eudora-mac/blob/77c41a09c29d36dd4ac18cd28e530c327746ba98/Include/buildversion.h
[src/Include/conf.h]: https://github.com/pmachapman/eudora-mac/blob/77c41a09c29d36dd4ac18cd28e530c327746ba98/Include/conf.h
[src/Include/emsapi-mac.h]: https://github.com/pmachapman/eudora-mac/blob/77c41a09c29d36dd4ac18cd28e530c327746ba98/Include/emsapi-mac.h
[src/Include/filters.h]: https://github.com/pmachapman/eudora-mac/blob/77c41a09c29d36dd4ac18cd28e530c327746ba98/Include/filters.h
[src/Include/imapmailboxes.h]: https://github.com/pmachapman/eudora-mac/blob/77c41a09c29d36dd4ac18cd28e530c327746ba98/Include/imapmailboxes.h
[src/Include/mailbox.h]: https://github.com/pmachapman/eudora-mac/blob/77c41a09c29d36dd4ac18cd28e530c327746ba98/Include/mailbox.h
[src/Include/prefdefs.h]: https://github.com/pmachapman/eudora-mac/blob/77c41a09c29d36dd4ac18cd28e530c327746ba98/Include/prefdefs.h
[src/Include/statmng.h]: https://github.com/pmachapman/eudora-mac/blob/77c41a09c29d36dd4ac18cd28e530c327746ba98/Include/statmng.h
[src/PrefDefs]: https://github.com/pmachapman/eudora-mac/blob/77c41a09c29d36dd4ac18cd28e530c327746ba98/PrefDefs
[src/PrefDefs.strn]: https://github.com/pmachapman/eudora-mac/blob/77c41a09c29d36dd4ac18cd28e530c327746ba98/PrefDefs.strn
[src/SettingsTwo.rsrc.r]: https://github.com/pmachapman/eudora-mac/blob/77c41a09c29d36dd4ac18cd28e530c327746ba98/SettingsTwo.rsrc.r
[src/StringDefs.xset]: https://github.com/pmachapman/eudora-mac/blob/77c41a09c29d36dd4ac18cd28e530c327746ba98/StringDefs.xset
[src/StrnDefs]: https://github.com/pmachapman/eudora-mac/blob/77c41a09c29d36dd4ac18cd28e530c327746ba98/StrnDefs
[src/StrnDefs.strn]: https://github.com/pmachapman/eudora-mac/blob/77c41a09c29d36dd4ac18cd28e530c327746ba98/StrnDefs.strn
[src/X-Eudora-Settings.txt]: https://github.com/pmachapman/eudora-mac/blob/77c41a09c29d36dd4ac18cd28e530c327746ba98/X-Eudora-Settings.txt
[src/acap.r]: https://github.com/pmachapman/eudora-mac/blob/77c41a09c29d36dd4ac18cd28e530c327746ba98/acap.r
[src/aete.r]: https://github.com/pmachapman/eudora-mac/blob/77c41a09c29d36dd4ac18cd28e530c327746ba98/aete.r
[src/anal.c]: https://github.com/pmachapman/eudora-mac/blob/77c41a09c29d36dd4ac18cd28e530c327746ba98/anal.c
[src/appleevent.c]: https://github.com/pmachapman/eudora-mac/blob/77c41a09c29d36dd4ac18cd28e530c327746ba98/appleevent.c
[src/buildtoc.c]: https://github.com/pmachapman/eudora-mac/blob/77c41a09c29d36dd4ac18cd28e530c327746ba98/buildtoc.c
[src/carbon.r]: https://github.com/pmachapman/eudora-mac/blob/77c41a09c29d36dd4ac18cd28e530c327746ba98/carbon.r
[src/classic.r]: https://github.com/pmachapman/eudora-mac/blob/77c41a09c29d36dd4ac18cd28e530c327746ba98/classic.r
[src/common.rsrc.r]: https://github.com/pmachapman/eudora-mac/blob/77c41a09c29d36dd4ac18cd28e530c327746ba98/common.rsrc.r
[src/compact.c]: https://github.com/pmachapman/eudora-mac/blob/77c41a09c29d36dd4ac18cd28e530c327746ba98/compact.c
[src/concentrator.c]: https://github.com/pmachapman/eudora-mac/blob/77c41a09c29d36dd4ac18cd28e530c327746ba98/concentrator.c
[src/ctb.c]: https://github.com/pmachapman/eudora-mac/blob/77c41a09c29d36dd4ac18cd28e530c327746ba98/ctb.c
[src/ends.c]: https://github.com/pmachapman/eudora-mac/blob/77c41a09c29d36dd4ac18cd28e530c327746ba98/ends.c
[src/export.c]: https://github.com/pmachapman/eudora-mac/blob/77c41a09c29d36dd4ac18cd28e530c327746ba98/export.c
[src/fileview.c]: https://github.com/pmachapman/eudora-mac/blob/77c41a09c29d36dd4ac18cd28e530c327746ba98/fileview.c
[src/filtmng.c]: https://github.com/pmachapman/eudora-mac/blob/77c41a09c29d36dd4ac18cd28e530c327746ba98/filtmng.c
[src/filtrun.c]: https://github.com/pmachapman/eudora-mac/blob/77c41a09c29d36dd4ac18cd28e530c327746ba98/filtrun.c
[src/filtthread.c]: https://github.com/pmachapman/eudora-mac/blob/77c41a09c29d36dd4ac18cd28e530c327746ba98/filtthread.c
[src/filtwin.c]: https://github.com/pmachapman/eudora-mac/blob/77c41a09c29d36dd4ac18cd28e530c327746ba98/filtwin.c
[src/imapauth.c]: https://github.com/pmachapman/eudora-mac/blob/77c41a09c29d36dd4ac18cd28e530c327746ba98/imapauth.c
[src/imapconnections.c]: https://github.com/pmachapman/eudora-mac/blob/77c41a09c29d36dd4ac18cd28e530c327746ba98/imapconnections.c
[src/imapdownload.c]: https://github.com/pmachapman/eudora-mac/blob/77c41a09c29d36dd4ac18cd28e530c327746ba98/imapdownload.c
[src/imapmailboxes.c]: https://github.com/pmachapman/eudora-mac/blob/77c41a09c29d36dd4ac18cd28e530c327746ba98/imapmailboxes.c
[src/import.c]: https://github.com/pmachapman/eudora-mac/blob/77c41a09c29d36dd4ac18cd28e530c327746ba98/import.c
[src/linkmng.c]: https://github.com/pmachapman/eudora-mac/blob/77c41a09c29d36dd4ac18cd28e530c327746ba98/linkmng.c
[src/linkwin.c]: https://github.com/pmachapman/eudora-mac/blob/77c41a09c29d36dd4ac18cd28e530c327746ba98/linkwin.c
[src/mailbox.c]: https://github.com/pmachapman/eudora-mac/blob/77c41a09c29d36dd4ac18cd28e530c327746ba98/mailbox.c
[src/mailxfer.c]: https://github.com/pmachapman/eudora-mac/blob/77c41a09c29d36dd4ac18cd28e530c327746ba98/mailxfer.c
[src/main.c]: https://github.com/pmachapman/eudora-mac/blob/77c41a09c29d36dd4ac18cd28e530c327746ba98/main.c
[src/makefile]: https://github.com/pmachapman/eudora-mac/blob/77c41a09c29d36dd4ac18cd28e530c327746ba98/makefile
[src/makefilter.c]: https://github.com/pmachapman/eudora-mac/blob/77c41a09c29d36dd4ac18cd28e530c327746ba98/makefilter.c
[src/menu.c]: https://github.com/pmachapman/eudora-mac/blob/77c41a09c29d36dd4ac18cd28e530c327746ba98/menu.c
[src/messact.c]: https://github.com/pmachapman/eudora-mac/blob/77c41a09c29d36dd4ac18cd28e530c327746ba98/messact.c
[src/message.c]: https://github.com/pmachapman/eudora-mac/blob/77c41a09c29d36dd4ac18cd28e530c327746ba98/message.c
[src/mime.c]: https://github.com/pmachapman/eudora-mac/blob/77c41a09c29d36dd4ac18cd28e530c327746ba98/mime.c
[src/msmaildb.c]: https://github.com/pmachapman/eudora-mac/blob/77c41a09c29d36dd4ac18cd28e530c327746ba98/msmaildb.c
[src/mstore.c]: https://github.com/pmachapman/eudora-mac/blob/77c41a09c29d36dd4ac18cd28e530c327746ba98/mstore.c
[src/nickexp.c]: https://github.com/pmachapman/eudora-mac/blob/77c41a09c29d36dd4ac18cd28e530c327746ba98/nickexp.c
[src/nickmng.c]: https://github.com/pmachapman/eudora-mac/blob/77c41a09c29d36dd4ac18cd28e530c327746ba98/nickmng.c
[src/osxabsync.cp]: https://github.com/pmachapman/eudora-mac/blob/77c41a09c29d36dd4ac18cd28e530c327746ba98/osxabsync.cp
[src/palmconduitae.c]: https://github.com/pmachapman/eudora-mac/blob/77c41a09c29d36dd4ac18cd28e530c327746ba98/palmconduitae.c
[src/pgpin.c]: https://github.com/pmachapman/eudora-mac/blob/77c41a09c29d36dd4ac18cd28e530c327746ba98/pgpin.c
[src/pgpout.c]: https://github.com/pmachapman/eudora-mac/blob/77c41a09c29d36dd4ac18cd28e530c327746ba98/pgpout.c
[src/pop.c]: https://github.com/pmachapman/eudora-mac/blob/77c41a09c29d36dd4ac18cd28e530c327746ba98/pop.c
[src/prefs.c]: https://github.com/pmachapman/eudora-mac/blob/77c41a09c29d36dd4ac18cd28e530c327746ba98/prefs.c
[src/prefs.xset]: https://github.com/pmachapman/eudora-mac/blob/77c41a09c29d36dd4ac18cd28e530c327746ba98/prefs.xset
[src/sasl.c]: https://github.com/pmachapman/eudora-mac/blob/77c41a09c29d36dd4ac18cd28e530c327746ba98/sasl.c
[src/schizo.c]: https://github.com/pmachapman/eudora-mac/blob/77c41a09c29d36dd4ac18cd28e530c327746ba98/schizo.c
[src/scriptmenu.c]: https://github.com/pmachapman/eudora-mac/blob/77c41a09c29d36dd4ac18cd28e530c327746ba98/scriptmenu.c
[src/search.c]: https://github.com/pmachapman/eudora-mac/blob/77c41a09c29d36dd4ac18cd28e530c327746ba98/search.c
[src/searchwin.c]: https://github.com/pmachapman/eudora-mac/blob/77c41a09c29d36dd4ac18cd28e530c327746ba98/searchwin.c
[src/sendmail.c]: https://github.com/pmachapman/eudora-mac/blob/77c41a09c29d36dd4ac18cd28e530c327746ba98/sendmail.c
[src/speechutil.c]: https://github.com/pmachapman/eudora-mac/blob/77c41a09c29d36dd4ac18cd28e530c327746ba98/speechutil.c
[src/spell.c]: https://github.com/pmachapman/eudora-mac/blob/77c41a09c29d36dd4ac18cd28e530c327746ba98/spell.c
[src/spool.c]: https://github.com/pmachapman/eudora-mac/blob/77c41a09c29d36dd4ac18cd28e530c327746ba98/spool.c
[src/squish.c]: https://github.com/pmachapman/eudora-mac/blob/77c41a09c29d36dd4ac18cd28e530c327746ba98/squish.c
[src/stationerywin.c]: https://github.com/pmachapman/eudora-mac/blob/77c41a09c29d36dd4ac18cd28e530c327746ba98/stationerywin.c
[src/statmng.c]: https://github.com/pmachapman/eudora-mac/blob/77c41a09c29d36dd4ac18cd28e530c327746ba98/statmng.c
[src/taskProgress.c]: https://github.com/pmachapman/eudora-mac/blob/77c41a09c29d36dd4ac18cd28e530c327746ba98/taskProgress.c
[src/threading.c]: https://github.com/pmachapman/eudora-mac/blob/77c41a09c29d36dd4ac18cd28e530c327746ba98/threading.c
[src/toc.c]: https://github.com/pmachapman/eudora-mac/blob/77c41a09c29d36dd4ac18cd28e530c327746ba98/toc.c
[src/two.rsrc.r]: https://github.com/pmachapman/eudora-mac/blob/77c41a09c29d36dd4ac18cd28e530c327746ba98/two.rsrc.r
[src/url.c]: https://github.com/pmachapman/eudora-mac/blob/77c41a09c29d36dd4ac18cd28e530c327746ba98/url.c
[src/uupc.c]: https://github.com/pmachapman/eudora-mac/blob/77c41a09c29d36dd4ac18cd28e530c327746ba98/uupc.c
[src/vcard.c]: https://github.com/pmachapman/eudora-mac/blob/77c41a09c29d36dd4ac18cd28e530c327746ba98/vcard.c