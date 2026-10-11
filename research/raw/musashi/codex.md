<!-- provenance: helper=codex model=gpt-6.1-sol started=2026-10-11T18:14:20+00:00 exit=0 brief=research/briefs/ -->

# Musashi: research for Sweetbriar

[Saved report.md](codex-artifacts/report.md)

**Research date: 2026-10-11.** Historical dates below refer to the documents or releases concerned. Present-day product capabilities, library versions and Microsoft retirement policy were checked on this date. **Verified** means supported by a cited document, not independently tested in a running Musashi installation. Japanese quotations and paraphrases are identified as translations. Recommendations are design judgments.

## 1. TL;DR

- **Musashi was a real Japanese Mac client by Mamoru Misono, 弥園 護, trading as SONO SOFTWARE/SONOSOFT.** The archived business profile describes a sole proprietorship opened on 1997-07-01. [Vendor profile, archived 2008](http://web.archive.org/web/20080416224742/http://www.sonosoft.com/about).
- **It supported both classic Mac OS and Mac OS X.** The vendor announced 3.4 for OS X and Classic on 2002-05-12; an actual OS X application bundle survives. [Vendor news](http://web.archive.org/web/20031211053343/http://www.sonosoft.com/index.html), [Mac OS X 3.4.1 README](<codex-artifacts/artifacts/extracted-341-osx/Musashi-3.4.1/README.rtf>).
- **Its history did not end in 2002.** Packaged notes record 3.4.2 on 2006-06-19 and 3.5 on 2008-03-14; the vendor publicly announced freeware 3.5 on 2008-04-24. Preserve that date discrepancy. [English release notes](<codex-artifacts/artifacts/extracted-35-en/Musashi-3.5-en/What's new ?>), [2008 announcement](http://web.archive.org/web/20080526121455/http://www.sonosoft.com/archives/14).
- **The most valuable workflow is the Virtual Message Viewer:** newly received messages appear together with their filing-folder names, and search results use a comparable viewer. This separates reading from mailbox navigation. [Japanese manual §10](<codex-artifacts/artifacts/extracted-35-jp/Musashi-3.5-jp/HTML/10.html>), [Japanese manual §14](<codex-artifacts/artifacts/extracted-35-jp/Musashi-3.5-jp/HTML/14.html>).
- **Japanese text handling was an explicit concern:** JIS output selection, inline Japanese input conversion and fonts chosen by message language. This is documented functionality, not an assumption based on nationality. [Japanese manual §6](<codex-artifacts/artifacts/extracted-35-jp/Musashi-3.5-jp/HTML/06.html>), [Japanese manual §14](<codex-artifacts/artifacts/extracted-35-jp/Musashi-3.5-jp/HTML/14.html>).
- **Control came with limits.** Filters could run on receiving, sending or selected stored messages, but supported only two conditions and stopped at the first matching rule. [Japanese manual §12](<codex-artifacts/artifacts/extracted-35-jp/Musashi-3.5-jp/HTML/12.html>).
- **Do not romanticize reliability.** The Japanese release ledger records fixes for mail-loss bugs in early 3.x. These are historical fixed defects, not proof that every later version lost mail. [Japanese release notes](<codex-artifacts/artifacts/extracted-35-jp/Musashi-3.5-jp/リリースノート>).
- **Recommendation:** revive stable reading sessions, inspectable rules and excellent multilingual text handling in Sweetbriar. General unread views, templates and outgoing filters already survive elsewhere: Thunderbird explicitly supports filters after sending. [Thunderbird documentation, checked 2026-10-11](https://support.mozilla.org/en-US/kb/organize-your-messages-using-filters).

## 2. Findings

### 2.1 Evidence recovered and its limits

The starting [Macintosh Garden page](http://macintoshgarden.org/apps/musashi) was accessible over HTTP after HTTPS access failed. Its download links and a working mirror yielded four packages, all extracted locally:

| Package examined | Contents actually recovered |
| --- | --- |
| English 68K 3.5, musashi-68k-35-en.sea_.hqx | Application/resource forks; README; release notes; seventeen numbered/appendix HTML manual files; Default Module; Musashi Sounds; PFC plug-in; Register/Report/Upgrade templates; browser-oriented manual launchers. |
| Japanese 68K 3.5, musashi-68k-35-jp.sea_.hqx | Japanese README, longer Japanese release ledger, Japanese HTML manual, modules, plug-in resources and templates. |
| OS X 3.4.1, mu-osx-341.sit_.hqx | Musashi.app, executable and resources, six localization directories, English/Japanese HTML manuals, README, release and compatibility RTF files. |
| PowerPC 3.0.6, musashi-ppc-306.sit_.hqx | Application, README, registration/upgrade instructions, module and templates. Its upgrade notice says the English manual was still being translated. |

The primary download URLs are listed in §6. The manual citations link directly to the extracted documents saved beside this report. Archive listings are preserved in artifacts/listing-*.json. These are distribution packages, **not source-code archives**. [3.0.6 upgrade notice](<codex-artifacts/artifacts/extracted-306-ppc/Musashi Folder/Important things to read>), [Garden download inventory](http://macintoshgarden.org/apps/musashi).

REA could not recognize the legacy BinHex container. Format-aware extraction recovered its contents without running the application. Consequently, this report establishes what the shipped documents describe; it does not claim runtime measurements, successful connections to modern providers, or verification of the native mailbox format.

The Japanese documentation is especially valuable. The English overview accidentally reverses POP3 and SMTP, while the Japanese README correctly describes POP3 receiving and SMTP sending. Some English sections omit details present in Japanese. I therefore cross-checked them rather than treating every English sentence as authoritative. [English manual §3](<codex-artifacts/artifacts/extracted-35-en/Musashi-3.5-en/HTML/03.html>), [Japanese packaged README](<codex-artifacts/artifacts/extracted-35-jp/Musashi-3.5-jp/はじめにお読みください>).

### 2.2 History, platforms and business story

The vendor identifies **Mamoru Misono** as the developer. His archived introduction emphasizes useful, convenient, approachable software with some enjoyment in using it. The business was a Japanese sole proprietorship with application, web and business-system development work. This supports describing Musashi as a personally authored shareware product; it does not establish the size of a development team. [Vendor introduction](http://web.archive.org/web/20031211053343/http://www.sonosoft.com/index.html), [2008 business profile](http://web.archive.org/web/20080416224742/http://www.sonosoft.com/about).

| Date or period | Verified milestone |
| --- | --- |
| Before 1.2.3; exact date unknown | The vendor's release history says earlier versions were Japanese-only; 1.2.3 added quoted-printable handling. [Early release history](http://web.archive.org/web/19980525053101/http://www.sonosoft.com/musashi/relnote.html). |
| 1997-10-04 | An archived Info-Mac announcement describes MUSASHI 2.0.5, by Misono, for 68K/PowerPC, English/Japanese, System 7.0.1+, MacTCP/Open Transport, at least 2 MB RAM and 1 MB disk. This establishes circulation by then, not its initial launch date. [Archived announcement](https://groups.google.com/g/fj.sys.mac/c/qKNBNlStAw4). |
| 1998 archived website | 2.2.2 was offered in English, French, Italian and Japanese, with 68K and PowerPC builds. [Vendor product page](http://web.archive.org/web/19981203090133/http://www.sonosoft.com/musashi/). |
| 1998-12-26 / 1999-02-19 | Japanese 3.0 / 3.0.6 in the Japanese release ledger. The 3.0.6 upgrade notice identifies hierarchical folders, multilingual font settings and Remote Mail as 3.x additions. [Japanese release notes](<codex-artifacts/artifacts/extracted-35-jp/Musashi-3.5-jp/リリースノート>), [3.0.6 upgrade notice](<codex-artifacts/artifacts/extracted-306-ppc/Musashi Folder/Important things to read>). |
| 1999-05-25 | 3.1 fixes Latin header display, quoted-printable soft breaks and x-uuencode handling. [Japanese release notes](<codex-artifacts/artifacts/extracted-35-jp/Musashi-3.5-jp/リリースノート>). |
| 2000-05-08 | 3.2; its notes mention new Cyrillic, Turkish and Greek converters and attachment removal. [English release notes](<codex-artifacts/artifacts/extracted-35-en/Musashi-3.5-en/What's new ?>). |
| 2001-12-20 / 2001-12-22 | 3.3 / 3.3.1. SMTP authentication and POP-before-SMTP arrived in this period. [Vendor news](http://web.archive.org/web/20031211053343/http://www.sonosoft.com/index.html), [Japanese release notes](<codex-artifacts/artifacts/extracted-35-jp/Musashi-3.5-jp/リリースノート>). |
| 2002-05-12 / 2002-06-14 | Vendor news dates OS X/Classic 3.4 and 3.4.1 respectively. Impress separately lists 3.4.1 on June 14. [Vendor news](http://web.archive.org/web/20031211053343/http://www.sonosoft.com/index.html), [Impress, 2002-06-14](https://internet.watch.impress.co.jp/www/digest/backno/0206/14.htm). |
| 2006-06-19 | 3.4.2, including message-viewer error handling and attachment/SMTP fixes. [English release notes](<codex-artifacts/artifacts/extracted-35-en/Musashi-3.5-en/What's new ?>). |
| 2008-03-14 / 2008-04-24 | Packaged 3.5 ledger date / public vendor announcement and update date. The announcement offers OS X and Classic builds and declares 3.5 freeware. [English release notes](<codex-artifacts/artifacts/extracted-35-en/Musashi-3.5-en/What's new ?>), [Vendor announcement](http://web.archive.org/web/20080526121455/http://www.sonosoft.com/archives/14). |

Two chronology traps deserve explicit treatment. The archived download page labels **3.4.1 “12/May/2002,”** conflicting with the news, Impress and packaged OS X notes. Also, the Classic release ledger associates 3.4 with March 15, while the OS X release notes give May 12. These are distinguishable documents/build histories; forcing a single date would conceal the evidence. [Vendor downloads](http://web.archive.org/web/20031203195859/http://www.sonosoft.com/download.html), [OS X release notes](<codex-artifacts/artifacts/extracted-341-osx/Musashi-3.4.1/ReleaseNotes.rtf>).

Classic 3.5's README specifies System 7+, MacTCP or Open Transport and over 3 MB RAM, with operation under OS X restricted to Classic for **that build**. The separate OS X package specifies Mac OS X. A Macintosh Garden category such as “System 7” must not be used to erase the OS X version. [3.5 README](<codex-artifacts/artifacts/extracted-35-en/Musashi-3.5-en/README>), [Mac OS X 3.4.1 README](<codex-artifacts/artifacts/extracted-341-osx/Musashi-3.4.1/README.rtf>).

The commercial record changes over time. Vector’s **1998-01-22** review lists **¥3,000** and a **30-day trial** for J-2.1. [Vector review](https://www.vector.co.jp/magazine/softnews/980122/n9801225.html). The 1999-era 3.0.6 README asks **US$33**, with a **40-day trial** and **US$9** upgrade from 2.x; OS X 3.4.1 asks **US$25** and US$9. An archived Japanese registration form in December 2003 lists **¥2,800**, or **¥1,000** for upgrading. These are historical prices, not current offers. [3.0.6 README](<codex-artifacts/artifacts/extracted-306-ppc/Musashi Folder/README>), [Mac OS X 3.4.1 README](<codex-artifacts/artifacts/extracted-341-osx/Musashi-3.4.1/README.rtf>), [Japanese registration form](http://web.archive.org/web/20031231061350/http://www.sonosoft.com/jp/regform.html).

Registration used services including Kagi; the vendor described manually returning a registration code within two or three days of receiving notice. The Japanese page also linked Vector registration. That is verified participation in a shareware distribution/payment ecosystem, rather than proof of a boxed retail business. [English registration](http://web.archive.org/web/20031203202422/http://www.sonosoft.com/register.html), [Japanese registration](http://web.archive.org/web/20031211205640/http://www.sonosoft.com/jp/register.html).

**Why did it decline?** No verified cancellation notice or complete explanation was found. A February 2005 user said updates had stopped and that they had moved to Apple Mail. Later releases contradict interpreting that as permanent cessation. In April 2008 the vendor acknowledged the old interface and said 3.5 mainly removed shareware machinery with small fixes. [2005 user account](https://plaza.rakuten.co.jp/kojim/diary/200502280000/), [2008 vendor announcement](http://web.archive.org/web/20080526121455/http://www.sonosoft.com/archives/14).

**Inference:** intermittent maintenance and an increasingly dated product probably reduced its appeal. A contemporary counterpoint already existed: Adam Engst argued in January 2003 that application-level multiuser support mattered less under OS X because operating-system accounts supplied separation. Neither observation proves why Misono eventually stopped developing Musashi. [Macworld, January 2003, printed p.14](https://vintageapple.org/macworld/pdf/MacWorld_0301_January_2003.pdf).

**Code and people:** the 3.5 license retains Misono's ownership and restrictions; freeware did not make it open source. No source release or transfer was verified. Credits identify Yasuhiko Hino for sounds, Rolf Zimmermann for icons, and Marco Piovanelli's WASTE text editor, not a documented full-time team. [English manual §1](<codex-artifacts/artifacts/extracted-35-en/Musashi-3.5-en/HTML/01.html>), [3.5 README](<codex-artifacts/artifacts/extracted-35-en/Musashi-3.5-en/README>).

As checked **2026-10-11**, the current sonosoft.com company profile still names **弥園 護**, states founding in July 1997 and incorporation on **2008-11-20**, and describes application/system services. Continuity with the earlier business is a strong inference from the same name, founder and domain; Musashi staffing, code custody and present maintenance remain **unverified**. A claim that “Sonosoft died” would be unsupported. [Current company profile](https://sonosoft.com/?page_id=32), [Current services](https://sonosoft.com/?page_id=34).

### 2.3 Concrete feature inventory

Unless otherwise stated, these are documented in the recovered 3.5 manuals. Descriptions drawn from the Japanese manual are **English translations/paraphrases**, with the English manual used for its actual English command labels. They are not extrapolations to every earlier version.

| Area | Documented behavior |
| --- | --- |
| Users and administration | Single-user or multiuser mode; manager password; add/delete/rename users; per-user passwords and configuration directories; administrator can reset a user's password. Changing the directory does not automatically move existing configuration files. [English manual §4](<codex-artifacts/artifacts/extracted-35-en/Musashi-3.5-en/HTML/04.html>). |
| Switching users | Options → Switch user, without quitting; password requested if configured. Large folder/message collections may require increasing Classic application memory before startup succeeds. [English manual §5](<codex-artifacts/artifacts/extracted-35-en/Musashi-3.5-en/HTML/05.html>). |
| Multiple accounts | Per-account POP login, SMTP host, password, email/name, Reply-To, default signature, output charset and attachment format; send-only accounts; explicit port suffixes such as smtp.example:587. [English manual §6](<codex-artifacts/artifacts/extracted-35-en/Musashi-3.5-en/HTML/06.html>). |
| Receiving | Manual account selection or interval/startup checks; holding the check-mail button opens an account menu. New arrivals open a Virtual Message Viewer with a folder-name column and normal message operations. Another requested account check queues behind the current one. [Japanese manual §10](<codex-artifacts/artifacts/extracted-35-jp/Musashi-3.5-jp/HTML/10.html>). |
| Remote Mail | Do → Remote mail: obtain subject, sender, date and size from the POP server; mark messages to retrieve, delete, or both; execute the choices before downloading full messages. **This is documented POP functionality.** [Japanese manual §13](<codex-artifacts/artifacts/extracted-35-jp/Musashi-3.5-jp/HTML/13.html>). |
| Retention and authentication | Leave messages on POP server; optionally remove after a specified number of days; APOP; POP-before-SMTP; SMTP authentication. Japanese 3.3.1 notes add LOGIN/PLAIN after 3.3's CRAM-MD5, so the preferences manual's CRAM-MD5-only wording is stale. [English manual §6](<codex-artifacts/artifacts/extracted-35-en/Musashi-3.5-en/HTML/06.html>), [Japanese release notes](<codex-artifacts/artifacts/extracted-35-jp/Musashi-3.5-jp/リリースノート>). |
| Folder layout | Large icons, small icons or list; free icon positioning outside list mode; folder icons selectable; nested folders up to three levels. A folder contains either subfolders or messages, not both. [Japanese manual §7](<codex-artifacts/artifacts/extracted-35-jp/Musashi-3.5-jp/HTML/07.html>). |
| Reading and selection | Message-list plus content viewer; selective header display; hide list; multi-selection; sort by sender/name, date or subject, ascending/descending, with an unsorted state. [Japanese manual §7](<codex-artifacts/artifacts/extracted-35-jp/Musashi-3.5-jp/HTML/07.html>). |
| Keyboard reading | Space pages forward and advances at the message end; Delete scrolls backward. Holding Control–1…9 scrolls forward at nine speeds; adding Shift reverses direction. Arrow keys change selection; Return/Enter opens a folder. These are context-specific, not universal Delete-key behavior. [Japanese manual §7](<codex-artifacts/artifacts/extracted-35-jp/Musashi-3.5-jp/HTML/07.html>). |
| Folder commands | English Do menu includes New folder, Rename folder, Delete folder, Change folder icon and Open. Drag folders between parents; Option-click the Folder heading toggles automatic name sorting. [English manual §7](<codex-artifacts/artifacts/extracted-35-en/Musashi-3.5-en/HTML/07.html>). |
| Message operations | Move/copy/trash actions and drag-and-drop organization; double-click attachment icons to open them; Command-click URLs with Internet Config/Internet control-panel support. [English manual §7](<codex-artifacts/artifacts/extracted-35-en/Musashi-3.5-en/HTML/07.html>), [Module specification](<codex-artifacts/artifacts/extracted-35-en/Musashi-3.5-en/HTML/AB.html>). |
| Filters | Name/edit/reorder filters; up to two conditions joined by AND/OR. Examples include subject containing text and account identity. Actions specify filing destination and highlight; triggers are receiving and sending. Manual application can select filters for stored messages. First matching rule wins. Vector’s earlier **J-2.1** review explicitly documents header/body text conditions. [Vector review](https://www.vector.co.jp/magazine/softnews/980122/n9801225.html), [Japanese manual §12](<codex-artifacts/artifacts/extracted-35-jp/Musashi-3.5-jp/HTML/12.html>). |
| Search | File → Find offers read/unread/sent/unsent views; detailed string/date criteria; refine an existing result set; Find string searches messages listed in the current viewer. Results appear in the Virtual Message Viewer. [Japanese manual §14](<codex-artifacts/artifacts/extracted-35-jp/Musashi-3.5-jp/HTML/14.html>). |
| Address book | Hierarchical groups, including groups inside groups; entries with comments; save a message sender; explicit Save/Command-S. Names have punctuation/whitespace restrictions. Recipient fields accept colon-separated group paths. [English manual §8](<codex-artifacts/artifacts/extracted-35-en/Musashi-3.5-en/HTML/08.html>), [English manual §11](<codex-artifacts/artifacts/extracted-35-en/Musashi-3.5-en/HTML/11.html>). |
| Signatures | Named multiple signatures, account defaults and composer selection; separate editor with explicit save, rename and delete. [English manual §9](<codex-artifacts/artifacts/extracted-35-en/Musashi-3.5-en/HTML/09.html>). |
| Composer | Tabs for recipients, account/signature and detailed encodings; Reply, Reply to All and Reply to Sender; Direct Forward preserves original sender/date information. Save stores the draft in Outbox; Send Later queues it. [English manual §11](<codex-artifacts/artifacts/extracted-35-en/Musashi-3.5-en/HTML/11.html>). |
| Attachments in prose | Attachment icons inserted at the caret, moved by dragging and deleted like a character; Option-click temporarily shows caret position/total character count. The Japanese manual describes preserving icon placement when another Musashi user reads the message. [Japanese manual §11](<codex-artifacts/artifacts/extracted-35-jp/Musashi-3.5-jp/HTML/11.html>). |
| Templates | Open or insert a template; plain-text files containing optional To, Cc, Bcc, Subject, Account and Signature headers, blank line, then body. This is a documented editable file format. [English manual §11](<codex-artifacts/artifacts/extracted-35-en/Musashi-3.5-en/HTML/11.html>), [Template specification](<codex-artifacts/artifacts/extracted-35-en/Musashi-3.5-en/HTML/AA.html>). |
| Reply/text preferences | Configurable reply/forward introduction using sender, subject and date substitutions; quotation prefix; optional quoted headers/sender name; date format, four-digit years, weekday/seconds and time-zone conversion; outgoing wrap width. [English manual §6](<codex-artifacts/artifacts/extracted-35-en/Musashi-3.5-en/HTML/06.html>). |
| Japanese/display preferences | Japanese (JIS) output; inline Japanese input conversion; per-language fonts to avoid Latin text appearing as half-width kana; three quote-depth highlight colors, configurable quote markers and URL highlighting. [Japanese manual §6](<codex-artifacts/artifacts/extracted-35-jp/Musashi-3.5-jp/HTML/06.html>), [Japanese manual §14](<codex-artifacts/artifacts/extracted-35-jp/Musashi-3.5-jp/HTML/14.html>). |
| Other preferences | Automatic attachment extraction to a chosen directory; text-file creator; default incoming/outgoing account; send queued messages during checking; standard Mac OS versus custom controls; printing fonts/style; arrival sound/dialog; warning-dialog timer that automatically answers Yes. [English manual §6](<codex-artifacts/artifacts/extracted-35-en/Musashi-3.5-en/HTML/06.html>). |
| Import/export and local sharing | Plug-in menus for messages/address books in both directions; drag messages to Finder as text; printing; Local Forward between application users uses local files without a server; migration of 2.x configuration. [English manual §14](<codex-artifacts/artifacts/extracted-35-en/Musashi-3.5-en/HTML/14.html>). |
| Attachment formats and converters | Base64, AppleDouble/AppleSingle, BinHex and uuencode; language-conversion plug-ins. Release notes mention Eudora Archive import, Eudora-format export and UNIX /etc/aliases address export. The **native mailbox format itself is unverified**. [English manual §3](<codex-artifacts/artifacts/extracted-35-en/Musashi-3.5-en/HTML/03.html>), [Japanese release notes](<codex-artifacts/artifacts/extracted-35-jp/Musashi-3.5-jp/リリースノート>). |
| Modules and personality | Creator/type MUSA/MODL; cicn icons and clut color tables; 24×24 action icons, 32×32 empty/nonempty folder pairs, 12×12 message-state icons; seven message highlights, three quote colors; PICT preview in the data fork. This was a published customization format. [Module specification](<codex-artifacts/artifacts/extracted-35-en/Musashi-3.5-en/HTML/AB.html>). |

Important boundaries: “Send Later” here means a queue, not verified scheduling for a chosen future timestamp. A Virtual Message Viewer is not proof of conversation threading. No documented References/In-Reply-To thread-building algorithm, regex filtering, arbitrary scripting, IMAP, JMAP, Gmail API, Exchange integration, or encrypted local store was established.

### 2.4 What was distinctive about the Japanese design?

**Text correctness was a first-class user decision.** The Japanese manual tells users to select Japanese (JIS) output and explains that using a Japanese font for European text can yield half-width-kana mojibake. Per-language font selection and inline conversion were concrete responses to that environment. It would be wrong to turn this into a claim that Musashi supported every Japanese charset automatically; the complete converter repertoire and detection behavior were not verified. [Japanese manual §6](<codex-artifacts/artifacts/extracted-35-jp/Musashi-3.5-jp/HTML/06.html>), [Japanese manual §14](<codex-artifacts/artifacts/extracted-35-jp/Musashi-3.5-jp/HTML/14.html>).

Character sets and attachment encodings must remain separate. ISO-2022-JP is specified by **RFC 1468**; quoted-printable/Base64 concern MIME transfer representation, while BinHex/AppleDouble concern other aspects of attachment representation. Musashi's documents expose both output-code and attachment-format controls. The lesson for Sweetbriar is to preserve and explain these layers when repairing historical mail. [RFC 1468](https://www.rfc-editor.org/rfc/rfc1468.html), [RFC 2045: MIME transfer encodings](https://www.rfc-editor.org/rfc/rfc2045.html), [RFC 2047: encoded headers](https://www.rfc-editor.org/rfc/rfc2047.html), [English manual §3](<codex-artifacts/artifacts/extracted-35-en/Musashi-3.5-en/HTML/03.html>).

**Its filing interface had spatial character.** Folder icons and freely arranged folder windows offered more visual identity than a folder-name tree alone. The module specification distinguishes empty/nonempty folder pictures and replied/forwarded/pending/attachment status icons. These are demonstrable design choices; a special Japanese theory of spatial organization is **unverified**. [Module specification](<codex-artifacts/artifacts/extracted-35-en/Musashi-3.5-en/HTML/AB.html>), [Japanese manual §7](<codex-artifacts/artifacts/extracted-35-jp/Musashi-3.5-jp/HTML/07.html>).

Customization had actual third-party participation. As checked **2026-10-11**, Vector preserves listings for **PowerMigaModule, GaiarthModule, migaModule, migaModuleCOMPLETE**, and **KAEDE**, a Musashi 2 address-book-to-text utility. The freeware migaModuleCOMPLETE, by **miga**, targets Musashi 3.x; its description says the icons have a Finder-like feel (**translation**). These listings verify an ecosystem’s existence, not the modules’ runtime quality. [Vector Musashi category](http://www.vector.co.jp/vpack/filearea/mac/net/mail/musashi), [Module description](http://www.vector.co.jp/soft/mac/net/se112846.html).

**Shared-computer operation was deliberate.** INTERNET magazine's July 1999 profile of **3.0.6**, printed p.217, describes password-separated users and a “共通メール” folder visible to several users. Translation: **shared/common mail**. The vendor also promoted small-office use on a shared Mac. This is not equivalent to a modern server-delegated Exchange mailbox. [INTERNET magazine, July 1999](https://iwparchives.jp/wp-content/themes/twentytwelve/bn/pdf/im199907-198-email.pdf), [Vendor product page](http://web.archive.org/web/20031203201821/http://www.sonosoft.com/musashi.html).

**The unusual reading workflow is better evidenced than threading.** A 2005 Japanese user, managing 212 Apple Mail folders, wanted unread messages gathered in a preview, then removed/filed after reading and refreshing. They said Musashi had provided this workflow. Translation of their description: **“Once marked as read, it is placed in the filing folder.”** [User account, 2005-02-28](https://plaza.rakuten.co.jp/kojim/diary/200502280000/).

That recollection is valuable evidence of the experience, but it does **not** establish a literal disk move triggered by reading. The manuals establish a new-arrival viewer, folder column, received-mail filtering and unread searches; the exact refresh/removal semantics are **unverified**. The safest reconstruction is a view that keeps filing from interrupting reading, with storage timing left open pending runtime investigation. [Japanese manual §10](<codex-artifacts/artifacts/extracted-35-jp/Musashi-3.5-jp/HTML/10.html>), [Japanese manual §12](<codex-artifacts/artifacts/extracted-35-jp/Musashi-3.5-jp/HTML/12.html>), [Japanese manual §14](<codex-artifacts/artifacts/extracted-35-jp/Musashi-3.5-jp/HTML/14.html>).

### 2.5 What users loved—and what they disliked

**Compactness was the author’s stated goal.** Vector’s **1998-01-22** review of J-2.1 includes Misono’s first-person explanation: he wanted a client he would want to use himself and intended to preserve **“a responsive, compact style”** while other applications grew heavier (**translation** of 軽快で、コンパクトなスタイル). He explicitly mentions older, less powerful Macs and updates guided by user feedback. Reviewer **福住 護** praises understandable setup, account/signature switching and sufficient everyday functionality, but notes the missing mail-size download limit. This is primary evidence of design intent and contemporary reception, not a measured performance result. [Vector review and developer comment](https://www.vector.co.jp/magazine/softnews/980122/n9801225.html).

**Small, quick, bilingual.** Two reviews preserved on Brightblack carry dates **1998-09-13** for Musashi 2.2 and **1999-07-17** for Musashi 3. The first praises stable operation on a PowerBook 190 and an iMac, Japanese input/display and accessible preferences; its comparison with Outlook Express's kanji handling is that reviewer's experience, not a general interoperability benchmark. It also notes awkward English filter wording, no spell checker or inline HTML, and no nested folders in 2.2. [2.2 review](https://www.brightblack.net/retro/review-of-musashi-2.2/).

The 3.x review welcomes nested mailboxes, incoming/outgoing filters, separate window settings and a tidy tabbed composer. Its rating rises from the earlier review's 88% to 92%. Those are historical editorial scores, not measurements of performance or safety. [3.x review](https://www.brightblack.net/retro/review-of-musashi-3/).

**A manageable middle ground.** Macintosh Repository reproduces a passage attributed to Drew D. Saur's Mac Orchard in 2000, praising the visual interface and positioning Musashi for people who found Eudora Light dated and Eudora Pro overwhelming. That attribution is **secondhand preservation**; I did not recover the original 2000 page. [Repository's attributed excerpt](https://www.macintoshrepository.org/903-musashi-3-x).

**Contemporary mainstream criticism was real.** Adam C. Engst's Macworld guide, posted **2002-09-30** and printed in October, calls Musashi's message-list/preview interface **“fast and easy to use.”** It also reports no automatic size threshold for skipping large POP messages and does not include Musashi among the IMAP clients. The comparison table gives Musashi 3.4 two mice, praising multiple users but criticizing absent inline HTML/graphics, spelling support, documentation, search and filtering. The article's broad spelling-checker sentence conflicts with its Musashi-specific table; the specific criticism is more useful than assuming a verified built-in checker. [Web article](https://www.macworld.com/article/1001571/email.html), [October 2002 issue, printed pp.68–71](https://vintageapple.org/macworld/pdf/MacWorld_0210_October_2002.pdf).

**Users disputed the priorities.** In Macworld's January 2003 letters, James Platt defends multi-term, mailbox-scoped searching using existing filters, protection of downloaded mail from other application users, and plain-text viewing with HTML opened separately. He calls Musashi **“the easiest multiple-user environment to set up.”** Engst replies that OS X user accounts reduce the distinction. This is a useful disagreement over workflow and audience; Platt's “Sherlock-like” description does not establish a particular indexing engine or relevance-ranking implementation. [January 2003 issue, printed p.14](https://vintageapple.org/macworld/pdf/MacWorld_0301_January_2003.pdf).

**Maintenance and interoperability frustrated survivors.** A comment dated **2008-04-25** on the vendor's freeware announcement says, in translation, **“I'm really glad updates have resumed.”** The writer reports attachment errors, an account-setting workaround and a desire for SSL. These are one user’s reports, not independently reproduced defects. [Vendor announcement and comment](http://web.archive.org/web/20080526121455/http://www.sonosoft.com/archives/14).

The limits supported by stronger primary evidence are:

- **Safety defects existed and were fixed:** Japanese 3.0.1 notes describe received mail disappearing after a forced termination; 3.0.5 describes mail disappearing when cancelling a user switch; 3.2.1 describes deletion bypassing Trash for some messages; 3.2.4 fixes exposure of parts of a multiline Bcc header in 3.2.3. These are precise affected behaviors in historical release notes. [Japanese release notes](<codex-artifacts/artifacts/extracted-35-jp/Musashi-3.5-jp/リリースノート>).
- **Scaling was constrained:** the startup manual advises increasing Classic application memory if a large collection prevents launch. No timing figures or maximum mailbox size were verified. [English manual §5](<codex-artifacts/artifacts/extracted-35-en/Musashi-3.5-en/HTML/05.html>).
- **Hierarchy and rules were bounded:** see the three-level folder and two-condition filter limits in §2.3. These restrict claims of unlimited organizational power.
- **HTML was externalized:** the Japanese manual treats HTML mail as attachments opened through the browser. That documents a choice, not immunity to malicious content. [Japanese manual §7](<codex-artifacts/artifacts/extracted-35-jp/Musashi-3.5-jp/HTML/07.html>).
- **App passwords are not established encryption:** the manager can reset users' passwords, and no cryptographic storage specification was found. [English manual §4](<codex-artifacts/artifacts/extracted-35-en/Musashi-3.5-en/HTML/04.html>).
- **TLS absence is an archival claim with contemporary corroboration:** Garden says no SSL; the 2008 comment requests it. A complete protocol audit of every build remains **unverified**. [Garden](http://macintoshgarden.org/apps/musashi), [2008 comment](http://web.archive.org/web/20080526121455/http://www.sonosoft.com/archives/14).

### 2.6 What is actually lost or weakened in today's clients?

**Comparison date: 2026-10-11.** This is a documentation comparison, not a six-client hands-on test. An undocumented exact behavior is marked **unverified**, rather than treated as absent.

| Current client | Documented overlap with Musashi | What this does—and does not—establish |
| --- | --- | --- |
| Apple Mail on Mac | Smart Mailboxes gather criteria-matching messages across physical mailboxes; incoming rules can move or highlight mail. [Apple guide](https://support.apple.com/en-mide/guide/mail/mlhlp1190/mac). | Cross-folder views and color filing are retained. The consulted documentation does not establish Musashi's reported keep-visible-until-refresh reading session. Exact equivalence: **unverified**. |
| Outlook for Mac | Unread filtering, including Shift-Command-O; the feature matrix lists saved searches, categories and unified inbox. [Unread guide](https://support.microsoft.com/en-us/outlook/view-only-your-unread-messages-in-outlook-for-mac), [Feature matrix](https://support.microsoft.com/en-gb/outlook/outlook-for-mac-features?nochrome=true). | Filtering itself is retained. The matrix marks composing with different text encodings unavailable: a documented weakening of the old explicit encoding control, subject to the matrix's stated product scope. |
| Gmail web | Unread-first layout, multiple inbox sections defined by search operators or labels, and configurable priority sections. [Google guide](https://support.google.com/mail/answer/18522?ctx=topic&hl=en&topic=1669050). | Gathering/filtering is retained. These layouts do not, by themselves, establish a stable cross-folder reading session or application-local profiles. |
| Spark | Smart Inbox sorts personal/newsletter/notification mail; unified accounts. Templates provide automatic fields, custom placeholders and team sharing. [Smart Inbox](https://sparkmailapp.com/features/smart_inbox), [Templates](https://sparkmailapp.com/teams/templates). | Triage and templating survive. Spark warns that platform experiences vary. Its category workflow is a different documented organization model; precise Musashi refresh behavior is **unverified**. |
| Thunderbird | Per-account filters run on incoming mail, after sending, on archiving and manually; actions include move/delete/forward. [Mozilla guide, updated 2024-12-28](https://support.mozilla.org/en-US/kb/organize-your-messages-using-filters). | **Outgoing filters are not a lost feature.** Thunderbird is an important counterexample to claims that classic-client control disappeared altogether. |
| Mimestream | Gmail-backed search and filters, unread/starred list filtering, profiles, account colors, unified inbox, multiple windows, variable-based templates and Send and Archive. [Vendor feature page](https://mimestream.com/). | Many desired features already exist in a native Mac product. Its documented foundation is Gmail API; this does not establish the same cross-provider experience for IMAP/JMAP/Exchange. |

The best-supported revival candidates are therefore **combinations and interaction details**, rather than a list of supposedly extinct capabilities:

1. A persistent reading session spanning filing destinations, with visible folder/account context.
2. A reader that combines one-key progression, deliberate quote styling and unobtrusive technical controls.
3. Plain-text, portable templates that carry account/signature choices.
4. A coherent visual vocabulary for folders and message state.
5. Explicit local transfer/export that does not require sending a copy through a mail server.

These are recommendations derived from the documented inventory, not claims that every modern client lacks each component. Custom module resource editing, nine-speed key scrolling and physically shared application-user mailboxes are particularly specific Musashi implementations; exact modern equivalents across all six products remain **unverified**.

## 3. Options compared

The following tables are **design judgments for Sweetbriar**, not recovered Musashi specifications.

### Reading and filing

| Option | Benefit | Cost or failure mode | Judgment |
| --- | --- | --- | --- |
| Immediately file mail; use a live unread query | Simple and reflects current state | Read messages can leave the working list while the user is navigating | Useful standard mode |
| Immediately file mail; preserve a reading-session list | Keeps the user's place while mailbox state synchronizes normally | Needs explicit refresh, status indicators and predictable handling of deletion elsewhere | **Recommended Musashi-inspired mode** |
| Delay actual filing until the user marks read | Literal implementation of the 2005 description | Reading on another device may trigger unexpected filing; “read” and “processed” become entangled | Optional experiment, not default |
| Keep all new mail in Inbox until explicit processing | Clear, deliberate triage | Duplicates a workflow many users already manage manually | Offer as an alternative |

The recommendation does not depend on proving whether Musashi physically deferred moves. Its acceptance criterion is observable: **a rule may file a message, and reading it must not steal the user's position in the current session**.

### Portability and personality

| Choice | Advantage | Strongest objection |
| --- | --- | --- |
| Rust owns message state, queries, rules, templates and synchronization; Swift owns Mac interaction | Linux can reuse behavior while macOS retains native controls and editing | FFI boundaries and asynchronous updates require careful design |
| Recreate Musashi's custom controls and resource-fork modules | Maximum visual resemblance | Conflicts with Sweetbriar's modern native-platform goal |
| Native controls, optional folder symbols/colors, accessible message-state badges | Retains identity without requiring users to learn obsolete widgets | Less nostalgic than a faithful skin |
| Local user/profile separation first; server sharing only through supported permissions | Keeps simple workflows independent of provider features | Cannot reproduce team collaboration purely client-side |

## 4. Recommendations for Sweetbriar

Everything in this section is a **proposal**, with protocol/library facts cited separately.

### 4.1 Build the Virtual Message Viewer as a stable reading session

Provide **New arrivals**, **Unread across accounts**, and arbitrary search-result windows. Show account, actual folder/labels, unread status and pending operations. Keep a session's ordered message IDs stable as the user reads; refresh should be explicit or deferred until it will not disrupt navigation. New arrivals can appear behind a clear “new messages” affordance.

A message marked read should remain in this session until refresh or explicit removal. Offer separate **Read**, **Done** and **File** actions; reading a message should not silently mean that work on it is finished. Store the session locally so a restart can restore position.

This behavior is **client-side**. Synchronizing read state or moving mail uses the provider's ordinary API:

| Backend | Concrete read/filing operation | Required server capability |
| --- | --- | --- |
| IMAP | STORE the Seen flag; use UID MOVE when supported, with cautious fallback/reconciliation | IMAP server; capability-aware MOVE support. [RFC 9051](https://datatracker.ietf.org/doc/html/rfc9051), [RFC 6851](https://www.rfc-editor.org/rfc/rfc6851.html). |
| JMAP | Email/set updates to keywords/$seen and mailboxIds | JMAP Mail server. [RFC 8621, §§4.1 and 4.6](https://datatracker.ietf.org/doc/html/rfc8621). |
| Microsoft 365 | PATCH message.isRead; POST message/move for filing | Microsoft Graph permissions and supported mailbox/account. [Update API](https://learn.microsoft.com/en-us/graph/api/message-update?view=graph-rest-1.0), [Move API](https://learn.microsoft.com/en-us/graph/api/message-move?view=graph-rest-1.0). |
| Gmail API | Modify message labels, including read/inbox state through system labels | Gmail API authorization. [Label model](https://developers.google.com/gmail/api/guides/labels), [messages.modify](https://developers.google.com/gmail/api/reference/rest/v1/users.messages/modify). |

These APIs were checked **2026-10-11**. Graph's move documentation describes creating a destination copy and removing the original; use a stable **Sweetbriar-local identity** rather than assuming a provider identifier is immutable through every operation. IMAP MOVE also permits partial failure across a set of messages; it is not a transaction over an entire mailbox. [Graph move semantics](https://learn.microsoft.com/en-us/graph/api/message-move?view=graph-rest-1.0), [RFC 6851](https://www.rfc-editor.org/rfc/rfc6851.html).

**Trade-off:** a session can contain messages that no longer match its original query. Display their current state honestly. **Strongest counterargument:** users may interpret “Unread” as an always-live truth. Address that with an explicit session mode and a conventional live view alongside it.

### 4.2 Revive deterministic rules, with explanations

Use a shared Rust predicate model for search and rules. Support named conditions, AND/OR groups, account scope, destination and presentation actions. Add explicit triggers for **incoming**, **after successful send**, and **manual application**; offer **first-match wins** as a clearly named policy inspired by Musashi.

Show why a message matched, which actions ran and which were skipped. Preview the effect on existing messages before applying a rule. Persist execution records so resynchronization does not repeatedly forward or move the same message.

Matching, highlighting and explanations are **client-side**. Moving mail or changing synchronized labels needs the backend operations above. Execution while Sweetbriar is closed needs either a maintained local service or supported server-side rules; do not promise to translate arbitrary local predicates to every provider.

**Trade-off:** a richer condition editor can lose Musashi's simplicity. **Strongest counterargument:** Thunderbird already supplies capable filters. Sweetbriar must improve discoverability, reproducibility and cross-provider behavior, rather than merely duplicate a checkbox.

### 4.3 Make Japanese mail a test corpus, not a locale checkbox

Keep original message bytes immutable. Display decoded Unicode, record declared versus selected encoding, and offer a **local decoding override** with a preview. An override must not rewrite the original message or silently change what will be forwarded/exported.

Evaluate **mail-parser 0.11.9** with its optional **encoding_rs** dependency. Its documentation lists Shift_JIS, EUC-JP and ISO-2022-JP support, alongside MIME parsing. This is a candidate selection based on documentation checked **2026-10-11**, not a guarantee that all malformed Musashi-era mail will decode correctly. [mail-parser documentation](https://docs.rs/mail-parser/latest/mail_parser/).

Test mixed Latin/Japanese headers, incorrectly declared charsets, quoted-printable soft breaks, encoded filenames, half-width kana and historical attachments. Offer clear decoding controls in an inspector, while the composer defaults to modern Unicode output unless interoperability requires an explicit alternative. Header decoding should follow the relevant MIME rules. [RFC 2047](https://www.rfc-editor.org/rfc/rfc2047.html).

Use the Mac text system for Japanese input and font fallback; keep parsing/conversion in Rust. Give quote depth a readable visual treatment and preserve selection/caret behavior during input composition. These are **client-side** features; no new IMAP/JMAP/Exchange capability is required to render retained MIME correctly.

**Trade-off:** compatibility testing and manual repair controls add complexity. **Strongest counterargument:** most current mail needs none of this. Place advanced controls in the inspector and treat repair as a preservation feature, rather than burdening ordinary composition.

### 4.4 Couple precise search with useful result views

Reproduce the useful distinction between searching mail and operating on the results. Let users refine a result set, inspect raw headers, move/export selected matches, and turn a query into a named view or rule predicate.

Evaluate **Tantivy 0.26.2** for local indexing and **lindera-tantivy 5.0.1** for Japanese morphological tokenization; these versions were displayed on their documentation pages on **2026-10-11**. Tantivy supports custom tokenizers; Lindera's integration documents Japanese dictionaries such as IPADIC and UniDic. [Tantivy](https://docs.rs/tantivy/latest/tantivy/), [Tokenizer API](https://docs.rs/tantivy/latest/tantivy/tokenizer/index.html), [Lindera integration](https://docs.rs/lindera-tantivy/latest/lindera_tantivy/).

My recommendation is to index original/display-normalized text separately and provide an exact-text path as well as language-aware matching. A tokenizer that works well for sentences must also be tested against addresses, order numbers, kanji/kana variations and mixed scripts. Treat the search index as rebuildable; it must not become the authoritative store for mail.

Local search requires **no server feature** after content is downloaded. Searching uncached server mail requires backend search support and must clearly report its scope. JMAP specifies Email/query filtering; Mimestream demonstrates the alternative of Gmail-backed search. Those are options, not proof that all providers return identical results. [RFC 8621 §4.4](https://datatracker.ietf.org/doc/html/rfc8621), [Mimestream](https://mimestream.com/).

**Trade-off:** a local index costs disk, CPU and maintenance effort. **Strongest counterargument:** provider search can avoid a full local download. Support it as a complementary route, with visible completeness and offline limitations.

### 4.5 Revive portable templates and deliberate text editing

Store templates as human-readable files with a versioned header schema for identity, signature, recipients and subject. The original six-field Musashi format is a useful import target, but Sweetbriar should define and document its own format. Include native editing, previews and unresolved-placeholder checks.

Provide explicit **Reply to Sender** for mailing-list contexts, a readable quoting model and attachments with clear placement/ownership. Preserve convenient one-key reading progression; make unusual scrolling shortcuts optional and remappable. These are **client-side** choices. Sending still uses the account backend; ordinary template editing requires no server support.

**Trade-off:** portable files require validation and migration when the schema changes. **Strongest counterargument:** Spark and Mimestream already offer sophisticated templates. The distinctive Sweetbriar value would be portability, account correctness, offline use and common behavior across backends. [Spark templates](https://sparkmailapp.com/teams/templates), [Mimestream templates](https://mimestream.com/).

### 4.6 Keep personality, export and local sharing; use real isolation

Use native Mac controls with customizable folder symbols, restrained colors, quote styling and status badges. Make color supplementary to text/icons. Put corresponding presentation choices in each platform's native UI, rather than attempting to share a custom-drawn Mac interface with Linux.

Allow local export of raw .eml plus readable text, with explicit retention of original headers and attachment bytes. A Musashi-inspired Local Forward can copy mail between explicitly authorized local profiles without sending SMTP mail. That is **client-side**.

Actual shared/team mailboxes need server permissions, account access and correct synchronization. The magazine's shared application folder cannot supply those guarantees. For local privacy, prefer operating-system accounts or separately encrypted stores over an application login whose encryption properties are unspecified.

**Trade-off:** deep module theming and shared-computer administration widen scope. **Strongest counterargument:** they may serve few present-day users. Ship folder identity and robust export first; add local profile transfer only when a concrete user workflow warrants it.

### 4.7 Make safety the prerequisite for every revived workflow

The early mail-loss and Bcc fixes are reasons to copy interaction ideas while setting a higher preservation standard. The old automatic-Yes dialog timer should remain a historical curiosity; Sweetbriar should keep destructive decisions explicit. [Japanese release notes](<codex-artifacts/artifacts/extracted-35-jp/Musashi-3.5-jp/リリースノート>), [English manual §6](<codex-artifacts/artifacts/extracted-35-en/Musashi-3.5-en/HTML/06.html>).

Evaluate **rusqlite 0.40.2**, the version displayed on **2026-10-11**, for a transactional metadata, draft and operation store. A proposed configuration is SQLite WAL with synchronous=FULL, with macOS fullfsync evaluated on supported storage. SQLite explicitly distinguishes WAL FULL durability from NORMAL, which may lose recent commits after power loss. Settings alone do not prove that separately stored message blobs are durable. [rusqlite](https://docs.rs/rusqlite/latest/rusqlite/), [SQLite synchronous/fullfsync documentation](https://www.sqlite.org/pragma.html).

Proposed acceptance conditions:

- Persist incoming raw mail and its metadata before authorizing destructive server cleanup.
- Acknowledge draft/settings saves only after their durable commit; preserve recoverable draft revisions.
- Journal pending remote mutations; reconcile after reconnect instead of blindly replaying uncertain sends or moves.
- Keep raw messages available while rebuilding search; a damaged index must not destroy mail.
- Exercise process termination, power-loss simulation, disk-full errors, interrupted imports, account switching and cancelled user operations.
- Verify recovery by message counts, byte hashes, draft contents, settings and Bcc visibility—not just whether the app relaunches.

For an initial preservation-oriented implementation, storing raw MIME blobs in the same transactional database can simplify consistency. Separate immutable blob files may become attractive at scale, but then synchronization, renames, references and garbage collection need an explicit recovery protocol.

**Trade-off:** durability has latency and storage costs. **Strongest counterargument:** aggressive synchronous saves can harm typing responsiveness. Batch durable writes on background workers, show save status accurately, and test the user-visible latency; do not claim a save finished before it did.

For **Microsoft 365**, prefer Graph as the new adapter foundation. Microsoft's official documentation, updated **2026-09-04** and checked **2026-10-11**, says Exchange Online EWS disablement begins in **October 2026** and becomes complete in **April 2027**. This is phased retirement, not a claim that every tenant already lost EWS on October 1. The Exchange team's accompanying guidance distinguishes Exchange Online from on-premises Exchange; evaluate the latter separately. [Microsoft retirement documentation](https://learn.microsoft.com/en-us/exchange/clients-and-mobile-in-exchange-online/deprecation-of-ews-exchange-online), [Exchange team guidance](https://techcommunity.microsoft.com/blog/exchange/exchange-online-ews-your-time-is-almost-up/4492361).

The cited Microsoft roadmap also labels remaining Graph parity dates as targets. Do not treat a planned Q4 capability as already delivered. This matters especially if a proposed “local transfer” later expands into faithful server-side import or archive manipulation. [Microsoft parity roadmap](https://learn.microsoft.com/en-us/exchange/clients-and-mobile-in-exchange-online/deprecation-of-ews-exchange-online).

**Recommended first slice:** stable cross-folder reading sessions, multilingual MIME correctness, explicit rule explanations, safe drafts, local search and lossless export. **Strongest argument against the whole recommendation:** much of the visible functionality already exists, and an additional stateful reading mode can confuse users. Sweetbriar earns its place only if it makes these features coherent across providers while preserving native interaction and dependable recovery.

## 5. Risks and open questions

1. **Initial launch and final cessation:** circulation by 1997 and freeware in 2008 are documented. The first release date, last-ever build, cancellation decision and complete maintenance history remain **unverified**. Do not convert the latest recovered release into an exact death date.
2. **3.5 dating:** the package says March 14; the vendor announcement/update says April 24. This may distinguish build and publication, but that explanation is an **inference**.
3. **Unread-session semantics:** the 2005 account is recollection. Exact removal timing, refresh behavior and physical filing on read require a controlled runtime test or additional documentation.
4. **Storage and import:** the native message/settings layouts, crash consistency and preservation of resource forks/attachment metadata remain **unverified**. Import/export plug-ins establish capabilities, not lossless migration.
5. **Threading:** no verified algorithm or conversation-thread feature was recovered. Avoid making a Musashi threading design a Sweetbriar requirement.
6. **Security:** app passwords do not establish at-rest encryption. SSL limitations have documentary/user support, but build-by-build protocol behavior was not audited.
7. **Search engine claims:** “Sherlock-like” is Platt's description. The actual engine, language normalization, index persistence and performance limits remain **unverified**.
8. **Representativeness:** a handful of reviews and personal accounts cannot establish what most users thought. Macworld's criticism and reader response should remain side by side.
9. **Archive reliability:** manuals contain omissions, stale preferences and date inconsistencies. Preserve the packages and distinguish documentation from observed operation.
10. **Modern parity:** current-client tables describe consulted documentation as of 2026-10-11. Exact reading-session behavior and advanced customization need hands-on testing before a product claim that competitors cannot do them.
11. **Code/team fate:** no open-source release, acquisition, transfer or team roster was verified. The current Sonosoft business is not evidence of active Musashi maintenance.
12. **Absolute preservation:** “never lose mail” should become explicit, tested guarantees for named failures and a recoverable backup/export design. A client cannot establish that promise merely by selecting Rust or SQLite.

## 6. Sources

All links below were consulted during this research. Access date: **2026-10-11**. Wayback URLs identify exact captures; document publication dates are stated where established. Download links point to original distribution artifacts, with extracted primary documents linked in the findings.

### Archives and recovered distributions

- [Macintosh Garden: Musashi, HTTP page](http://macintoshgarden.org/apps/musashi). The requested HTTPS endpoint failed during this research.
- [Macintosh Repository: Musashi 3.x](https://www.macintoshrepository.org/903-musashi-3-x).
- [English 68K 3.5 package](http://gardenmirror.oldapplestuff.com/apps/musashi-68k-35-en.sea_.hqx).
- [Japanese 68K 3.5 package](http://gardenmirror.oldapplestuff.com/apps/musashi-68k-35-jp.sea_.hqx).
- [Mac OS X 3.4.1 package](http://gardenmirror.oldapplestuff.com/apps/mu-osx-341.sit_.hqx).
- [PowerPC 3.0.6 package](http://gardenmirror.oldapplestuff.com/apps/musashi-ppc-306.sit_.hqx).

### Vendor and historical announcements

- [Early Musashi product page, capture 1998-12-03](http://web.archive.org/web/19981203090133/http://www.sonosoft.com/musashi/).
- [Early release history, capture 1998-05-25](http://web.archive.org/web/19980525053101/http://www.sonosoft.com/musashi/relnote.html).
- [Vendor introduction/news, capture 2003-12-11](http://web.archive.org/web/20031211053343/http://www.sonosoft.com/index.html).
- [English product page, capture 2003-12-03](http://web.archive.org/web/20031203201821/http://www.sonosoft.com/musashi.html).
- [Japanese product page, capture 2003-12-03](http://web.archive.org/web/20031203195921/http://www.sonosoft.com/jp/musashi.html).
- [Download page, capture 2003-12-03](http://web.archive.org/web/20031203195859/http://www.sonosoft.com/download.html).
- [English registration, capture 2003-12-03](http://web.archive.org/web/20031203202422/http://www.sonosoft.com/register.html).
- [Japanese registration, capture 2003-12-11](http://web.archive.org/web/20031211205640/http://www.sonosoft.com/jp/register.html).
- [Japanese registration form, capture 2003-12-31](http://web.archive.org/web/20031231061350/http://www.sonosoft.com/jp/regform.html).
- [2008 vendor business profile, capture 2008-04-16](http://web.archive.org/web/20080416224742/http://www.sonosoft.com/about).
- [Vendor site-renewal post, dated 2008-03-08](http://web.archive.org/web/20080416224752/http://www.sonosoft.com/archives/9).
- [Vendor Musashi/freeware-intention post, dated 2008-03-08](http://web.archive.org/web/20080416224747/http://www.sonosoft.com/archives/10).
- [Freeware 3.5 announcement, dated 2008-04-24, with April 25 user comment](http://web.archive.org/web/20080526121455/http://www.sonosoft.com/archives/14).
- [Info-Mac/fj.sys.mac archived Musashi 2.0.5 announcement](https://groups.google.com/g/fj.sys.mac/c/qKNBNlStAw4); search-index text was readable, direct page access was unreliable.
- [Impress digest, 1999-01-05: Japanese 3.0](https://internet.watch.impress.co.jp/www/digest/backno/9901/05.htm).
- [Impress digest, 2000-05-09: 3.2](https://internet.watch.impress.co.jp/www/digest/backno/0005/09.htm).
- [Impress digest, 2001-02-26: Musashi-V beta](https://internet.watch.impress.co.jp/www/digest/backno/0102/26.htm). The name is verified; a final “version 5” is not.
- [Impress digest, 2002-06-14: 3.4.1](https://internet.watch.impress.co.jp/www/digest/backno/0206/14.htm).
- [ASCII, 2002-10-18: MacPower's Jaguar-era email comparison contents](https://ascii.jp/elem/000/000/334/334279/). This verifies inclusion of Musashi 3.4.1, not the unseen comparison's findings.

### Reviews, testimony and current business

- [INTERNET magazine, July 1999, email feature, printed pp.198–218](https://iwparchives.jp/wp-content/themes/twentytwelve/bn/pdf/im199907-198-email.pdf); Musashi profile p.217.
- [Preserved Musashi 2.2 review, dated 1998-09-13](https://www.brightblack.net/retro/review-of-musashi-2.2/).
- [Preserved Musashi 3 review, dated 1999-07-17](https://www.brightblack.net/retro/review-of-musashi-3/).
- [Macworld: Mac OS X e-mail guide, 2002-09-30](https://www.macworld.com/article/1001571/email.html).
- [Macworld, October 2002 scan](https://vintageapple.org/macworld/pdf/MacWorld_0210_October_2002.pdf).
- [Macworld, January 2003 scan](https://vintageapple.org/macworld/pdf/MacWorld_0301_January_2003.pdf).
- [Japanese user's Apple Mail/Musashi workflow account, 2005-02-28](https://plaza.rakuten.co.jp/kojim/diary/200502280000/).
- [Current Sonosoft website](https://sonosoft.com/), [company profile](https://sonosoft.com/?page_id=32), [services](https://sonosoft.com/?page_id=34).
- [Vector, 1998-01-22: J-2.1 review and Misono’s developer statement](https://www.vector.co.jp/magazine/softnews/980122/n9801225.html).
- [Vector: Musashi add-ons category](http://www.vector.co.jp/vpack/filearea/mac/net/mail/musashi), [migaModuleCOMPLETE](http://www.vector.co.jp/soft/mac/net/se112846.html).

### Current-client documentation

- [Apple Mail: Smart Mailboxes and incoming organization](https://support.apple.com/en-mide/guide/mail/mlhlp1190/mac).
- [Apple Mail: adding rules](https://support.apple.com/en-lamr/guide/mail/cpmlprefrulesadd/mac).
- [Outlook for Mac: unread filter](https://support.microsoft.com/en-us/outlook/view-only-your-unread-messages-in-outlook-for-mac).
- [Outlook for Mac: feature matrix](https://support.microsoft.com/en-gb/outlook/outlook-for-mac-features?nochrome=true).
- [Gmail: inbox layouts](https://support.google.com/mail/answer/18522?ctx=topic&hl=en&topic=1669050).
- [Spark: Smart Inbox](https://sparkmailapp.com/features/smart_inbox), [templates](https://sparkmailapp.com/teams/templates).
- [Thunderbird: filters](https://support.mozilla.org/en-US/kb/organize-your-messages-using-filters).
- [Mimestream: features](https://mimestream.com/).

### Standards, APIs and Rust/storage candidates

- [RFC 1468: ISO-2022-JP](https://www.rfc-editor.org/rfc/rfc1468.html).
- [RFC 2045: MIME content and transfer encodings](https://www.rfc-editor.org/rfc/rfc2045.html).
- [RFC 2047: MIME encoded headers](https://www.rfc-editor.org/rfc/rfc2047.html).
- [RFC 9051: IMAP4rev2](https://datatracker.ietf.org/doc/html/rfc9051).
- [RFC 6851: IMAP MOVE](https://www.rfc-editor.org/rfc/rfc6851.html).
- [RFC 8621: JMAP Mail](https://datatracker.ietf.org/doc/html/rfc8621).
- [Graph: update message](https://learn.microsoft.com/en-us/graph/api/message-update?view=graph-rest-1.0), [move message](https://learn.microsoft.com/en-us/graph/api/message-move?view=graph-rest-1.0).
- [Gmail API: labels](https://developers.google.com/gmail/api/guides/labels), [messages.modify](https://developers.google.com/gmail/api/reference/rest/v1/users.messages/modify).
- [Exchange Online EWS retirement](https://learn.microsoft.com/en-us/exchange/clients-and-mobile-in-exchange-online/deprecation-of-ews-exchange-online), [Exchange team timeline](https://techcommunity.microsoft.com/blog/exchange/exchange-online-ews-your-time-is-almost-up/4492361).
- [mail-parser](https://docs.rs/mail-parser/latest/mail_parser/).
- [Tantivy](https://docs.rs/tantivy/latest/tantivy/), [tokenizers](https://docs.rs/tantivy/latest/tantivy/tokenizer/index.html).
- [lindera-tantivy](https://docs.rs/lindera-tantivy/latest/lindera_tantivy/).
- [rusqlite](https://docs.rs/rusqlite/latest/rusqlite/).
- [SQLite PRAGMA documentation](https://www.sqlite.org/pragma.html).