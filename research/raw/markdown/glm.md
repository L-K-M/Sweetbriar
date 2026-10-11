<!-- provenance: helper=glm model=zai-coding-plan/glm-5.3 started=2026-10-11T19:50:59+00:00 exit=143 brief=research/briefs/ -->

# Markdown as Sweetbriar's Primary Mail Format — Research Report

*Sweetbriar research assignment, 2026-10-11. All time-sensitive facts were checked against live sources on this date; each carries a citation. Facts vs. opinions are separated throughout; "(unverified)" marks claims I could not confirm.*

---

## 1. TL;DR

- **Writing in Markdown: yes, unconditionally.** It is proven prior art (MailMate has shipped exactly this for years — its manual documents it as "perhaps... its most defining feature"; Mimestream ships Markdown shortcuts; Markdown Here has ~60k stars on GitHub), and it maps cleanly onto `multipart/alternative` per [RFC 2046 §5.1.4](https://www.rfc-editor.org/rfc/rfc2046). This part of the proposal is low-risk and differentiating.
- **Reading normalized-only: no. Reading adaptively normalized: yes.** Convert incoming mail to a canonical CommonMark/GFM-based document and render it with native typography (TextKit 2), but *classify first*: text-like mail renders normalized by default; layout-heavy mail (newsletters, receipts, marketing) renders the sanitized original by default, with one-click switch and per-sender memory. This mirrors what users already demand — Thunderbird users have long relied on per-message HTML toggling add-ons, and MailMate remembers your plain/HTML choice per message.
- **Use Markdown as the interchange and storage format, but a richer AST as the render model.** Plain CommonMark cannot carry `dir`/`lang` (RTL), table semantics, or `cid:` image maps; store CommonMark+GFM plus sidecar metadata, and render from the AST, not from re-parsed Markdown.
- **The Rust ecosystem is ready.** `mail-parser` 0.11.9, `htmd` 0.5.5 (~16 ms to convert a 1.37 MB page on an M4), `html2text` 0.17.2, `comrak` 0.56.0 (652/652 CommonMark, 670/670 GFM), `pulldown-cmark` 0.13.4, `ammonia` 4.2.3, `dom_smoothie` 0.18.2 — all actively maintained as of Sep–Oct 2026.
- **`text/markdown` (RFC 7763) is irrelevant on the wire.** Ten years after registration no mainstream client renders it; even MailMate — its closest ally — uses a private `markup=markdown` parameter on `text/plain` instead, and says so ("MailMate is the only client which can do this"). Send `multipart/alternative` with rendered plain text + generated HTML.
- **Privacy and dark mode by construction** are the strongest reading-side wins: normalized rendering loads zero remote content, needs no WebKit process for most mail, and sidesteps the dark-mode mess that even Apple Mail and Gmail have not fully solved (Mimestream markets "Dark Mode Reformatting" as a feature; Apple built Mail Privacy Protection into the OS in 2021).
- **Keep `format=flowed` (RFC 3676)** for the plain part; it is the standard mechanism for reflowable prose in plain-text clients (which major modern clients are commonly said to emit — unverified here).
- **Biggest risk is fidelity and trust:** a lossy normalized view can misrepresent receipts, legal notices, and phishing cues. Mitigations: immutable originals (already planned), a fast, prominent "Show original", per-sender memory, and *never* defaulting a layout-heavy message to the normalized view.
- **Estimated engineering weight:** the reading pipeline is a multi-quarter effort (classification, table heuristics, RTL/CJK edge cases); the writing pipeline is a few weeks to a credible v1 using comrak/htmd. Sequence writing first.

---

## 2. Findings

### 2.1 Reading: normalize, then render (Q1)

#### 2.1.1 Prior art and outcomes

| Product | What it does | Outcome / evidence |
|---|---|---|
| **Firefox Reader View / Readability.js** | Strips clutter (ads, buttons, background images), then applies Mozilla's own typography: font, size, line-height, width, light/sepia/dark themes, and text-to-speech ([SUMO KB, archived 2023-05-30](https://web.archive.org/web/20230530085329/https://support.mozilla.org/en-US/kb/firefox-reader-view-clutter-free-web-pages)). Readability is the extraction engine ([mozilla/readability README](https://github.com/mozilla/readability)). | Reader modes are now a browser staple; Readability is actively maintained (pushed 2026-08-04, 11.5k stars). The README itself warns extraction is heuristic (`isProbablyReaderable` "is likely to produce both false positives and false negatives") and that output must be sanitized — i.e., the authors treat fidelity as best-effort. |
| **Thunderbird** | Built-in `View ▸ Message Body As ▸ Original HTML / Simple HTML / Plain Text`. Simple HTML "preserves the formatting of HTML emails but blocks JavaScript code and remote content" ([gHacks](https://www.ghacks.net/2010/08/16/switch-between-html-and-plain-text-emails-in-thunderbird/)); confirmed by Mozilla support answers ([support.mozilla.org, 2015](https://support.mozilla.org/en-US/questions/1093890)) and the Toggle HTML add-on description, Jan 2025 ([addons.thunderbird.net](https://addons.thunderbird.net/en-us/thunderbird/addon/toggle-html/)). | Demand for *adaptive* rendering is proven: the most-used answer to "per-message HTML" is the **Allow HTML Temp** add-on, recommended on Mozilla Connect as recently as Sep 2025. A 2024 feature request on the German TB forum even asks for auto-generated plain text "ideally markdown formatted" when a message lacks a plain part — users independently asking for Sweetbriar's model. |
| **MailMate** | Renders *plain-text* mail through its own generated HTML + CSS themes ("Messages are displayed using a standard WebView. HTML is generated for plain text messages and it is styled using CSS"), offers a global **"Prefer Plain Text"** viewer option, and *remembers your alternative choice per message* (`View ▸ Message Body Parts ▸ Next/Previous Alternative`) ([Manual](https://manual.mailmate-app.com/preferences)). Blocks all external references by default in every HTML message ([Manual, Security](https://manual.mailmate-app.com/preferences)). | The closest shipped analogue to "own design, sender-independent typography." Its per-message memory of the plain/HTML choice is exactly the interaction Sweetbriar should copy (opinion). |
| **Mimestream** (2026) | Native macOS Gmail client; markets **"Dark Mode Reformatting"** (reformats received HTML for dark mode), tracker blocking for 75+ services, and Markdown + code formatting in the composer ([mimestream.com](https://mimestream.com/)). | Evidence that (a) reformatting others' HTML is now a marketable feature, and (b) mainstream native clients keep WebKit-grade HTML rendering — they normalize *around* the message, not the message itself. |
| **HEY** | Strips spy pixels and renders original HTML with its own chrome; a 2021 BBC-covered analysis found **two-thirds of emails HEY handled contained hidden trackers** ([BBC, 2021-02-17](https://www.bbc.com/news/technology-56071437)). | HEY normalizes *privacy*, not typography. Its presence on the Can I Email scoreboard (i.e., its CSS support is gradeable, [caniemail.com/clients/hey/](https://www.caniemail.com/clients/hey/)) shows it renders original HTML. |
| **Gmail / Apple Mail** | Both render original HTML; Apple Mail (macOS) scores 288/308 on Can I Email — the best of all clients ([caniemail.com scoreboard](https://www.caniemail.com/)). Gmail offers a per-compose "Plain text mode" ([Gmail Help](https://support.google.com/mail/answer/8260?hl=en&co=GENIE.Platform%3DDesktop)). | The fidelity ceiling users compare against: whatever Sweetbriar shows must be *at least as readable*, and original rendering is the fallback users expect. |
| **Terminal clients** | HTML mail → text via pipelines like `html2text`, whose README explicitly names "the text/plain fallback in HTML e-mails" as a target ([rust-html2text README](https://github.com/jugglerchris/rust-html2text/)); the Rust CLI `himalaya` proves the server-side scope (IMAP/SMTP/JMAP/Gmail API/Microsoft Graph) in pure Rust ([himalaya README](https://github.com/pimalaya/himalaya)). | Normalized reading has been the terminal world's default forever; quality is acceptable for prose, poor for layout (opinion, consistent with html2text's "reasonable job on reasonable HTML" phrasing). |

**(Opinion)** No mainstream 2024–2026 client normalizes message *content* end-to-end. Reader modes normalize web pages; mail clients normalize privacy and chrome. That gap is Sweetbriar's opportunity and its risk: there is no large-scale evidence that users will accept normalized newsletters, and there *is* evidence (Thunderbird add-ons, MailMate options) that users want sender-format control with low-friction toggling.

#### 2.1.2 Feasibility per mail category

Assessment uses the converter inventory in §2.1.3; failure modes are derived from those libraries' documented behavior (tables, "faithful mode," annotation limits) plus the HTML-email reality documented by Can I Email (layout tables are the norm; classic Outlook on Windows supports **no** `display:flex/grid`, `border-radius`, `box-shadow`, `float`, `position`, or `@media` — only table attributes like `cellpadding`/`width`/`bgcolor` — [caniemail.com/clients/outlook/](https://www.caniemail.com/clients/outlook/)).

| Category | Normalized fidelity | Notes & failure modes |
|---|---|---|
| Personal plain mail | **~100%** | Trivial. Fixed-width quoting (`> `), `-- ` signatures, and `format=flowed` reflow are well-specified by [RFC 3676](https://www.rfc-editor.org/rfc/rfc3676). |
| Quoted threads | **High** | AST-level quote attribution beats character-level `>` — this is where normalization *improves* on originals (opinion). Risk: mixed quote markers and Outlook-style "From: … Sent: …" blocks need heuristics. |
| Mailing lists & patches | **High** | Patches are plain text; fenced code preservation is essential. RFC 3676 itself mandates that "hand-aligned text, such as ASCII tables or art, source code" be sent fixed, not flowed — the same rule applies to our converter. |
| Newsletters (Substack et al.) | **Medium** | Article body extracts well (Readability-style heuristics); mastheads, multi-column layouts, and hero images degrade. Substack's plain-text alternative part is usually decent — prefer the sender's own plain part when present. |
| Transactional mail / receipts | **Low–Medium** | Two-column layout tables flatten into confusing prose or broken GFM tables. This is the canonical failure case for html→markdown (htmd's table converter targets `<thead>/<tbody>` structures; nested layout tables come out mangled). **Default these to original.** |
| Marketing mail | **Low** | Layout *is* the content. Normalize only on explicit user request. |
| Calendar invites | **Special-case** | `text/calendar` (iCalendar) should never go through Markdown; render a native event card (Mimestream does this, [mimestream.com](https://mimestream.com/)). |
| Code / preformatted | **High** | Must map `<pre>` → fenced blocks with `hardbreaks`-safe handling; MailMate's "all line breaks are hard line breaks" choice is instructive ([MailMate manual](https://manual.mailmate-app.com/preferences)). |
| RTL and CJK | **Medium, needs AST support** | CommonMark has no direction concept; base direction must be captured from HTML `dir`/`lang` and carried in the AST (see [MDN: `dir`](https://developer.mozilla.org/en-US/docs/Web/HTML/Reference/Global_attributes/dir) — base direction is required for correct bidi, `dir="auto"` is a heuristic). RFC 3676 §10 explicitly warns flowed text "may not be suitable" for RTL scripts. CJK needs UAX #14 line breaking (RFC 3676 cites Annex 14) and `DelSp=yes` handling. |
| Emoji | **High** | Pure Unicode; no converter impact. |
| Inline `cid:` images | **High with metadata** | `cid:` URLs must be preserved and mapped to attachment parts at render time (MailMate uses `cid:` links for inline images, [manual](https://mail.mailmate-app.com/preferences)). Markdown image syntax with `cid:` src is an internal convention; keep the map in sidecar metadata, not in the `.md`. |
| Signatures / disclaimers | **High** | `-- ` separator is standardized by RFC 3676 §4.3; legal disclaimers must never be dropped from the stored original (see §2.1.5). |
| AMP for Email | **Out of scope, by design** | AMP is still live in Gmail (docs updated [2026-09-03](https://developers.google.com/workspace/gmail/ampemail)); support remains limited to Gmail, Yahoo and Mail.ru per 2026 vendor guides ([Mailmodo, 2026-01-27](https://mailmodo.com/guides/amp-email-support/)); Google itself says the AMP part is only usable for 30 days and a full HTML/plain fallback is mandatory ([Gmail testing docs](https://developers.google.com/workspace/gmail/ampemail/testing-dynamic-email)). Can I Email estimates 16.28% client support ([caniemail.com/features/amp/](https://www.caniemail.com/features/amp/)). Normalized rendering shows the HTML/plain fallback — which is what most recipients see anyway. |

**Layout-table vs data-table detection** (engineering proposal, opinion): score each table on (1) explicit `role="presentation"` (the attribute is in email-client support tables, i.e., actually used — [caniemail](https://www.caniemail.com/clients/outlook/)); (2) nesting depth; (3) presence of `<th>`/`<thead>`/`caption`; (4) ratio of cells containing block-level children vs inline text; (5) fixed pixel widths vs `auto`; (6) row/column count regularity. Layout tables → flatten to sequence or paragraphs; data tables → GFM table (htmd's converter handles the well-formed case, [htmd README](https://github.com/letmutex/htmd)). Precedent for heuristic scoring: Readability's link-density scoring ([readability README](https://github.com/mozilla/readability)).

#### 2.1.3 Tooling (Rust preferred)

All versions/dates from the crates.io API, retrieved 2026-10-11.

| Crate | Version (date) | Role | Notes |
|---|---|---|---|
| [comrak](https://crates.io/crates/comrak) | 0.56.0 (2026-10-06) | MD → HTML, MD → MD (formatter) | 652/652 CommonMark 0.31.2, 670/670 GFM; the formatter enables canonical re-serialization ([README](https://github.com/kivikakk/comrak)). My pick for the composer and for canonicalization. |
| [pulldown-cmark](https://crates.io/crates/pulldown-cmark) | 0.13.4 (2026-05-20) | Streaming MD parser | Event parser with source maps, optional SIMD, no unsafe; 172M downloads. Best for incremental rendering and source-range highlighting in the editor. |
| [htmd](https://crates.io/crates/htmd) | 0.5.5 (2026-07-27) | HTML → Markdown | Passes turndown.js's full test suite; GFM table output; **~16 ms for a 1.37 MB page on Apple M4**; "faithful mode" keeps unknown tags as HTML; custom element handlers; built on html5ever ([README](https://github.com/letmutex/htmd)). |
| [html2text](https://crates.io/crates/html2text) | 0.17.2 (2026-10-11) | HTML → terminal-style plain text | html5ever-based; link annotations; explicitly targets the plain-text fallback of HTML email. Ideal for the *plain part* generator and for search text. |
| [html2md](https://crates.io/crates/html2md) | 0.2.17 (2026-08-20) | HTML → Markdown | Older design; htmd is the stronger 2026 choice (opinion). |
| [mail-parser](https://crates.io/crates/mail-parser) | 0.11.9 (2026-09-09) | MIME parse | Zero-copy, lazy decoding, "battle-tested with millions of real-world e-mail messages dating from 1995 until today"; provides RFC 8621 body lists and *converts HTML→text and text→HTML when an alternative is missing* ([README](https://github.com/stalwartlabs/mail-parser)). |
| [html5ever](https://crates.io/crates/html5ever) | 0.40.1 (2026-09-14) | HTML parsing | The spec-compliant foundation used by htmd/html2text. |
| [lol_html](https://crates.io/crates/lol_html) | 3.0.1 (2026-07-29) | Streaming rewriter | For the sanitizer pass over original HTML (block remote URLs, neutralize forms) without building a DOM. |
| [ammonia](https://crates.io/crates/ammonia) | 4.2.3 (2026-10-11) | HTML sanitizer | Whitelist-based; the Rust analogue of the DOMPurify step Readability's README insists on. |
| [dom_smoothie](https://crates.io/crates/dom_smoothie) | 0.18.2 (2026-09-21) | Readability-style extraction | Maintained Rust readability port; the older `readability` crate last shipped 0.3.0 in 2023-12 and looks stale. |
| [syntect](https://crates.io/crates/syntect) | 5.3.0 (2025-09-27) | Code highlighting | For fenced blocks in both reading and writing. |
| [tantivy](https://crates.io/crates/tantivy) | 0.26.2 (2026-09-08) | Full-text search | Index the normalized text (§2.3). |
| [textwrap](https://crates.io/crates/textwrap) | 0.16.4 (2026-09-13) | Line wrapping | For `format=flowed` generation. |

**Pandoc** remains the quality reference (`pandoc -f html -t gfm`, `-f gfm -t html5`), supporting CommonMark/GFM among dozens of formats ([pandoc.org](https://pandoc.org/)); use it in CI as a differential test oracle for Sweetbriar's converters (opinion).

#### 2.1.4 Benefits

- **Privacy/security by construction.** A normalized document has no `<img src="https://tracker…">`, no CSS, no forms, no scripts. Tracking pixels are endemic: HEY found them in two-thirds of mail ([BBC, 2021](https://www.bbc.com/news/technology-56071437)); a study by OMC (an email-intelligence firm) found more than 40% of the emails in its corpus tracked ([WIRED, 2017](https://www.wired.com/story/how-email-open-tracking-quietly-took-over-the-web/)); a Princeton study found ~30% of analyzed emails leaked addresses to third parties ([Wikipedia: Spy pixel, retrieved 2026-10-11](https://en.wikipedia.org/wiki/Spy_pixel)). Apple's response — Mail Privacy Protection, iOS 15+, hides the recipient IP and proxies remote content ([Apple Support](https://support.apple.com/en-us/HT212797)) — validates the threat model but *costs* an image proxy; normalization makes the question moot for normalized mail.
- **Consistent typography, dark mode, accessibility.** The normalized document renders through Sweetbriar's own styles — light/dark, text scaling, VoiceOver — with no sender CSS to fight. Even Mimestream, which keeps original HTML, had to build "Dark Mode Reformatting" ([mimestream.com](https://mimestream.com/)); classic Outlook cannot even respond to `prefers-color-scheme` ([caniemail](https://www.caniemail.com/clients/outlook/)).
- **Speed and memory: most mail can be native.** Plain and normalized mail can render via TextKit 2 — Apple's "next-generation text engine, redesigned for improved correctness, safety, and performance" ([WWDC21, Meet TextKit 2](https://developer.apple.com/videos/play/wwdc2021/10061/)) — with no WebKit process. WKWebView remains for "Show original," locked down: JavaScript off via `defaultWebpagePreferences.allowsContentJavaScript` (the iOS 14+/modern API; older `WKPreferences.javaScriptEnabled` is the legacy path — [Stack Overflow 47037024](https://stackoverflow.com/questions/47037024/ios-wkwebview-swift-javascript-enable), [55654442](https://stackoverflow.com/questions/55654442/swift-wkwebview-interface-with-javascript-is-not-run)); and remote-content blocking enforced by the Rust sanitizer before load. (Quantified per-message WebKit memory numbers: not verified; treat as "a separate out-of-process tab," per the WKWebView-as-lone-Safari-tab framing in [Hacking with Swift's WKWebView guide](https://www.hackingwithswift.com/articles/112/the-ultimate-guide-to-wkwebview).)
- **Better quoting, digests, search.** Quote-aware ASTs give real thread structure; digest views and snippet generation fall out of the same document model; search indexes clean text (§2.3).
- **SwiftUI rendering exists.** [MarkdownUI](https://github.com/gonzalezreal/MarkdownUI) renders Markdown natively in SwiftUI (now in maintenance mode, with its successor **Textual**, "a SwiftUI-native text rendering engine," under active development — [MarkdownUI README](https://github.com/gonzalezreal/MarkdownUI)); Apple's [swift-markdown](https://github.com/swiftlang/swift-markdown) provides a cmark-gfm-based parser with an immutable markup tree ([README](https://github.com/swiftlang/swift-markdown)). So even the UI layer need not depend on the Rust core's representation (opinion: render from the shared AST spec, not from a Swift parser, to keep behavior cross-platform-identical).

#### 2.1.5 Risks

- **Broken newsletters and receipts** — the concrete fidelity analysis in §2.1.2. Mitigation: classification + default-to-original for layout mail.
- **Legal/compliance.** The user must be able to see exactly what was sent; in US litigation, ESI is produced "in a form or forms in which it is ordinarily maintained" ([FRCP 34(b)(2)(E)(ii)](https://www.law.cornell.edu/rules/frcp/rule_34)) — one more reason the immutable original (already planned) is non-negotiable, and why "Show original" must be a first-class, instant affordance, not a debug menu.
- **Phishing.** Normalized rendering *diverges from sender intent*. It cuts both ways: spoofed "PayPal" branding disappears, and real URLs become visible (defense); but users trained on brand cues may miss them, and any divergence between what the user sees and what the original says can be exploited or litigated. Keep DKIM/SPF/deliverability banners attached to the *message*, not to either rendering mode.
- **Branding/forms.** Forms die in normalization (correct for security; bad for "confirm your flight" interactive mail). AMP mail degrades to its HTML/plain fallback — acceptable, since Gmail itself only honors AMP for 30 days ([Google](https://developers.google.com/workspace/gmail/ampemail/testing-dynamic-email)).
- **Engineering gravity.** The table/RTL/CJK edge cases are a treadmill; see §5.

#### 2.1.6 Design options for reading

| | (a) Normalized-only + "original" toggle | (b) Adaptive classify + per-sender memory | (c) Reader-mode toggle (original default) |
|---|---|---|---|
| Consistency | Maximal | High for text mail | Minimal |
| Fidelity risk | High (receipts/newsletters) | Low if classifier is conservative | Minimal |
| Privacy | Maximal | High (defaults still normalized for text mail) | Low unless user opts in |
| User surprise | "My bank statement looks wrong" | Least; matches per-message expectation | Least for mainstream; purists must toggle per message |
| Prior art | MailMate (plain-preferred, per-message memory) | Thunderbird + Allow HTML Temp; MailMate per-message alternative memory | Safari/Firefox Reader |
| Engineering cost | Converter must be near-perfect | Classifier + policy store on top of converter | Cheapest |

**Recommendation: (b)**, with MailMate's per-message memory generalized to per-sender. Details in §4.

---

### 2.2 Writing in Markdown (Q2)

#### 2.2.1 Prior art

- **MailMate** (the deepest implementation): composer is plain-text-only — "a feature... its most defining feature"; optional Markdown is "automatically converted... to HTML which is the (unofficial) standard for rich text emails," with a live preview pane; generated HTML is sent **as an alternative to the plain part** — "the opposite of what most email clients... do"; themes (including per-signature themes), Prism/ASCIIMath/Mermaid bundles, `cid:` inline images, and the twist that **all line breaks are hard breaks** for plain-text-reader compatibility. If HTML generation is off, it sends `Content-Type: text/plain; format=flowed; markup=markdown` and admits "MailMate is the only client which can do this" ([MailMate manual: Introduction, Compose, Preferences](https://manual.mailmate-app.com/preferences)).
- **Mimestream**: "Markdown and Code — express yourself clearly with markdown formatting shortcuts and `code` blocks" ([mimestream.com](https://mimestream.com/)) — shorthand-style formatting in an otherwise standard composer.
- **Markdown Here**: Chrome/Firefox/Thunderbird extension converting Markdown to rich text *in situ* in web composers; 60,261 stars, repo active as of 2025-08-22 ([GitHub API](https://api.github.com/repos/adam-p/markdown-here)). Demonstrates long-standing demand from the *sender* side.
- **macdown** (open-source macOS Markdown editor, AppKit; 9.8k stars, last push 2023-07-10 per [GitHub API](https://api.github.com/repos/MacDownApp/macdown)) and **MarkdownUI/Textual** (§2.1.4) show both the AppKit and SwiftUI rendering paths for live-styled editing. (Obsidian/Bear/iA Writer-style live preview is well known but not individually verified here.)

#### 2.2.2 What to send

The correct wire format is `multipart/alternative` with parts ordered plainest-first: RFC 2046 §5.1.4 — alternatives appear "in an order of increasing faithfulness to the original content... the best choice is the LAST part," i.e., `text/plain` then `text/html` ([RFC 2046](https://www.rfc-editor.org/rfc/rfc2046)).

- **Plain part: rendered flowing prose, not raw Markdown (opinion).** Raw Markdown in the plain part is readable to us, and MailMate's choice shows it's viable for a geek audience; but `*bullets*` and `# headings` confuse mainstream plain-text readers (Thunderbird, MailMate users with "Prefer Plain Text", every terminal client). Generating proper wrapped text with **`format=flowed`** — RFC 3676 exists precisely to make prose reflow gracefully ("embarrassing line wrap" is the problem it solves; space-stuffing, `DelSp`, 78-char generation rules all specified; sig separator `-- ` standardized).
- **HTML part: generated, email-safe, table-light.** Constraints from the receiving end, per Can I Email's data (client pages + [JSON API](https://www.caniemail.com/api/data.json), retrieved 2026-10-11): classic Outlook (Word engine) supports **no** flex/grid/border-radius/box-shadow/`@media`/`position`/CSS variables and only table-era attributes; Gmail webmail supports flex, float, `<table>` and class selectors, but `<style>` and `@media` are only *partial*, and `@import` and `position` are unsupported; Outlook.com and Outlook for Mac support the modern set; Apple Mail macOS leads at 288/308 ([scoreboard](https://www.caniemail.com/)). Markdown's feature set (headings, emphasis, lists, code, blockquotes, images, GFM tables/strikethrough/task lists — [GFM spec](https://github.github.com/gfm/)) maps almost 1:1 onto the *intersection* of client support: inline styles, simple `<table>` for GFM tables, `bgcolor`-friendly colors, no layout tables needed because the content is linear. This is the rare case where generating email HTML is *easy* (opinion).
- **`text/markdown` (RFC 7763) in practice: no.** The RFC registered the type in March 2016 with a `variant` parameter, a `preview-type` Content-Disposition parameter, and even reserved the identifiers "Standard/Common/Markdown" because they were contested ([RFC 7763](https://www.rfc-editor.org/rfc/rfc7763)). Ten years on, no mainstream client renders it — the RFC itself notes receivers "SHOULD only accept Markdown variants that they explicitly know about," which is why even MailMate ships a private `markup=markdown` parameter instead. Sweetbriar should *accept* `text/markdown` attachments and render them, and may mirror MailMate's parameter as an opt-in ("Include Markdown source"), but must not depend on it.
- **Interoperability matrix (opinion, grounded in the caniemail data above):** Outlook-Word renders the simple HTML acceptably (it was designed for exactly this subset); Gmail webmail keeps inline styles and typically preserves structure (its historical aggressiveness toward `<style>` blocks is the reason to inline everything); Apple Mail renders essentially perfectly; plain-text clients see the flowed prose; MailMate users see your Markdown-ish prose in their preferred plain view.

#### 2.2.3 Editing UX

- **Raw Markdown with live styling (WYSIWYM) is the sweet spot (opinion).** MailMate's optional live preview pane proves the model ([manual](https://manual.mailmate-app.com/compose)); pulldown-cmark's source maps enable inline styled rendering with selectable source text ([README](https://github.com/raphlinus/pulldown-cmark)). A full WYSIWYG TextKit 2 editor (like the one Mimestream implies with "shortcuts") forfeits "the Markdown is the source of truth" property that makes drafts diffable, forwardable, and searchable.
- **Pasting HTML** → convert to Markdown in the paste pipeline (htmd with a "faithful mode" boundary: unrecognized constructs paste as raw HTML islands that the HTML part can carry verbatim — [htmd README](https://github.com/letmutex/htmd)).
- **Inline images**: Markdown `![](cid:…)` bound to attachments, exactly MailMate's mechanism ([manual](https://mail.mailmate-app.com/preferences)).
- **Tables/code blocks**: GFM tables; fenced code with syntect highlighting, optionally pre-rendered like MailMate's Prism bundle for recipients whose clients can't style code ([manual](https://manual.mailmate-app.com/preferences)).
- **Signatures**: per-identity; optional HTML variant (raw HTML signature injected into the HTML part) as MailMate does for company-policy signatures, with its warning that this "easily doubles the size of each email" ([manual](https://manual.mailmate-app.com/compose)).

#### 2.2.4 Replying and forwarding

- **Reply**: build the quote from the *normalized AST* as a Markdown blockquote with an attribution line; MailMate's alternative — embed the sender's original HTML untouched and drop it if the user edits the quote — is a proven design worth copying ("The editor shows you a plain text variant... if you do try to edit this then MailMate offers you to drop the corresponding HTML," [manual](https://manual.mailmate-app.com/preferences)). Offer both: "quote as Markdown" (default) and "quote original formatting."
- **Forward**: attach the untouched original as `message/rfc822` (the faithful path), or re-send normalized content inline if the user chooses. The immutable store guarantees the faithful path is always available.

---

### 2.3 Data model (Q3)

- **Originals immutable.** Store raw bytes (`.eml`) append-only; MailMate uses `.eml` internally with a Spotlight plugin for indexing ([manual](https://manual.mailmate-app.com/preferences)). This is also the e-discovery-safe choice given FRCP 34's "ordinarily maintained" standard ([Cornell LII](https://www.law.cornell.edu/rules/frcp/rule_34)).
- **Normalized form: derived and cached, not authoritative.** Regenerable at any time from the original. Cache it with a **converter version stamp** (like a schema migration level): bump → re-derive lazily or in background. The canonical serialization is CommonMark+GFM (comrak's formatter can canonicalize on write — [README](https://github.com/kivikakk/comrak)), plus a sidecar (or fenced/HTML-island metadata) for: per-block `dir`/`lang` (from HTML `dir`, per [MDN](https://developer.mozilla.org/en-US/docs/Web/HTML/Reference/Global_attributes/dir)), `cid:` map, table kind, quote-attribution structure, and classification results.
- **Per-message and per-sender rendering choice.** A small policy store: `sender → default mode`, `message-id → override` (MailMate's per-message memory, generalized; [manual](https://manual.mailmate-app.com/preferences)). Toggle events write here; "remember for this sender" is one click.
- **Search over normalized text.** Index the normalized document (plus original plain parts and decoded attachments' text) in Tantivy ([crates.io](https://crates.io/crates/tantivy)); this yields clean snippets without `>` noise and enables "search in what you actually read" semantics. MailMate's searchable-decrypted-messages tradeoff (indexing decrypted text writes plaintext into database files — [manual](https://manual.mailmate-app.com/preferences)) is the privacy caveat to document for encrypted mail.

---

## 3. Options compared

### 3.1 Reading pipeline

| Dimension | Sanitized-original only (current plan) | Normalized-only (+toggle) | **Adaptive (recommended)** |
|---|---|---|---|
| WKWebView usage | Every HTML message | Only "Show original" | Layout-heavy classes only |
| Privacy | Good (JS off, remote blocked) | Best by construction | Best-for-text, good-for-layout |
| Dark mode / a11y | Sender-dependent (hard) | Full control | Full control for text mail |
| Speed/memory | WebKit process per view | Native text view | Mostly native |
| Fidelity risk | None | Highest | Lowest |
| Precedent | Mimestream | MailMate (for plain) | Thunderbird+ATH, MailMate memory |

### 3.2 Writing pipeline

| Dimension | Plain-only composer | WYSIWYG composer | **Markdown/WYSIWYM composer (recommended)** |
|---|---|---|---|
| Output | `text/plain` | `multipart/alternative` | `multipart/alternative` (+ optional `markup=markdown`) |
| Plain part | source | generated | generated, `format=flowed` |
| Learning curve | None | None | Small (Mimestream-style shortcuts hide it) |
| Drafts as data | Trivial | Opaque | Markdown source, diffable |
| Precedent | Mailsmith lineage | Apple Mail, Gmail | **MailMate, Mimestream, Markdown Here** |

### 3.3 Storage of the normalized form

| | Store nothing (derive on open) | **Cache + version stamp (recommended)** | Store as primary |
|---|---|---|---|
| Latency | Re-convert each open | Convert once | Convert once |
| Consistency | Always current converter | Current after migration | Risk of drift from original |
| Disk | Index only | `.md` + sidecar (~10–30% of body size, estimate) | Duplicates mail store |

---

## 4. Recommendations for Sweetbriar

1. **Adopt Markdown end-to-end as the *canonical internal representation*, not as the universal *default view*.** Concretely: every message gets a normalized CommonMark+GFM document (with metadata sidecar), every composer writes Markdown, and the *view* is chosen adaptively (option (b) from §2.1.6).
2. **Classifier defaults (v1, opinion):** normalized for plain-text mail (always), quoted threads, patches, notifications, and any HTML message whose conversion survives a fidelity gate (e.g., table-flatten events below threshold, no `<form>`, text density above X); sanitized-original for newsletters/marketing/receipts/calendar/AMP fallback. One-click toggle both ways; remember per sender; "Show original" always available and instant.
3. **Rendering:** TextKit 2 for normalized documents (AttributedString from the AST; SwiftUI components informed by MarkdownUI/Textual); WKWebView (JS off via `allowsContentJavaScript`, remote content blocked in the Rust sanitizer) only for original views. This makes the fast path the private, dark-mode-perfect, tracker-free path — the exact columns in Sweetbriar's goals.
4. **Composer:** WYSIWYM Markdown on TextKit 2 with live styling driven by pulldown-cmark/comrak source maps; paste converts HTML→Markdown (htmd); `cid:` inline images; GFM tables; syntect highlighting. Send `multipart/alternative` (plain flowed first, email-safe generated HTML last, per RFC 2046 §5.1.4). Optional per-identity "include Markdown source" (MailMate-compatible `markup=markdown`, and/or a `text/markdown` attachment for the two of us who care).
5. **Replies/forwarding:** quote-from-AST as Markdown blockquote (default) with "keep original formatting" fallback à la MailMate; forward as untouched `message/rfc822`.
6. **Data model:** immutable originals; normalized cache with converter versioning; per-sender/per-message policy store; Tantivy index over normalized text.
7. **Testing:** differential corpus harness (real-world mail sets — mail-parser's "millions of messages since 1995" claim is the bar — with pandoc as oracle and golden-file render snapshots); telemetry-free local metrics for the fidelity gate.

**Trade-offs accepted:** a second rendering path to maintain forever; classifier mistakes (mitigated by memory + toggle); some senders' intent invisible by default (mitigated by conservative defaults); composer parity work vs. WYSIWYG competitors (mitigated by shortcuts + live styling).

**Strongest counter-argument:** *This is a decade-long fidelity war disguised as a feature.* Mail is the world's most hostile HTML corpus (Outlook's Word engine still lacking flex/grid in 2026 per [caniemail](https://www.caniemail.com/clients/outlook/), AMP's 30-day half-life, layout tables everywhere), and every normalized rendering that ever misleads a user — a receipt total, a legal disclaimer, a phishing cue — becomes "Sweetbriar lost my mail's meaning." Thunderbird, with 20+ years of manpower, still renders originals and delegates adaptation to an add-on. The honest reply: that is why the recommendation is *adaptive with conservative defaults and instant originals*, and why the immutable store plus versioned cache makes the normalized layer a pure, discardable cache — if classification is ever wrong, nothing is lost. If Sweetbriar cannot staff the converter treadmill, fall back to option (c) (reader-mode toggle) with the same internals; 80% of the benefit, 20% of the exposure.

**Fit for both audiences:** plain-text purists (Mailsmith/Claris Emailer/MailMate refugees) get: always-plain rendering, flowed prose, Markdown source of truth, no remote content ever, `markup=markdown` interop with MailMate peers. Mainstream users get: newsletters that look like newsletters, dark mode everywhere, fast native rendering, and a composer that feels like Notes with superpowers (opinion).

---

## 5. Risks and open questions

- **Classifier quality is unproven at scale.** No public dataset of "did normalization preserve this mail's meaning?" exists (to my knowledge — unverified). Build the corpus harness before the feature flag ships.
- **RTL/CJK correctness** needs the AST metadata design *before* v1 storage format freezes; `dir` per block at minimum ([MDN](https://developer.mozilla.org/en-US/docs/Web/HTML/Reference/Global_attributes/dir)); RFC 3676's own i18n caveats apply to the plain part.
- **AMP trajectory:** still live in Gmail as of 2026-09-03 with narrow support ([Google](https://developers.google.com/workspace/gmail/ampemail), [Mailmodo](https://mailmodo.com/guides/amp-email-support/)); monitor for retirement — it would remove a whole failure class.
- **Outlook "new" vs "classic":** Can I Email tracks them separately; if classic Outlook's Word engine ever dies, email-safe HTML constraints loosen and the generated HTML can get prettier.
- **Encrypted mail:** normalized caching of decrypted content has the MailMate-documented privacy cost ([manual](https://manual.mailmate-app.com/preferences)); default to derive-on-open for encrypted mail.
- **Verification gaps in this report:** Safari Reader specifics; Superhuman/Spark display options; Obsidian/Bear/iA Writer live-preview details; Gmail's exact `<style>`-stripping behavior (asserted from caniemail data and common practice, not from a Google statement); WKWebView per-instance memory figures. All marked or omitted above.
- **Effort sizing (estimate, opinion):** composer + wire format: ~4–6 weeks to v1. Reading pipeline: fidelity gate + table heuristics + policy UI: one engineer-year for "good," ongoing treadmill after that.

---

## 6. Sources (consulted 2026-10-11)

**RFCs & law:** [RFC 2046 (multipart/alternative §5.1.4)](https://www.rfc-editor.org/rfc/rfc2046) · [RFC 3676 (format=flowed)](https://www.rfc-editor.org/rfc/rfc3676) · [RFC 7763 (text/markdown)](https://www.rfc-editor.org/rfc/rfc7763) · [FRCP Rule 34, Cornell LII](https://www.law.cornell.edu/rules/frcp/rule_34)

**Clients & prior art:** [MailMate manual — Introduction](https://manual.mailmate-app.com/) · [Compose](https://manual.mailmate-app.com/compose) · [Preferences](https://manual.mailmate-app.com/preferences) · [freron.com](https://freron.com/) · [Firefox Reader View KB (Wayback 2023-05-30)](https://web.archive.org/web/20230530085329/https://support.mozilla.org/en-US/kb/firefox-reader-view-clutter-free-web-pages) · [mozilla/readability](https://github.com/mozilla/readability) · [gHacks: Thunderbird HTML/plain switching](https://www.ghacks.net/2010/08/16/switch-between-html-and-plain-text-emails-in-thunderbird/) · [SUMO question 1093890](https://support.mozilla.org/en-US/questions/1093890) · [Toggle HTML add-on](https://addons.thunderbird.net/en-us/thunderbird/addon/toggle-html/) · [Mozilla Connect: quick-toggle idea](https://connect.mozilla.org/t5/ideas/quick-toggle-between-plain-text-and-html-view-in-thunderbird/idi-p/24922) · [Mimestream](https://mimestream.com/) · [Mimestream Help](https://mimestream.com/help/) · [Gmail: Format your messages (plain text mode)](https://support.google.com/mail/answer/8260?hl=en&co=GENIE.Platform%3DDesktop) · [Gmail AMP for Email (2026-09-03)](https://developers.google.com/workspace/gmail/ampemail) · [Gmail AMP testing/fallback rules](https://developers.google.com/workspace/gmail/ampemail/testing-dynamic-email) · [Mailmodo: AMP support 2026](https://mailmodo.com/guides/amp-email-support/) · [adam-p/markdown-here](https://github.com/adam-p/markdown-here) · [MacDown](https://github.com/MacDownApp/macdown) · [himalaya](https://github.com/pimalaya/himalaya)

**Rendering support & privacy:** [Can I Email — home/scoreboard](https://www.caniemail.com/) · [Outlook client page](https://www.caniemail.com/clients/outlook/) · [AMP feature page](https://www.caniemail.com/features/amp/) · [HEY client page](https://www.caniemail.com/clients/hey/) · [Apple: Mail Privacy Protection / remote content](https://support.apple.com/en-us/HT212797) · [BBC: spy pixels endemic (2021)](https://www.bbc.com/news/technology-56071437) · [WIRED: email open tracking (2017)](https://www.wired.com/story/how-email-open-tracking-quietly-took-over-the-web/) · [Wikipedia: Spy pixel](https://en.wikipedia.org/wiki/Spy_pixel)

**Apple platform:** [WWDC21: Meet TextKit 2](https://developer.apple.com/videos/play/wwdc2021/10061/) · [Hacking with Swift: Ultimate Guide to WKWebView](https://www.hackingwithswift.com/articles/112/the-ultimate-guide-to-wkwebview) · [Stack Overflow 47037024 (allowsContentJavaScript)](https://stackoverflow.com/questions/47037024/ios-wkwebview-swift-javascript-enable) · [Stack Overflow 55654442](https://stackoverflow.com/questions/55654442/swift-wkwebview-interface-with-javascript-is-not-run) · [MarkdownUI](https://github.com/gonzalezreal/MarkdownUI) · [swift-markdown](https://github.com/swiftlang/swift-markdown)

**Rust crates (versions via crates.io API, 2026-10-11):** [comrak](https://crates.io/crates/comrak) ([README](https://github.com/kivikakk/comrak)) · [pulldown-cmark](https://crates.io/crates/pulldown-cmark) ([README](https://github.com/raphlinus/pulldown-cmark)) · [htmd](https://crates.io/crates/htmd) ([README](https://github.com/letmutex/htmd)) · [html2text](https://crates.io/crates/html2text) ([README](https://github.com/jugglerchris/rust-html2text/)) · [html2md](https://crates.io/crates/html2md) · [mail-parser](https://crates.io/crates/mail-parser) ([README](https://github.com/stalwartlabs/mail-parser)) · [html5ever](https://crates.io/crates/html5ever) · [lol_html](https://crates.io/crates/lol_html) · [ammonia](https://crates.io/crates/ammonia) · [readability](https://crates.io/crates/readability) · [dom_smoothie](https://crates.io/crates/dom_smoothie) · [syntect](https://crates.io/crates/syntect) · [tantivy](https://crates.io/crates/tantivy) · [scraper](https://crates.io/crates/scraper) · [textwrap](https://crates.io/crates/textwrap)

**Specifications & reference tools:** [CommonMark](https://commonmark.org/) · [GitHub Flavored Markdown Spec](https://github.github.com/gfm/) · [Pandoc](https://pandoc.org/) · [MDN: `dir` global attribute](https://developer.mozilla.org/en-US/docs/Web/HTML/Reference/Global_attributes/dir)

*Search performed via Brave Search (`search.brave.com`) where noted; DuckDuckGo and Bing were unusable from this environment (bot challenges / poor relevance).*
