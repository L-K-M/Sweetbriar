<!-- provenance: helper=codex model=gpt-6.1-sol started=2026-10-11T18:24:02+00:00 exit=0 brief=research/briefs/ -->

# Musashi: original-archive extraction and lessons for Sweetbriar

Research and verification date: **2026-10-11**. Scope: the supplied Japanese 1.2.4 archive, English and Japanese classic PowerPC 3.4.1 distributions, and PowerPC Mac OS X 3.4.1 distribution. **The applications were neither run nor emulated.**

## 1. TL;DR

- **Extraction succeeded completely:** 128 files, 256 fork sidecars, 189 nonempty forks; every fork passed its applicable checksum and declared-length check. Resource types, IDs and payload bytes in all 74 extracted resource files agreed with an independent resource parser. [Extraction results][extraction], [independent verification][verification].
- A reusable **pure-Python extractor** now handles the methods actually present: stored data, method 13 LZSS/Huffman, and method 15 Arsenic. Original filename bytes, names, Finder metadata, type/creator codes, fork offsets and hashes are preserved. [Tool and reproduction instructions][tools].
- **106 textual documents were converted to UTF-8.** The 3.4.1 packages contain substantial HTML manuals, not merely read-me files; both languages’ 17 chapters are byte-identical between their classic and Mac OS X copies. [Document index][documents], [comparison][manual-comparison].
- Musashi’s distinctive ideas include **local users, permission-controlled mailbox sharing in 1.2.4, serverless forwarding between users in 3.4.1, virtual message viewers, editable subject metadata, and reusable mail templates.** [1.2.4 usage][usage124]; [EN §10][e10], [JP §14, translated][j14]; `CDIL` 1230 in [E].
- The Japanese manual establishes details missing from the English translation: **three-level folder nesting, folders containing either messages or subfolders, progressive search within results, and Control-1…9 variable-speed reading.** [JP §§7,14, translated][j7], [j14].
- Rules support **two conditions, AND/OR, move/highlight actions, incoming/outgoing application, and first-match priority**; a search can load a filter’s conditions. [JP §12, translated][j12]; `CDIL` 1120/1150, `CDCG` 1120/1150, `MENU` 502/503 in [E].
- **Authentication documentation is stale:** the manual says CRAM-MD5 only, but the Japanese release history records LOGIN/PLAIN additions on **2001-12-22**. All three 3.4.1 executables contain those mechanism names. [Japanese release notes, translated][release-j]; [binary string locations][protocol-strings].
- **Platform differences matter:** classic 3.4.1 has a Modules preference; Mac OS X ships four preference tabs and embeds the module’s visual/sound resources. No `.nib` or AppleScript dictionary was recovered. `CDCG` 1140/`CDIL` 1145 in [E]; `tab#` 1140 in [X-E]; [resource/file inventory][inventory].
- For Sweetbriar, revive **one actionable message viewer for folders, arrivals and searches; shared search/rule predicates; visible queue state; account-aware templates; and optional personal organization**. Preserve original mail bytes and make these operations transactional. These are recommendations, not claims that Musashi already provided modern durability guarantees.

## 2. Findings

### 2.1. Extraction, provenance and reproducibility

Macintosh Garden identifies Musashi as a Sonosoft client released in 1997 and lists the supplied distributions. Its page was fetched successfully over **HTTP on 2026-10-11**, after HTTPS failed; a local snapshot is retained. The assignment states that the original downloads’ MD5s matched the site. The original HQX containers are absent here, so this investigation independently fingerprints and verifies the **decoded archives**, rather than claiming to have repeated the original HQX MD5 comparison. [Garden page](http://macintoshgarden.org/apps/musashi), [retrieved snapshot][garden-snapshot].

| Archive | Format; methods on nonempty forks | Files / directories | Nonempty forks | Expanded bytes, both forks |
|---|---|---:|---:|---:|
| `MUSASHI_PPC_124J.sit.data` | Classic StuffIt; 13 | 6 / 2 | 12 | 790,593 |
| `mu-ppc-341-en.sea.data` | StuffIt 5; 15 and 0 | 32 / 5 | 61 | 1,104,689 |
| `mu-ppc-341-jp.sea.data` | StuffIt 5; 15 and 0 | 29 / 5 | 55 | 1,128,692 |
| `mu-osx-341.sit.data` | StuffIt 5; 15 and 0 | 61 / 13 | 61 | 1,621,317 |

All three 3.4.1 archives mix method 15 with stored files; for example, the English IE manual launcher has a 38-byte stored data fork. Counts and sizes above come from the fully consumed archive manifests. Every file has `.data` and `.rsrc` outputs, including zero-length forks. [Extraction summary][extraction], [1.2.4 manifest][manifest124], [English manifest][manifest-e], [Japanese manifest][manifest-j], [Mac OS X manifest][manifest-x].

The extractor follows the published XADMaster container layouts. For classic StuffIt it checks the 112-byte entry headers and their CRC16s, tracks folder start/end records, and reads resource data before file data. For StuffIt 5 it reads variable-length headers, parent-directory offsets, Finder metadata and both fork descriptors; it also recognizes the 48-byte directory markers, including the final marker outside the root-entry count. File/directory headers and those markers pass CRC16 verification. Unsupported compression and encryption fail explicitly. [Container tool][stuffit], [classic layout](https://raw.githubusercontent.com/MacPaw/XADMaster/master/XADStuffItParser.m), [StuffIt 5 layout](https://raw.githubusercontent.com/MacPaw/XADMaster/master/XADStuffIt5Parser.m).

Method 13 uses separate literal/match Huffman contexts, variable match lengths, distance coding and a 64 KiB history window. The 1.2.4 corpus exercises custom code tables and predefined selectors 1, 2 and 4. Method 15 decodes arithmetic models, move-to-front data, inverse Burrows-Wheeler blocks, optional de-randomization and run-length coding, then verifies the stream’s embedded CRC32. The implementations and lookup tables retain XADMaster attribution and its LGPL text. [Method 13 tool][method13], [reference implementation](https://raw.githubusercontent.com/MacPaw/XADMaster/master/XADStuffIt13Handle.m); [Arsenic tool][arsenic], [reference implementation](https://raw.githubusercontent.com/MacPaw/XADMaster/master/XADStuffItArsenicHandle.m), [BWT source](https://raw.githubusercontent.com/MacPaw/XADMaster/master/BWT.c).

Names were not uniformly Shift-JIS: Japanese names in the 3.4.1 containers are valid UTF-8; 1.2.4 Japanese names use the legacy fallback. Exact name bytes are retained in `name_raw_hex`, with the selected encoding. Portable sidecars and JSON preserve fork metadata; this does not reconstruct a native HFS filesystem or Finder extended attributes. [Manifests][manifest-j], [manifest124], [extractor][stuffit].

The extractor needs only Python’s standard library. The analysis environment, installed locally on **2026-10-11**, additionally uses `rsrcfork` **1.8.0** for an independent resource-map comparison and `striprtf` **0.0.33** for RTF conversion; these are the installed versions, not assertions about future releases. [Reproduction instructions][tools], [rsrcfork](https://pypi.org/project/rsrcfork/), [striprtf](https://pypi.org/project/striprtf/).

An independent helper audit found no material extraction issue: it rechecked all 256 output hashes, all 82 non-Arsenic fork CRC16s, the decoding tables against XADMaster, and the absence of missing or colliding output paths. All 3,747 resource entries parsed without errors. Arsenic forks use their embedded CRC32, as in XADMaster; independent resource comparison covers payload bytes keyed by type/ID, not resource names or attributes. The supplied BinHex decoder does not verify its outer CRCs, and the original HQX files are unavailable. The two `.sea.rsrc` input files contain the self-extractor stub, not archive members; their stub UI is outside the application’s resource inventory. [Independent audit][audit], [verification][verification].

REA was attempted first as instructed. This server rejected StuffIt; its plist inspection additionally required macOS utilities unavailable on this Linux host. The retained failure record makes that limitation explicit. Python resource parsing and `plistlib` supplied the needed static evidence. [REA record][rea], [parsed bundle metadata][plist].

### 2.2. What the archives contain

| Distribution | Applications and supporting files | Documentation and examples |
|---|---|---|
| Japanese 1.2.4 | `MUSASHI-J(PPC) 1.2.4`, type `APPL`, creator `MUSA` | Four SimpleText documents: release notes, usage, registration, mandatory read-me; `Template/MUSASHIユーザ登録`, a registration-message template. [Manifest][manifest124]. |
| English classic 3.4.1 | `Musashi`; separate `Register` application; `Modules/Default Module`; `Modules/Musashi Sounds`; `Plug-Ins/PFC` | 17 HTML manual files (`00`–`14`, `AA`, `AB`), five prose/read-me documents, three templates (`Register`, `Report`, `Upgrade`), two browser-specific manual launchers. [Manifest][manifest-e]. |
| Japanese classic 3.4.1 | Same main application/supporting layout; `PFC-jp` | 17 Japanese HTML chapters, introductory/release documents, three corresponding registration/report/upgrade templates, two browser launchers. [Manifest][manifest-j]. |
| Mac OS X 3.4.1 | `Musashi.app`, including executable, plists, `PkgInfo`, icon, base resource file, and six localized resource/InfoPlist pairs | English and Japanese HTML manuals, six RTF read-me/release/compatibility files, two manual entry pages, `.DS_Store`. No separate Register, Templates, Modules or Plug-Ins directories are packaged. [Manifest][manifest-x]. |

SimpleText prose resides in the **data fork**; `styl` resource 128 supplies formatting runs. It is misleading to assume every SimpleText document holds its text in a `TEXT` resource. The separate Register utility does contain `TEXT` resources, which were dumped too. No example mailbox, real mail corpus, `.nib`, separate Apple Guide file or additional help database appears in the complete extracted member list. [File inventory][inventory], [all resource files][resource-index], [document conversions][documents].

Japanese legacy text was decoded with Apple’s published **MacJapanese** mapping, retaining its private transcoding hints rather than substituting replacement characters. HTML declared as Shift-JIS uses that declared encoding; the Cocoa-generated RTF files use their encoding/font information. This distinction matters for MacJapanese’s extra characters and punctuation. Original bytes and each conversion’s provenance remain available. [Conversion tool][doc-tool], [mapping](https://www.unicode.org/Public/MAPPINGS/VENDORS/APPLE/JAPANESE.TXT), [document index][documents].

All Japanese-to-English explanations below are **translations**, indicated by “JP, translated” or “1.2.4, translated.” Resource dumps preserve original Japanese. Translations normalize typography and obvious label truncations only when explained; they do not invent runtime behavior.

### 2.3. Resource inventory and bundle evidence

The principal resource files are referenced throughout as follows. Each linked dump identifies the original extracted fork, and each resource includes its type, signed ID, name, attributes, offset, length and SHA-256. Raw `.bin` files and individual decoded `.json` files sit beside each dump. [Resource index][resource-index].

| Evidence key | Original extracted file |
|---|---|
| **124** | `extracted/jp124/MUSASHI-J(PPC) Folder/MUSASHI-J(PPC) 1.2.4.rsrc` — [124]. |
| **E** | `extracted/en341/Musashi-3.4.1-en/Musashi.rsrc` — [E]. |
| **J** | `extracted/jp341/Musashi-3.4.1-jp/Musashi.rsrc` — [J]. |
| **X-E / X-J** | Mac OS X `Contents/Resources/English.lproj/Localized.rsrc.data` / `Japanese.lproj/Localized.rsrc.data` — [X-E], [X-J]. |
| **X-base** | Mac OS X `Contents/Resources/Musashi.rsrc.data` — [X-base]. |

| Resource type | 1.2.4 application | Classic 3.4.1 English | Mac OS X English localization |
|---|---:|---:|---:|
| `MENU` | 30 | 43 | 43 |
| `STR#` | 16 | 38 | 38 |
| `DITL` / `DLOG` | 45 / 38 | 2 / 2 | 0 / 0 |
| Custom `CDIL` / `CDLG` | 0 / 0 | 46 / 39 | 45 / 39 |
| `AMIT` contextual menu lists | 0 | 12 | 12 |
| `CDCG` choice groups | 0 | 9 | 8 |
| `CDTX` text styling | 0 | 7 | 7 |
| `CMNM` menu modifiers / `CPNL` button panels | 0 / 0 | 10 / 5 | 10 / 5 |
| `MPIF` conversion/import/export declarations | 0 | 24 | 24 |
| `TMPL` format descriptions | 0 | 9 | 9 |
| `tab#` | 0 | 0 | 2 |
| `BNDL` / `FREF` / `vers` | 1 / 10 / 2 | 1 / 12 / 2 | 0 / 0 / 0 |

These counts are direct observations in [124], [E] and [X-E]. Classic Japanese has 28 `MPIF` resources; Mac OS X Japanese likewise has 28. The Mac OS X **base** file separately supplies one `BNDL`, twelve `FREF`, 106 `cicn`, four `clut`, five `snd ` and other icons/cursors/window resources. The complete type/count lists, including code and image resources, are retained rather than reduced to the UI types shown here. [J], [X-J], [X-base], [resource index][resource-index].

Full type/count inventories for the principal files (the trailing space in `snd ` is part of its type):

- **1.2.4 application:** `BNDL` 1, `CDEF` 6, `CNTL` 20, `CURS` 3, `DITL` 45, `DLOG` 38, `FREF` 10, `ICN#` 44, `MENU` 30, `MUSA` 1, `PICT` 1, `SICN` 1, `SIZE` 1, `STR#` 16, `WIND` 5, `cfrg` 1, `cicn` 82, `dctb` 35, `icl4` 44, `icl8` 44, `ics#` 44, `ics4` 44, `ics8` 44, `mrgi` 1, `snd ` 2, `vers` 2, `wctb` 5. [124].
- **Classic 3.4.1 English application:** `AMIT` 12, `BNDL` 1, `CDCG` 9, `CDEF` 5, `CDIL` 46, `CDLG` 39, `CDTX` 7, `CMNM` 10, `CPNL` 5, `CURS` 7, `DITL` 2, `DLOG` 2, `EMSG` 1, `FREF` 12, `ICN#` 11, `LDEF` 2, `MDEF` 1, `MENU` 43, `MPIF` 24, `MUSA` 1, `SICN` 1, `SIZE` 1, `STR#` 38, `TCNV` 1, `TMPL` 9, `WIND` 5, `cfrg` 1, `cicn` 28, `clut` 1, `icl4` 11, `icl8` 11, `ics#` 11, `ics4` 11, `ics8` 11, `mctb` 2, `vers` 2, `wctb` 5. [E].
- **Mac OS X English localization:** `AMIT` 12, `CDCG` 8, `CDIL` 45, `CDLG` 39, `CDTX` 7, `CMNM` 10, `CPNL` 5, `EMSG` 1, `MENU` 43, `MPIF` 24, `STR#` 38, `TCNV` 1, `TMPL` 9, `mctb` 2, `tab#` 2. [X-E].
- **Mac OS X base resources:** `BNDL` 1, `CURS` 7, `FREF` 12, `ICN#` 11, `MUSA` 1, `SICN` 1, `WIND` 5, `cicn` 106, `clut` 4, `icl4` 11, `icl8` 11, `ics#` 11, `ics4` 11, `ics8` 11, `snd ` 5, `wctb` 5. [X-base].

Crucially, Musashi includes its **own format descriptions**: `TMPL` 128–136 describe `CMNM`, `CPNL`, `CDLG`, `CDIL`, `CDCG`, `CDTX`, `EMSG`, `AMIT` and `MPIF`. Those descriptions allowed structured decoding of the custom UI rather than blind extraction of printable strings. All implemented custom decoders consumed their records without trailing bytes. `MENU` and standard dialog parsing follows Apple’s documented formats. [E, `TMPL` 128–136][E]; [decoder][resource-tool]; [Apple MENU format](https://dev.os9.ca/techpubs/mac/Toolbox/Toolbox-183.html), [Apple DITL format](https://dev.os9.ca/techpubs/mac/Toolbox/Toolbox-438.html).

No `Mcmd`, `xmnu`, `CMNU`, `STR `, `ALRT`, `aete` or `aeut` resource occurs in the **74 extracted resource files**. Menu-like evidence instead resides in `AMIT`, `CPNL` and standard `MENU`. The parser includes an `aete` decoder, but there is no dictionary to recover from these packages. That establishes absence in this corpus, **not absence of all Apple-event handling or scripting in the executable**. [Complete resource index][resource-index], [independent map verification][verification].

The Mac OS X plist declares `CFBundleIdentifier = sonosoft`, executable/name `Musashi`, package type `APPL`, signature `MUSA`, and version **3.4**. English `InfoPlist.strings` says **Musashi 3.4-en**. Classic `vers` 1/2 also say **3.4-en** or **3.4-jp**, while 1.2.4 `vers` 1/2 say **J-1.2.4**. Thus archive labels and release notes establish the 3.4.1 distribution identity; internal version metadata was not uniformly updated. [Info.plist][plist-source], [localized strings][info-e], [E, `vers` 1/2][E], [J, `vers` 1/2][J], [124, `vers` 1/2][124].

Static Mach-O inspection identifies the supplied Mac OS X executable’s PowerPC CPU type and dependencies on **Carbon.framework** and `libSystem.B.dylib`. No nib is packaged; UI definitions remain resource-based. This is a historical Carbon application, not evidence for a Swift/Cocoa architecture to adopt today. [Executable inspection][macho], [bundle inventory][manifest-x].

### 2.4. Complete recovered menu inventory

These are **static resource trees**, not a recording of enabled menus during execution. `→` names a submenu; `{dynamic}` identifies an empty or partially populated menu supplied at runtime. Separators are omitted from the tables for readability and preserved in the dumps. Context-dependent `Do`/`View` additions are listed separately; omitting them would omit much of Musashi’s functionality.

#### 1.2.4 menus — Japanese translated

| Menu; resource ID | Commands, in resource order |
|---|---|
| Apple; `MENU` 1 | About Musashi. [124]. |
| File; 128 | Open text file; Open template; Insert text file; Insert template; Save to text file; Save as copy; Close window; Preferences; Page setup; Print; Quit. [124]. |
| Edit; 129 | Undo; Cut; Copy; Paste; Clear; Select all; Paste with quotation. [124]. |
| Mail; 130 | Check new mail; New message; Reply; Reply to all concerned; Forward; Direct forward; Edit; Batch send; Move; Highlight →134; Find. [124]. |
| Mailbox; 131 | New; Rename; Icon →135; Delete; Sharing settings; Cancel sharing; Empty trash. [124]. |
| Options; 132 | Edit address book; Edit signatures; Edit filters; Organize mail files →136; Convert external files →138; Change user settings; Switch user →137. [124]. |
| Windows; 133 | `{dynamic window list}`. [124]. |
| Highlight; 134, 1404 | Normal; Red; Green; Blue; Yellow; Purple; Cyan. [124]. |
| Mailbox icon; 135 | Six icon-bearing entries, with NUL text labels. [124]. |
| Mailboxes; 136, 1302 | All mailboxes; `{dynamic mailboxes}`. [124]. |
| Users; 137 | New; subsequent user population is dynamic. [124]. |
| Conversion; 138 | Eudora mail; Eudora address book. [124]. |
| Attachment encoding; 1021, 1101 | BinHex; Base64 data fork; Base64 AppleDouble; Base64 AppleSingle. [124]. |
| Signatures; 1022, 1102 | None; `{dynamic signatures}`. [124]. |
| Window type; 1103 | Main window; Mail viewer; Mail editor. [124]. |
| Font / size; 1104, 1105 | `{dynamic fonts}`; 9, 10, 11, 12, 14. [124]. |
| Condition subject; 1301, 1401, 1402 | Subject; To; Cc; From; Reply-To; All headers; Body; Whole message. [124]. |
| Other dynamic selectors | Templates 1001; Mailboxes 1011/1403; Groups 1023/1201/1202. [124]. |

These rows account for all **30** `MENU` resources. The toolbar additionally exposes **Copy mail**, **Add sender to address book**, and presentation toggles for large/small icons, Bcc, normal/simple composition, receipt requests and header display (`STR#` 251/252). Those should not be lost merely because they are absent from the principal menu tree. [124, `STR#` 251/252][124].

#### 3.4.1 main menu tree — English resource labels

| Menu; resource ID | Commands and submenus |
|---|---|
| Apple; `MENU` 1 | About Musashi; Enter Registration Code. [E]. |
| File; 301 | Open →128: Text File, Template; Insert →129: Text File, Template; Close; Save; Save As; Find →130: Unread, Read, Unsent, Sent, Find, Find String, Find Next; Page Setup; Print; Quit. Mac OS X resource 301 omits Quit. [E], [X-E]. |
| Edit; 302 | Undo; Cut; Copy; Paste; Clear; Select All; Paste with Quotation. [E]. |
| View; 303 | Without buttons; With buttons; With buttons and messages; context-specific additions below. [E]. |
| Do; 304 | New Message →131 `{accounts}`; Check Mail →132 All Account + `{accounts}`; Remote Mail →133 `{accounts}`; Empty Trash; context-specific additions below. [E]. |
| Options; 305 | Preferences; Edit Address Book; Edit Signatures; Edit Filters; Compress Archive →134 Incoming Messages, Outgoing Messages; Import Messages →135; Export Messages →136; Import Address Book →137; Export Address Book →138; Manager; Change Password; Switch User →139. The import/export/user submenus are dynamically populated. [E]. |
| Windows; 306 | Root Window; `{additional windows}`. [E]. |

The resource tree does not establish where Carbon’s runtime places standard application commands on Mac OS X. In particular, the omitted File/Quit item does not mean the application cannot quit. [E/X-E, `MENU` 301][E], [X-E].

#### 3.4.1 contextual menus — `AMIT`

| Context; resource IDs | Recovered additions |
|---|---|
| Folder viewer, View/Do; 1110/1120 | Large Icon; Small Icon; List; Narrow/Normal/Wide Grid; View Options. New Folder; Open Folder; Rename Folder; Change Folder Icon; Delete Folder. [E]. |
| Message viewer, View/Do; 1210/1220 | A literal `L` placeholder; Without Header; With Some Headers; With All Headers; View Options. Move; Copy; Trash; Reply →140; Forward →141; Edit; Send; Highlight →142; Options →143. The Forward label is literally truncated to `Forwar`; `STR#` 4010 supplies Show List/Hide List labels. [E]. |
| Message editor, View/Do; 1310/1320 | View Options. Send →144; Append Attachment. [E]. |
| Address book editor, View/Do; 1410/1420 | View Options. New Group; New Entry; Rename; Delete. [E]. |
| Signature editor, View/Do; 1510/1520 | View Options. New Signature; Rename Signature; Delete Signature. [E]. |
| Progress window, View/Do; 1610/1620 | Empty View additions; Stop. [E]. |

Referenced submenus are: Reply/Reply to All/Reply to Sender (`MENU` 140); Forward/Direct Forward/Local Forward (141); None and seven highlights (142); Get Info/Add to address book/Apply filters/Remove attachments/Mark read/Mark unread (143); Send Now/Send Later (144). [E, `MENU` 140–144][E].

#### Remaining 3.4.1 selector menus

| `MENU` IDs | Contents |
|---|---|
| 401, 402, 403 | Dynamic language list; All Accounts + incoming accounts; outgoing accounts. [E]. |
| 404, 405, 406 | None + signatures; dynamic output converters; Base64 AppleDouble, AppleSingle, data fork, BinHex, UUEncode. [E]. |
| 501, 505 | Dynamic users and accounts. [E]. |
| 502 | Subject, To, Cc, From, Reply-To, Headers, Body, Whole Message, Account. [E]. |
| 503, 504 | Contain, Not Contain, Begin with, End; Earlier, Later, On time. [E]. |
| 506 | None + seven highlights. [E]. |
| 507 | Eight date layouts: year/month/day, year-month-day, year.month.day, day/month/year, day/month-name/year, month/day/year, month-name/day/year, and explicit year/month/day/hour/minute units. [E]. |
| 508, 509, 510 | Dynamic fonts; sizes 9/10/12/14/18; None + sounds. Mac OS X Japanese also includes size 11. [E], [X-J]. |
| 511, 512, 513 | Additional Reply, Forward and Send selectors duplicating the action choices above. [E]. |

Together the main tree, its submenus and these selectors account for all **43** `MENU` resources. The original Japanese menu texts and Mac OS X variants are fully dumped too; this table uses English as the compact baseline. [E], [J], [X-E], [X-J].

### 2.5. Preferences and account backends

**1.2.4, translated.** Network preferences expose POP server/login/password, SMTP server, save POP password, periodic checks, leave mail on server, JIS/EUC/Shift-JIS output and a Quoted-Printable option, and separate POP/SMTP port fields. Message preferences expose address/name, default signature, retain sent copies, partial/all headers, quote headers, sender-name quotation and a configurable quote string. Window preferences expose fonts, sizes and foreground/background/frame colors. User preferences select single/multiple users and startup password checking. Other options include Reply-To, receipt requests, suppressing the post-receive dialog, MIME-header decoding, inline Japanese input, text-file creator and reply/forward introductory text. [124, `DITL` 211–215][124].

**3.4.1 classic** organizes preferences into five categories; the table lists the complete recovered functional settings rather than generic OK/Cancel controls. [E, `CDCG` 1140][E].

| Category; `CDIL` ID | Settings |
|---|---|
| Accounts; 1141 | New/rename/duplicate/delete accounts; POP account/password; SMTP server; email/name; Reply-To; default signature; output charset; attachment encoding; leave mail and days before deletion; startup check; checking interval; APOP; Return-Receipt-To; outgoing-only; POP-before-SMTP; SMTP authentication. [E]; [EN §6][e6]. |
| Message; 1142 | Reply/forward introductory text; quote prefix; quote headers/sender; eight date layouts; four-digit year; weekday; seconds; timezone conversion; automatic attachment extraction and directory; text-file creator selection. [E]; [EN §6][e6]. |
| Color; 1143 | Seven message highlight colors and names; colors for first, second and deeper quotation levels; add/delete recognized quotation strings; URL highlight. [E]; [JP §6, translated][j6]. |
| Detail; 1144 | Default incoming/outgoing accounts; send queued messages while checking; word-wrap column count; inline input; use standard Mac OS controls; close editor after send; post-receive dialog; warning-dialog timer; printing language/font/size and no/some/all headers; arrival sound. [E, `CDCG` 1144][E]; [EN §6][e6]. |
| Module; 1145 | Select module; use its window, message-highlight and quotation-highlight colors. [E]; [EN §6][e6]. |

View-specific preferences independently choose fonts and foreground/background colors for folder, message, composition and address-book windows. Fonts can vary by language; the scope is all windows of the relevant kind, not merely the frontmost instance. Virtual message viewers have their own color scope and share relevant text-font settings. [E, `CDIL` 1170–1200 and `STR#` 4020 items 22–24][E]; [JP §14, translated][j14].

The **Mac OS X** preference resource `tab#` 1140 lists **Account, Message, Color, Detail**. `CDIL` 1145 and classic `CDCG` 1140 are absent. The bundle embeds 106 color icons and its sound resources instead of shipping a separate Modules folder. Therefore the manual’s five-category/module-selection account should not be presented as a verified Mac OS X UI feature. This is a static packaging/UI difference; runtime acceptance of external modules is **unverified**. [X-E, `tab#` 1140][X-E], [X-base], [manifest-x].

POP configuration combines user ID and host as `user@pop.host`; a quoted user ID permits embedded `:` or `@`. An appended `:port` overrides the port. The Japanese manual gives defaults **POP 110, SMTP 25** and explains that deletion-after-N-days is measured from first retrieval and performed on a later server access. An empty POP password prompts on each login. [JP §6, translated][j6]; [E, `STR#` 1015 and `CDIL` 1141][E].

The manuals document APOP and POP-before-SMTP, and initially describe SMTP authentication as CRAM-MD5 only. **Japanese release notes, translated:** 3.3 introduced POP-before-SMTP and CRAM-MD5 on **2001-12-20**; 3.3.1 added LOGIN and PLAIN on **2001-12-22**. 3.4’s **2002-03-15** classic notes report fixing authentication for outgoing-only accounts. These are dated vendor historical statements. Mechanism strings occur in English, Japanese and Mac OS X executables, corroborating their inclusion without proving interoperability. [Release notes][release-j], [EN §6][e6], [string offsets][protocol-strings].

No TLS/SSL or IMAP setting was recovered in the account resources or manuals; a bounded exact-string check found neither `STARTTLS` nor `IMAP` in the three 3.4.1 executables. Macintosh Garden independently describes the client as lacking SSL. **Conclusion:** POP/SMTP and the documented authentication methods are supported by the recovered evidence; modern encrypted/authenticated provider compatibility is not established. [E/J, `CDIL` 1141][E], [J]; [search record][protocol-strings]; [Garden, checked 2026-10-11](http://macintoshgarden.org/apps/musashi).

One unusually consequential option is the **warning-dialog timer**: the manual says a confirmation dialog automatically answers **Yes** after the interval. This behavior is documented, not inferred from the short resource label. [EN §6][e6]; [JP §6, translated][j6]; [E, `CDIL` 1144 item 17][E].

### 2.6. Users, mailbox model and reading workflow

**1.2.4, translated:** local usernames/passwords are separate from POP credentials. Each local user has private mailboxes and an individual POP server; single-user mode can optionally require a startup password. Initial mailboxes are Incoming, Created/Sent copies, Trash and, in multi-user mode, Shared mail. Mailboxes can be freely positioned as icons. Trash retains mail until explicitly emptied. [Usage][usage124]; [124, `DITL` 201/214 and `STR#` 301][124].

Its mailbox-sharing dialog selects users and separately permits **adding mail into** a shared mailbox and **moving mail out**. Recipients cannot rename or delete the owner’s shared mailbox; sharing ends when its owner revokes access or deletes it. This is application-level sharing on one installation, not evidence of a network collaborative server or cryptographic isolation. [1.2.4 usage, translated][usage124]; [124, `DITL` 220 and `MENU` 131][124].

**3.4.1** adds a management password, user creation/renaming/deletion/password changes, selectable user home directories, and single/multi-user mode. Management can reset a user’s password without knowing the old one. Changing a home directory **does not move configuration files automatically**. Users can switch without quitting. [EN §§4–5][e4], [e5]; [E, `CDIL` 1020–1080][E].

The 3.4.1 manuals instead document **Local Forward**: selected mail is delivered to other local users through local files, without a server connection. The receiving user is notified on startup and sees it in a virtual viewer. Shared-mailbox controls from 1.2.4 do not appear in the 3.4.1 main UI resources; the migration plug-in still contains a legacy Public/Shared mailbox label. Thus shared-mailbox support in 3.4.1 is **unverified**, while local forwarding is explicitly documented. [JP §14, translated][j14]; [E, `MENU` 141, `CDIL` 1250, `STR#` 1014][E]; [PFC, `STR#` 129][pfc-e].

**JP §7, translated:** 3.4.1 permits **three folder levels**. A folder contains either subfolders or messages, never both. Inbox, Outbox and Trash remain at the root, cannot be deleted/moved into another folder, and cannot gain subfolders. Deleting a normal folder deletes its subfolders and moves contained messages to Trash. An empty folder asks whether to open a folder viewer or message viewer. [Japanese chapter][j7]; [E, `STR#` 1017 and `CDIL` 1330][E].

Folders offer large icons, small icons or a list, manual positioning, three grid spacings, icon changes, drag-and-drop nesting, and name sorting toggled with Option-click. List mode can display lower-level folders in one viewer. This makes folder organization a spatial workspace as well as a hierarchy. The last sentence is an interpretation of the documented controls. [JP §7, translated][j7]; [E, `AMIT` 1110/1120][E].

Mail viewers combine a list and message pane, multi-selection, move/copy/trash, sender/date/subject sorting and ascending/descending order. Selecting multiple messages clears the content pane. Header display can be none/some/all, and the list can be hidden. Message information permits subject and status metadata changes, including opened/replied/forwarded/sent. [JP §7, translated][j7]; [E, `AMIT` 1210/1220, `CDIL` 1230][E].

**Reading gestures, translated:** Space scrolls forward and advances to the next message at the end; Delete scrolls in reverse. In 3.4.1, holding Control with a digit 1–9 slowly scrolls at nine speeds; adding Shift reverses direction. Arrow keys change the selected mail; 1.2.4 additionally documents Shift/Option selection/navigation combinations and a focus marker. Its message pane pages text above **24 KiB**. That historical display limit is not documented for 3.4.1. [JP §7][j7]; [1.2.4 usage][usage124].

A **virtual message viewer** presents newly received mail, status searches, detailed search results and imported mail, and supports ordinary actions on selected messages. Searching while a result viewer is frontmost refines those results. The Japanese manual explicitly explains this narrowing operation; the English version omits it. [EN §§10,14][e10], [e14]; [JP §14, translated][j14]; [E, `STR#` 4020/4030][E].

**JP §10, translated:** retrieval has a process-status window. Starting a check for another account while one is running queues it after the current retrieval, rather than proving parallel account downloads. Newly received messages appear together with their containing folder names; no result viewer opens if there is no new mail. [Japanese reception chapter][j10]; [E, `AMIT` 1620][E].

### 2.7. Rules, search, composition and attachments

**1.2.4, translated:** a filter can be enabled/disabled, choose one condition or two joined by AND/OR, optionally distinguish case for each condition, save matching mail in a selected mailbox and apply a highlight. Both filter conditions and search expose Subject/To/Cc/From/Reply-To/headers/body/whole-message fields. This is recovered UI evidence; the older usage document does not specify the complete filter priority algorithm. [124, `DITL` 500/400, `MENU` 1301/1401/1402][124]; [usage][usage124].

**3.4.1, translated:** named rules can be created, renamed, duplicated, deleted and reordered by drag. They have up to two conditions joined by AND/OR, independent case options, account matching, contain/not-contain/begins/ends predicates, destination-folder and highlight actions, and incoming/outgoing applicability. Automatic filtering stops at the first matching rule. Manual application lets the user select multiple rules for selected stored messages. [JP §12][j12]; [E, `CDIL` 1150, `CDCG` 1150, `MENU` 502/503/505/506][E].

Search adds status shortcuts, string/Find Next, two condition regions, date comparison controls, highlight selection controls, and scope choices for all messages, the current viewer and a selected folder. **Call filter** imports a rule into the search dialog. The static nine-item subject menu does not itself list date/highlight; the corresponding controls prove their presence in the search layout, while any dynamically added subject labels remain unobserved. Results can be searched again to narrow them. [E, `MENU` 130/502–506, `CDIL` 1120/1130, `CDCG` 1120/1121][E]; [JP §14, translated][j14].

Both versions support reply, reply-all, forwarding, signatures, text insertion, reusable templates and quoted paste. 1.2.4 documents nested address aliases, group-wide recipients, drag/drop text and desktop export; 3.4.1 adds explicit reply-to-sender, local forwarding, Send Now/Send Later, per-message account/output/attachment-encoding choices, inline attachment icons and an Option-click caret-offset/character counter. Long-pressing a compose/check button for about **half a second** exposes account choices; pressing Send similarly offers immediate/deferred sending. These gestures are documented, not measured. [1.2.4 usage, translated][usage124]; [EN §§10–11][e10], [e11]; [E, `MENU` 140/141/144, `AMIT` 1320][E].

**JP §11, translated:** Direct Forward retains original sender/date information in headers. Inline attachments can be repositioned by drag and deleted like a character; an attachment at the body’s end follows the appended signature. The caret/character counter remains visible only while the mouse is held. [Japanese composition chapter][j11].

Drafts are explicitly saved to Outbox; its Japanese description includes unfinished and sent messages. 1.2.4 can batch-send selected composed or previously sent messages; its error resources distinguish received mail from locally composed mail when enabling reply/resend operations. None of this establishes automatic draft recovery. [JP §7, translated][j7]; [EN §11][e11]; [1.2.4 usage][usage124]; [124, `STR#` 206][124].

3.4.1’s address book contains nested groups, named entries, addresses and comments; sender addresses can be captured from mail. Composition accepts colon-separated group paths and expands group/entry names. Multiple named signatures have account defaults and per-message selection; address/signature editors have explicit Save commands. [EN §§8,9,11][e8], [e9], [e11]; [E, `AMIT` 1420/1520 and `STR#` 4070][E].

Templates are plain text: `To`, `Cc`, `Bcc`, `Subject`, `Account` and `Signature` headers, an empty line, then the body; headers may be omitted. **JP release notes, translated**, additionally record recognition of template `Reply-To` in **3.2.1, 2000-05-19**. Thus the six-header appendix is incomplete for the later distribution. Reply/forward introductory text has `%N` sender, `%S` subject, `%D` date and a newline escape; the Japanese display uses the yen-shaped representation of the legacy backslash byte. [Template appendix][eAA]; [release history][release-j]; [EN §6][e6], [JP §6][j6].

Attachment encoding choices are BinHex, Base64 data-only, AppleDouble and AppleSingle in 1.2.4, plus UUEncode in 3.4.1. The 1.2.4 read-me explicitly says some MIME types/subtypes are unsupported. 3.4.1 lets attachments appear at the caret position as body icons, be added by drop, opened by double-click, automatically extracted to a configured directory, or removed from received mail with an irreversible-removal warning. [124, `MENU` 1021 and read-me, translated][124], [readme124]; [E, `MENU` 406/143 and `STR#` 1018 item 13][E]; [EN §§6,11][e6], [e11].

**JP §14, translated:** messages can be saved as text from their viewer or dragged into Finder, using the configured text-file creator; selected mail can be printed. Import/export plug-ins select destination folders, message scopes or address groups, with behavior depending on the plug-in. Per-language display-font choices also depend on installed plug-ins. [Japanese additional-features chapter][j14].

**JP §7, translated:** HTML mail is treated as an attachment; opening it launches the WWW browser. The recovered design therefore offers plain-text reading with external HTML viewing, rather than proving an embedded HTML mail renderer. The same chapter explains clickable underlined URLs with Command-click, delegated to the configured Internet application. [Japanese chapter][j7]; [E, `CDIL` 1143][E].

### 2.8. Encodings, import/export, themes and remote mail

The following are the shipped `MPIF` **declarations**, not claims that every importer/converter was exercised successfully:

| Declaration group | Recovered formats; resource IDs |
|---|---|
| Message output | Raw US-ASCII, Quoted-Printable raw, Base64 raw (201–203); Japanese ISO-2022-JP/Shift-JIS/EUC-JP (211–213); ISO-8859-1/-2 (221–222); ISO-8859-5, KOI8-R, Windows-1251 (231–233); ISO-8859-7 Greek (241); ISO-8859-9 Turkish (251); Chinese Big5, simplified Chinese EUC-CN, Korean EUC-KR (301/311/321, Japanese labels translated). [E], [J]. |
| English message import | Text; Eudora TOC; Eudora Archive; Musashi Archive (1101–1104). [E]. |
| English message export | Eudora Archive (1201). [E]. |
| Japanese message import | Musashi 2.x; Musashi 3.x; restore DUMP; Eudora TOC; Eudora mail, explicitly labelled mbox; text (1101–1106). [J, translated][J]. |
| Japanese message export | DUMP backup; Eudora mail; mbox (1201–1203). [J, translated][J]. |
| Address interchange | Eudora nicknames; Eudora nickname export; UNIX `/etc/alias` export, with resource spelling retained (1301, 1401–1402). The release notes refer to `/etc/aliases`. [E], [J], [release-j]. |

English and Japanese packages differ materially in their import/export declarations; the Japanese set must not be silently attributed to the English UI. Mac OS X English/Japanese retain their respective declaration counts. 1.2.4’s Eudora import selects a `.toc` or nickname file, but its read-me expressly excludes **Eudora 3.0 mail format**. [X-E/X-J, `MPIF`][X-E], [X-J]; [1.2.4 usage/read-me, translated][usage124], [readme124].

The shipped `PFC`/`PFC-jp` plug-in is specifically associated with **preference conversion**, as shown by its `DITL` 128/129 progress labels and `STR#` 128 errors. The 3.4.1 manual describes startup conversion of 2.x preferences; the Mac OS X compatibility note separately instructs users to copy older configuration into `~/Library/Preferences/Musashi Preferences Folder`. These are migration instructions, not proof of a lossless importer or automatic migration across operating systems. [PFC resources][pfc-e], [pfc-j]; [JP §14, translated][j14]; [Mac OS X compatibility note][compatibility].

**Classic modules** are external files with creator `MUSA`, type `MODL`. The appendix specifies `cicn` button icons at 24×24, folder icons at 32×32, and status/editor icons at 12×12. Empty/full folder variants occupy ranges 21001–21064 and 21101–21164, with slot 64 reserved for Trash; buttons occupy 20001–20053, and basic message states 21201–21204. Replied/forwarded/pending/attachment icons occupy 21296–21299; address/editor roles 22001–22007; remote received/to-retrieve/to-delete states 23001–23003. Status icons distinguish unread/read, unsent/sent, replied, forwarded, pending and attachments. Three `clut` resources, 20001–20003, define 32 window colors, seven message highlights and three quotation-depth colors. The data fork supplies a PICT preview, displayed within 200×150. [Appendix B][eAB]; [Default Module resources][module-e].

This is more than decorative recoloring: it exposes **semantic visual roles** such as empty/full mailbox, quoted depth and pending message. “Semantic” is the design interpretation; the actual roles and numeric resource ranges are documented. Named arrival sounds include Koto, Bell and Guitar; the read-me credits their creator and credits the default icon artist. [Appendix B][eAB]; [classic README][readme-e]; [sound resource file][sounds-e].

**Remote Mail** previews POP-server messages’ subject, sender, date and size, then lets the user mark/unmark each for retrieval or deletion and explicitly Execute. A message may be marked for both retrieval and deletion. Retrieved messages appear in a virtual viewer afterward. This is selective POP operation, not an IMAP synchronization model. [EN §13][e13], [JP §13, translated][j13]; [E, `CDIL` 1310 and `MENU` 133][E].

### 2.9. Document types, release history and evidence quality

| Version | Creator/signature and `FREF` document declarations |
|---|---|
| 1.2.4 | `MUSA`; `APPL` 128, `GPRF` 129, `UPRF` 130, `ADBK` 131, `SIGN` 132, `FILT` 133, `MIDX` 134, `MARC` 135, `ICNF` 136, `TEXT` 137. [124, `BNDL` 128 / `FREF`][124]. |
| Classic 3.4.1 | `MUSA`; duplicate `APPL` 128/129, `GPRF` 130, `UPRF` 131, `ADDR` 132, `SIGN` 133, `FLTR` 134, `INDX` 135, `UIDL` 136, `TEXT` 137, `MODL` 138, `MPLG` 139. [E, `BNDL` / `FREF`][E]. |
| Mac OS X 3.4.1 | Base resource file retains the twelve later `FREF` declarations and `MUSA` bundle signature. `Info.plist` has no document-type array. [X-base, `BNDL` / `FREF`][X-base]; [plist][plist-source]. |

The preference/address/signature/filter/index labels in `STR#` 1112 help interpret the later four-character types, but ownership declarations alone do **not** describe their on-disk serialization or prove recoverability. No actual user store was shipped. The `UIDL` declaration is evidence of a file type; it is insufficient to reconstruct the duplicate-detection algorithm. [E, `STR#` 1112][E]; [inventory][inventory].

The shipped release notes date **3.4.1 to 2002-06-14** and report fixes to Mac OS X printing, attachment filename extensions, 2.x message import and UNIX address export; the Japanese notes also mention garbled print titles. The older 1.2.4 notes record quoted-printable support, notification, word wrapping, bulk sending, Eudora import and earlier introduction of filters/search/background transfers. Their exact 1.2.4 release day is **unverified**. [3.4.1 English/Mac OS X notes][release-e], [release-x]; [Japanese notes, translated][release-j]; [1.2.4 release notes, translated][release124].

Historical terms changed: 1.2.4’s 1997 document specifies a **30-day trial and ¥3,000**; the 3.4.1 English distribution specifies **40 days, $25 registration, $9 upgrade**, while Japanese documentation specifies **¥2,800 and ¥1,000 upgrade**. These are historical package terms, **not current prices or an available purchase recommendation**. [1.2.4 read-me, translated][readme124]; [classic English README][readme-e]; [Japanese/Mac OS X introductory text, translated][readme-j].

The sources contain errors and stale material. English overview §3 swaps POP/SMTP sending/receiving; Japanese §3 and account settings correctly identify POP3 receive/SMTP send. “Important things to read” says no English manual exists although one is packaged. Mac OS X metadata still says 3.4, and its English third compose-tab title repeats “Destination” where Japanese says “Other.” Release history, Japanese text and structured resource relationships resolve some inconsistencies; they cannot establish successful execution of every command. [EN §3][e3], [JP §3, translated][j3]; [stale notice][important-e]; [X-E/X-J, `tab#` 1030][X-E], [X-J]; [plist][plist-source].

The Japanese history also documents serious **past** defects: a crash during retrieval could lose previously retrieved mail on next startup (fixed in 3.0.1, **1999-01-21**); cancelling user switching could make the original user’s mail disappear (3.0.5, **1999-02-11**); folder deletion could delete messages instead of moving them to Trash (3.2.1, **2000-05-19**); multiline Bcc could expose recipients (3.2.4, **2000-07-24**). These are vendor-reported historical bugs, not allegations that 3.4.1 still exhibits them. [Japanese release notes, translated][release-j].

## 3. Options compared

| Capability | 1.2.4 evidence | 3.4.1 evidence | Sweetbriar implication |
|---|---|---|---|
| Local identities | Local passwords and private mailboxes | Manager/password/home-directory controls | Keep account/workspace separation; decide separately whether application-level users are needed. [usage124], [e4]. |
| Sharing | Mailboxes shared with add/move permissions | Local forwarding documented; equivalent sharing unverified | Model references/permissions explicitly if shared local collections are adopted. [124, `DITL` 220][124]; [j14]. |
| Organization | Freely positioned mailbox icons | Three-level hierarchy; icon/list/grid modes; exclusive folder contents | Offer an optional visual overview while allowing folders to contain both messages and subfolders. [usage124], [j7]. |
| Rules/search | Two-condition move/highlight UI | Incoming/outgoing rules, first-match ordering, rule-to-search loading, result refinement | Use one typed predicate model, with explainable execution. [124, `DITL` 400/500][124]; [E, `CDIL` 1120/1150][E]; [j12], [j14]. |
| Composition | Templates, signatures, batch send | Account-aware templates, queue choices, more reply/forward actions | Keep durable drafts, explicit identity and an observable outbox. [usage124], [e11], [eAA]. |
| International text | Japanese output and limited MIME support | Sixteen converter declarations; per-language fonts | Preserve legacy bytes and expose decoding correction alongside Unicode display. [readme124]; [J, `MPIF`][J]; [j14]. |
| Presentation | Window colors, icons | Classic modules; Mac OS X embeds resources | Preserve semantic customization through modern native controls. [124, `DITL` 213][124]; [eAB]; [X-E, `tab#` 1140][X-E]. |
| Remote control | Later remote-mail work listed as planned | Selective POP preview/retrieve/delete | Translate to capability-specific sync and download controls. [release124], [e13]. |

## 4. Recommendations for Sweetbriar

These are **design recommendations**, derived from the evidence above rather than descriptions of existing Sweetbriar code.

1. **Build one message collection/viewer abstraction.** A folder, search, newly arrived set and import preview should return stable message IDs and support the same selection/actions. Let users refine results or turn them into saved collections. In Rust, separate `MessageId`, `CollectionQuery`, result membership and actions; expose snapshots/change events to Swift. Use native list/split-view controls and preserve fast Space-key reading as an optional mode. Musashi’s virtual viewer and result refinement supply the historical precedent. **Strongest counter-argument:** mutable search results can make “act on everything shown” ambiguous. Freeze the selected IDs when an action begins and report its scope. [e10], [j14], [j7].

2. **Make search and rules share a typed condition model.** Support `Contains`, `StartsWith`, account/date/status predicates and boolean composition, with “Search using this rule” and “Create rule from this search.” Go beyond Musashi’s two-condition ceiling, while keeping the ordinary editor simple. Preview matches and show why each rule applied. **Strongest counter-argument:** a useful search can be unsafe or expensive as an automatic rule. Keep pure predicates separate from actions and require an explicit application scope; offer first-match versus continued evaluation as a visible policy. Musashi’s Call filter and first-match order motivate this design. [E, `CDIL` 1120/1150][E]; [j12].

3. **Preserve personal organization without imposing historical constraints.** Offer named color tags, custom mailbox symbols, pinned collections and an optional spatial folder overview. Retain quotation-depth highlighting and clear account/status cues. Keep ordinary hierarchical navigation primary, with folders allowed to hold both messages and children. **Strongest counter-argument:** arbitrary positioning is hard to maintain across window sizes and Linux UI conventions. Store optional layout separately from mail semantics and let each native frontend present it appropriately. Musashi’s icon workspace and semantic module roles are the precedent, not its resource formats. [j7], [eAB].

4. **Treat templates as portable, account-aware documents.** Preserve a human-readable representation containing recipients, subject, identity, signature and body; make opening versus inserting explicit. Add preview and versioning so a template cannot silently change the sender account or recipients. Keep nested address groups but validate expansion for cycles. **Strongest counter-argument:** account-bearing templates are hazardous when copied across machines or reused after accounts change. Resolve accounts by stable IDs, display missing bindings, and require a visible identity decision. [eAA], [e11], [release-j].

5. **Prioritize local durability before advanced sharing.** Preserve original RFC-style message bytes; save edited subjects as separate local annotations, and removing an attachment as a derived view/version rather than destruction of the only copy. Keep versioned drafts/settings, a transactional queue and explicit pending/sending/sent/failed/uncertain states. A proposed implementation is immutable message blobs plus SQLite metadata transactions and a rebuildable search index; coordinate blob persistence before committing metadata references. SQLite documents both atomic transactions and the filesystem/flush assumptions underlying durability, checked **2026-10-11**. **Strongest counter-argument:** SQLite alone cannot protect external blobs, account migrations or every hardware failure. Prove recovery with fault injection and maintain restorable backups; do not promise safety solely from choosing a database. The Japanese bug history supplies concrete failure cases to test. [release-j]; [SQLite atomic commit](https://www.sqlite.org/atomiccommit.html), [synchronous settings](https://www.sqlite.org/pragma.html#pragma_synchronous).

6. **Revive selective server control through backend capabilities.** Offer “metadata only,” “download selected bodies/attachments,” per-account retention and an explicit plan for server deletions. Implement capability discovery in the Rust backend layer so POP, JMAP and API-specific accounts can expose appropriate operations without pretending their models are identical. **Strongest counter-argument:** selective download produces incomplete search and misleading offline availability. Show the coverage of local search and whether an action requires network access. Musashi’s Remote Mail is the inspiration, not a protocol implementation to copy. [e13].

7. **Defer a full local-user subsystem unless demand justifies it.** First separate personal/work account workspaces and provide export/import or explicit local copies. If application-level users are added later, define permissions, ownership, transfer audit records and actual storage protection; retain convenient local forwarding as a controlled copy operation. **Strongest counter-argument:** OS user accounts already offer a clearer boundary, while a second password system adds recovery and security obligations. The older client demonstrates both the usefulness and complexity of local users/sharing. [usage124], [e4], [j14].

The strongest overall argument against reviving Musashi literally is its separation into many specialized windows and customization modes: a fast new client can become complicated before it becomes dependable. My recommendation is to adopt its **consistent actions, expressive local organization and direct control**, with a compact native default UI and advanced options introduced only where they earn their complexity.

## 5. Risks and open questions

- **Static evidence has a clear boundary.** All archive members and resource entries are recovered; runtime enablement, generated menus, protocol interoperability, performance and crash behavior are untested. Menu key/modifier bytes are preserved, but a complete runtime keyboard map is unverified. [Resource dumps][resource-index].
- **No user data means no storage-format proof.** `FREF`, migration labels and importer declarations do not establish the serialization, atomicity or recovery properties of real mail/draft/settings stores. A separately supplied, non-sensitive user store would be needed for that next investigation. [124/E, `FREF`][124], [E]; [inventory][inventory].
- **Negative findings are bounded.** No packaged AppleScript dictionary, Apple Guide, nib or modern-provider backend was recovered; broader absence cannot be proven from those inventories alone. 3.5 and intervening archives were not supplied or examined. [resource-index], [manifest-x].
- **Localization changes functionality and meaning.** Japanese import/export declarations are richer; manuals have omissions and contradictions. Some English resources contain Japanese-encoded labels, and English release-note punctuation mixes encodings; raw bytes and the corresponding Japanese sources remain authoritative for those fragments. [E/J, `MPIF`, `STR#` 1020/1214][E], [J]; [release-e], [release-j].
- **A declared feature is not a modern safety contract.** Subject edits, attachment removal, home-directory changes, automatic confirmation and local password resets need redesigned ownership and recovery semantics. In particular, Sweetbriar should not adopt timed automatic **Yes** confirmations for destructive operations. [e4], [e6]; [E, `CDIL` 1230, `STR#` 1018][E].
- **Protocol security remains unverified here.** Do not infer current account compatibility from APOP, CRAM-MD5, LOGIN or PLAIN names. The report does not test today’s providers, encryption negotiation or credential storage. [e6], [release-j], [protocol-strings].
- **Recovery should be an acceptance criterion.** The documented historical failures suggest focused future tests: cancel user switching, crash at each receive/save step, move/delete nested folders, save a draft shown in multiple views, migrate settings, expand recipient groups, and serialize multiline Bcc/MIME headers. The proposed tests follow the historical cases; no new client tests were written in this extraction assignment. [release-j].

## 6. Sources actually consulted

All live web consultation occurred on **2026-10-11**. Distribution documents retain their historical dates; “checked today” does not turn a historical claim into a present-day product guarantee.

Primary artifact evidence:

- [Archive manifests and extraction totals][extraction]; [full file inventory][inventory]; [resource-file index][resource-index]; [independent resource comparison][verification]; [independent extraction audit][audit]; [UTF-8 document index][documents].
- [1.2.4 application resources][124], [usage][usage124], [read-me][readme124], [release notes][release124].
- [3.4.1 English resources][E], [Japanese resources][J], [English manual contents][e0], [Japanese manual contents][j0], [Japanese release history][release-j].
- [Mac OS X base resources][X-base], [English resources][X-E], [Japanese resources][X-J], [Info.plist][plist-source], [release notes][release-x], [compatibility note][compatibility]. The other four localizations are indexed in [resource-index].

Consulted web/format sources:

- [Macintosh Garden Musashi page](http://macintoshgarden.org/apps/musashi). HTTP retrieval succeeded; HTTPS and the attempted Wayback retrieval failed. [Local fetched page][garden-snapshot].
- XADMaster: [classic StuffIt parser](https://raw.githubusercontent.com/MacPaw/XADMaster/master/XADStuffItParser.m), [StuffIt 5 parser](https://raw.githubusercontent.com/MacPaw/XADMaster/master/XADStuffIt5Parser.m), [method 13](https://raw.githubusercontent.com/MacPaw/XADMaster/master/XADStuffIt13Handle.m), [Arsenic](https://raw.githubusercontent.com/MacPaw/XADMaster/master/XADStuffItArsenicHandle.m), [prefix codes](https://raw.githubusercontent.com/MacPaw/XADMaster/master/XADPrefixCode.m), [BWT](https://raw.githubusercontent.com/MacPaw/XADMaster/master/BWT.c), [license](https://raw.githubusercontent.com/MacPaw/XADMaster/master/LICENSE). Supporting headers/source snapshots and the exact fetched-URL list are retained in [source ledger][web-ledger].
- Apple documentation, mirrored: [MENU](https://dev.os9.ca/techpubs/mac/Toolbox/Toolbox-183.html), [DITL](https://dev.os9.ca/techpubs/mac/Toolbox/Toolbox-438.html).
- [Apple MacJapanese mapping](https://www.unicode.org/Public/MAPPINGS/VENDORS/APPLE/JAPANESE.TXT).
- [CPython 2.7.18 aete layout](https://raw.githubusercontent.com/python/cpython/v2.7.18/Lib/plat-mac/gensuitemodule.py).
- [rsrcfork](https://pypi.org/project/rsrcfork/), [striprtf](https://pypi.org/project/striprtf/).
- [SQLite atomic commit](https://www.sqlite.org/atomiccommit.html), [SQLite synchronous settings](https://www.sqlite.org/pragma.html#pragma_synchronous), consulted for the proposed durability design.

[extraction]: <codex-artifacts/evidence/extraction-summary.json>
[verification]: <codex-artifacts/evidence/verification.json>
[audit]: <codex-artifacts/evidence/extraction-audit.md>
[tools]: <codex-artifacts/tools/README.md>
[stuffit]: <codex-artifacts/tools/stuffit.py>
[method13]: <codex-artifacts/tools/method13.py>
[arsenic]: <codex-artifacts/tools/arsenic.py>
[resource-tool]: <codex-artifacts/tools/resources.py>
[doc-tool]: <codex-artifacts/tools/documents.py>
[documents]: <codex-artifacts/extracted-docs/index.json>
[manual-comparison]: <codex-artifacts/evidence/manual-comparison.json>
[inventory]: <codex-artifacts/evidence/inventory.json>
[resource-index]: <codex-artifacts/evidence/resource-index.json>
[protocol-strings]: <codex-artifacts/evidence/protocol-string-locations.json>
[macho]: <codex-artifacts/evidence/macho.json>
[plist]: <codex-artifacts/evidence/bundle/info.json>
[plist-source]: <codex-artifacts/extracted/osx341/Musashi-3.4.1/Musashi.app/Contents/Info.plist.data>
[info-e]: <codex-artifacts/extracted-docs/osx341/Musashi-3.4.1/Musashi.app/Contents/Resources/English.lproj/InfoPlist.strings.txt>
[rea]: <codex-artifacts/evidence/rea-plist.json>
[web-ledger]: <codex-artifacts/sources/consulted.json>
[garden-snapshot]: <codex-artifacts/sources/garden-1.html>
[manifest124]: <codex-artifacts/extracted/jp124/manifest.json>
[manifest-e]: <codex-artifacts/extracted/en341/manifest.json>
[manifest-j]: <codex-artifacts/extracted/jp341/manifest.json>
[manifest-x]: <codex-artifacts/extracted/osx341/manifest.json>
[124]: <codex-artifacts/evidence/resources/jp124/resources.json>
[E]: <codex-artifacts/evidence/resources/en341-app/resources.json>
[J]: <codex-artifacts/evidence/resources/jp341-app/resources.json>
[X-E]: <codex-artifacts/evidence/resources/osx341-english/resources.json>
[X-J]: <codex-artifacts/evidence/resources/osx341-japanese/resources.json>
[X-base]: <codex-artifacts/evidence/resources/osx341-base/resources.json>
[module-e]: <codex-artifacts/evidence/resources/en341-module/resources.json>
[pfc-e]: <codex-artifacts/evidence/resources/en341-5690d3b0bfb1/resources.json>
[pfc-j]: <codex-artifacts/evidence/resources/jp341-e3d7aea59708/resources.json>
[sounds-e]: <codex-artifacts/evidence/resources/en341-7557eb5990c2/resources.json>
[usage124]: <codex-artifacts/extracted-docs/jp124/MUSASHI-J(PPC) Folder/使い方について.txt>
[readme124]: <codex-artifacts/extracted-docs/jp124/MUSASHI-J(PPC) Folder/必ずお読みください.txt>
[release124]: <codex-artifacts/extracted-docs/jp124/MUSASHI-J(PPC) Folder/リリースノート.txt>
[release-e]: <codex-artifacts/extracted-docs/en341/Musashi-3.4.1-en/What's new ?.txt>
[release-j]: <codex-artifacts/extracted-docs/jp341/Musashi-3.4.1-jp/リリースノート.txt>
[release-x]: <codex-artifacts/extracted-docs/osx341/Musashi-3.4.1/ReleaseNotes.rtf.txt>
[compatibility]: <codex-artifacts/extracted-docs/osx341/Musashi-3.4.1/Compatibility.rtf.txt>
[readme-e]: <codex-artifacts/extracted-docs/en341/Musashi-3.4.1-en/README.txt>
[readme-j]: <codex-artifacts/extracted-docs/osx341/Musashi-3.4.1/_jp-お読みください.rtf.txt>
[important-e]: <codex-artifacts/extracted-docs/en341/Musashi-3.4.1-en/Important things to read.txt>
[e0]: <codex-artifacts/extracted-docs/en341/Musashi-3.4.1-en/HTML/00.html.txt>
[e1]: <codex-artifacts/extracted-docs/en341/Musashi-3.4.1-en/HTML/01.html.txt>
[e2]: <codex-artifacts/extracted-docs/en341/Musashi-3.4.1-en/HTML/02.html.txt>
[e3]: <codex-artifacts/extracted-docs/en341/Musashi-3.4.1-en/HTML/03.html.txt>
[e4]: <codex-artifacts/extracted-docs/en341/Musashi-3.4.1-en/HTML/04.html.txt>
[e5]: <codex-artifacts/extracted-docs/en341/Musashi-3.4.1-en/HTML/05.html.txt>
[e6]: <codex-artifacts/extracted-docs/en341/Musashi-3.4.1-en/HTML/06.html.txt>
[e7]: <codex-artifacts/extracted-docs/en341/Musashi-3.4.1-en/HTML/07.html.txt>
[e8]: <codex-artifacts/extracted-docs/en341/Musashi-3.4.1-en/HTML/08.html.txt>
[e9]: <codex-artifacts/extracted-docs/en341/Musashi-3.4.1-en/HTML/09.html.txt>
[e10]: <codex-artifacts/extracted-docs/en341/Musashi-3.4.1-en/HTML/10.html.txt>
[e11]: <codex-artifacts/extracted-docs/en341/Musashi-3.4.1-en/HTML/11.html.txt>
[e12]: <codex-artifacts/extracted-docs/en341/Musashi-3.4.1-en/HTML/12.html.txt>
[e13]: <codex-artifacts/extracted-docs/en341/Musashi-3.4.1-en/HTML/13.html.txt>
[e14]: <codex-artifacts/extracted-docs/en341/Musashi-3.4.1-en/HTML/14.html.txt>
[eAA]: <codex-artifacts/extracted-docs/en341/Musashi-3.4.1-en/HTML/AA.html.txt>
[eAB]: <codex-artifacts/extracted-docs/en341/Musashi-3.4.1-en/HTML/AB.html.txt>
[j0]: <codex-artifacts/extracted-docs/jp341/Musashi-3.4.1-jp/HTML/00.html.txt>
[j1]: <codex-artifacts/extracted-docs/jp341/Musashi-3.4.1-jp/HTML/01.html.txt>
[j2]: <codex-artifacts/extracted-docs/jp341/Musashi-3.4.1-jp/HTML/02.html.txt>
[j3]: <codex-artifacts/extracted-docs/jp341/Musashi-3.4.1-jp/HTML/03.html.txt>
[j4]: <codex-artifacts/extracted-docs/jp341/Musashi-3.4.1-jp/HTML/04.html.txt>
[j5]: <codex-artifacts/extracted-docs/jp341/Musashi-3.4.1-jp/HTML/05.html.txt>
[j6]: <codex-artifacts/extracted-docs/jp341/Musashi-3.4.1-jp/HTML/06.html.txt>
[j7]: <codex-artifacts/extracted-docs/jp341/Musashi-3.4.1-jp/HTML/07.html.txt>
[j8]: <codex-artifacts/extracted-docs/jp341/Musashi-3.4.1-jp/HTML/08.html.txt>
[j9]: <codex-artifacts/extracted-docs/jp341/Musashi-3.4.1-jp/HTML/09.html.txt>
[j10]: <codex-artifacts/extracted-docs/jp341/Musashi-3.4.1-jp/HTML/10.html.txt>
[j11]: <codex-artifacts/extracted-docs/jp341/Musashi-3.4.1-jp/HTML/11.html.txt>
[j12]: <codex-artifacts/extracted-docs/jp341/Musashi-3.4.1-jp/HTML/12.html.txt>
[j13]: <codex-artifacts/extracted-docs/jp341/Musashi-3.4.1-jp/HTML/13.html.txt>
[j14]: <codex-artifacts/extracted-docs/jp341/Musashi-3.4.1-jp/HTML/14.html.txt>
[jAA]: <codex-artifacts/extracted-docs/jp341/Musashi-3.4.1-jp/HTML/AA.html.txt>
[jAB]: <codex-artifacts/extracted-docs/jp341/Musashi-3.4.1-jp/HTML/AB.html.txt>