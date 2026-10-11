<!-- provenance: helper=codex model=gpt-6.1-sol started=2026-10-11T19:50:58+00:00 exit=0 brief=research/briefs/ -->

# Markdown as Sweetbriar’s primary mail format

**Research date: 2026-10-11.** This report distinguishes documented facts from design recommendations and estimated failure risks. Product capabilities, API documentation and dependency status were checked on this date; release and publication dates are given where available. No conversion benchmarks or recipient-client tests were run for this assignment. User accounts are anecdotes, not representative satisfaction measurements.

Apple’s compatibility page, published **2026-09-14**, identifies **macOS 27 Golden Gate** as the latest major release and specifies Apple-silicon Macs. The macOS recommendations below use current documentation, including WWDC26 TextKit changes. [Apple compatibility](https://support.apple.com/en-ca/127255), [WWDC26 TextKit](https://developer.apple.com/videos/play/wwdc2026/370/)

## 1. TL;DR

- **Use Markdown as a primary authoring format, while keeping plain-text composition first-class.** MailMate demonstrates that plain-text editing can produce useful HTML mail, previews and themed output. [MailMate introduction](https://manual.mailmate-app.com/introduction)
- **Use a richer document AST for normalized reading.** GFM tables have a single header row and inline cell content; that is too narrow for all received-mail structure. Markdown should be an import/export format, not the universal stored representation. [GFM tables](https://github.github.com/gfm/#tables-extension-)
- **Choose option (b): conservative adaptive rendering.** Normalize simple correspondence; default complex or uncertain messages to sanitized sender layout. Keep a prominent one-click switch and explicit per-message/per-sender preferences.
- **Never interpret ordinary incoming `text/plain` as Markdown automatically.** A 2025 MailMate user report describes dates becoming sequential list numbers. The underlying behavior follows Markdown list semantics. [Reported incident](https://www.mail-archive.com/mailmate%40lists.freron.com/msg18490.html), [CommonMark lists](https://spec.commonmark.org/0.31.2/#lists)
- **Send conventional `multipart/alternative`: rendered `text/plain` first, generated `text/html` second.** Offer literal Markdown as a deliberate compatibility preference. `text/markdown` is useful for interchange but does not establish universal email-client support. [RFC 2046](https://www.rfc-editor.org/rfc/rfc2046), [RFC 7763](https://www.rfc-editor.org/rfc/rfc7763)
- **Normalization improves control; security comes from an enforced policy.** Markdown permits image links and raw HTML, and Readability explicitly does not sanitize its output. Both reading modes need resource blocking and sanitization. [GFM](https://github.github.com/gfm/), [Readability security](https://github.com/mozilla/readability)
- **Use native text views for the supported document subset.** Current TextKit supports viewport-oriented rendering and new extension hooks; native rendering of “most mail,” or a particular memory saving, remains an unmeasured hypothesis. [Apple TextKit](https://developer.apple.com/videos/play/wwdc2026/370/)
- **The strongest objection is trust:** a pleasant transformation can conceal a changed amount, date or qualification. Immutable originals protect the archive; conservative defaults, visible provenance and fidelity tests protect the reader.

## 2. Findings

### 2.1 What “Markdown as the primary format” should mean

There are three different proposals here: Markdown as the composer’s source, Markdown as the reader’s normalization language, and Markdown as the mail archive. The first is well supported by existing products. The second is useful for a subset of messages. The third should be rejected within Sweetbriar’s stated immutable-original design.

**The steelman:** most correspondence benefits from the recipient’s typography rather than the sender’s font choices. A shared semantic representation could make quotation handling, accessibility, search, copying and digest views consistent across macOS and Linux. Markdown provides a readable, inspectable authoring language, and an HTML projection gives recipients familiar rich text. MailMate documents this composition approach; Safari Reader demonstrates recipient-controlled fonts, backgrounds and sizing with an explicit return to the original page. [MailMate](https://manual.mailmate-app.com/introduction), [Safari Reader](https://support.apple.com/en-mide/guide/safari/sfri32632/mac)

The important limit is **document meaning versus presentation**. Removing a decorative font is different from removing the relationship between an invoice item and its amount. GFM’s pipe tables cannot directly express arbitrary cell spans, multiple header associations or block content within cells. Raw HTML can carry extra structure inside Markdown, but then Markdown becomes a wrapper around the original complexity. [GFM table model](https://github.github.com/gfm/#tables-extension-), [WAI table relationships](https://www.w3.org/WAI/tutorials/tables/)

**Recommendation:** define a versioned Rust `MailDocument` AST. Include paragraphs, headings, inline emphasis, explicit line breaks, lists, literal/preformatted blocks, nested quotes, links, image references, tables with spans/header associations, language/direction metadata, and provenance back to MIME parts. Treat unsupported structure as a reason to retain sender layout. Do not require HTML → Markdown → HTML round trips before display. Decode supplied `format=flowed` according to its MIME rules before creating literal text nodes; this is not Markdown interpretation. [RFC 3676](https://www.rfc-editor.org/rfc/rfc3676)

Pandoc provides an architectural precedent: readers produce an intermediate AST and writers produce output formats. Its own manual cautions that conversion loses information when the intermediate model is less expressive than its input, including complex tables. A Sweetbriar AST needs the same honest boundary. [Pandoc conversion model](https://pandoc.org/MANUAL.html)

A declared `text/markdown` part can be parsed with a supported profile; unknown variants should remain literal. Interpreting ordinary plain text as Markdown should require an explicit user action. The media type’s optional variant is only a hint. [RFC 7763](https://www.rfc-editor.org/rfc/rfc7763)

Keep four operations separate:

1. **MIME selection:** identify supplied alternatives, related resources and specialized parts.
2. **Sanitization:** enforce permitted tags, attributes, URLs, CSS and resource access.
3. **Semantic normalization:** preserve content and relationships while replacing presentation.
4. **Article extraction:** optionally select an article and suppress surrounding material.

The normal reading view should perform the first three. Article extraction should be a separate, reversible newsletter feature. An unsubscribe link, warning or disclaimer must not disappear merely because an algorithm considers it boilerplate. Mozilla documents Readability’s false-positive/false-negative eligibility heuristic and its separate sanitization requirement. [Readability API and security](https://github.com/mozilla/readability)

### 2.2 Prior art: what it proves, and what it does not

The following is the documented position **as checked 2026-10-11**, except where a historical entry is explicitly dated.

| Product or approach | Verified behavior | Implication for Sweetbriar |
|---|---|---|
| Safari Reader / Firefox Reader View | Safari offers article-only reading and typography controls; Firefox offers a reader view with presentation controls. Eligibility is selective. [Apple](https://support.apple.com/en-mide/guide/safari/sfri32632/mac), [Mozilla](https://support.mozilla.org/en-US/kb/firefox-reader-view-clutter-free-web-pages) | Strong interaction precedent; article extraction is not a general email-fidelity guarantee. |
| Thunderbird | Offers Original HTML, Simple HTML and Plain Text choices. Its sanitized HTML implementation uses `nsTreeSanitizer`; this is not an HTML-to-Markdown pipeline. [Toggle HTML documentation](https://services.addons.thunderbird.net/EN-US/thunderbird/addon/toggle-html/), [Thunderbird source](https://raw.githubusercontent.com/mozilla/releases-comm-central/master/mailnews/mime/src/mimethsa.cpp) | Existing demand for simplified display; retain a readily accessible alternative. |
| Classic Outlook | Supports reading standard mail as plain text, with an InfoBar action to display HTML again. [Microsoft Support](https://support.microsoft.com/en-us/outlook/read-email-messages-in-plain-text-in-classic-outlook) | A mainstream precedent for reversible presentation preferences. |
| MailMate | Can prefer a supplied plain-text alternative and remember an individual message’s selected alternative. Its viewer converts even plain text into HTML. [Preferences](https://manual.mailmate-app.com/preferences), [Viewer conversion](https://github.com/mailmate/mailmate_manual/wiki/Bundles) | Uniform rendering technology does not mean all incoming HTML becomes Markdown. |
| Terminal clients | NeoMutt supports external MIME viewers through mailcap/auto-view. A converter can feed a terminal pager. [NeoMutt MIME handling](https://docs.neomutt.org/howto/mime.html) | Text projections are practical; users knowingly accept layout loss. |
| HEY | Documents tracker stripping/proxying and a newsletter Feed. Its developer’s **2021-05-20** account discusses isolating and scaling HTML newsletters, including styles and tables. [Privacy](https://www.hey.com/spy-trackers/), [Feed implementation account](https://world.hey.com/dhh/speeding-up-hey-s-the-feed-82e4d2ee) | Privacy and digest presentation can coexist with retained HTML. |
| Superhuman | Documents rich-text controls and selected-text “Quick Quote.” [Formatting](https://help.superhuman.com/hc/en-us/articles/46005721681165-Formatting-Text), [Quick Quote](https://help.superhuman.com/hc/en-us/articles/46005692763661-Quick-Quote) | Evidence for composition and quotation UX; universal incoming normalization is **unverified**. |
| Mimestream | Advertises Markdown formatting shortcuts and code blocks; its guide describes a formatted composer. [Product](https://mimestream.com/), [Formatting guide](https://mimestream.com/help/user-guide/formatting) | Shortcuts do not establish Markdown storage or incoming HTML normalization; those are **unverified**. |
| Spark | Its **2019-03-01** announcement documents rich-text composing. [Spark announcement](https://sparkmailapp.com/blog/introducing-rich-text-formatting-in-spark) | Evidence for mainstream formatting controls; an incoming Markdown normalization pipeline is **unverified**. |
| Newsletterss / Newsletter Reader | Its App Store entry dated **2024-05-21** describes an automatic reader mode that opens supported posts in an in-app browser. A **2024-04-24** entry notes newsletter dark-mode fixes. [Version history](https://apps.apple.com/gb/app/newsletter-reader/id1549306009) | Relevant recent, newsletter-specific prior art; it does not prove general mailbox conversion. |

For terminal reading, `w3m -dump` is an HTML-to-terminal projection, while Pandoc can convert HTML to GFM. These are useful reference outputs for evaluation, not proof of semantic equivalence or a reason to ship subprocess converters in the shared core. [w3m manual](https://raw.githubusercontent.com/tats/w3m/master/doc/w3m.1), [Pandoc formats](https://pandoc.org/MANUAL.html)

**What users thought:** the evidence shows conflicting needs. A Thunderbird user on **2023-09-21** wanted the simplified/plain-text view back. Another, on **2024-05-26**, reported that Simple HTML gave readable text but missing images, while Original HTML restored images with tiny text. A **2024** request specifically wanted HTML-to-Markdown viewing because supplied plain-text alternatives were sometimes poor. [Preference for simplification](https://www.reddit.com/r/Thunderbird/comments/16o9k3p/), [Image/typography trade-off](https://www.reddit.com/r/Thunderbird/comments/1d0up64/), [Normalization request](https://www.reddit.com/r/Thunderbird/comments/1f1lppe/)

MailMate’s **2025-02-18** discussion contains both appreciation for Markdown composition and adjustable fonts, and refusal to adopt the app because Markdown interferes with natural writing and a visual editor is preferred. These are direct user comments, not market-share or satisfaction statistics. [MailMate user discussion](https://mjtsai.com/blog/2025/02/18/mailmate-2-0-beta/)

The reviewed evidence supports **choice, predictability and readable typography**. It does not establish that users generally want every email transformed, or that a 2024–2026 general-purpose client has solved lossless HTML-to-Markdown reading.

### 2.3 Feasibility by message category

These are **engineering estimates**, not measured converter scores. “Low” means semantics should survive a supported conversion; “medium” requires specific structure handling; “high” means meaning or usability can depend on information outside a Markdown subset. Visual fidelity is a separate concern: normalization intentionally changes appearance.

| Mail category | Estimated loss and failure modes | Proposed treatment |
|---|---|---|
| Personal plain text | Low if literal; potentially high if punctuation becomes Markdown syntax. Hard breaks and spaces may be intentional. [CommonMark rules](https://spec.commonmark.org/0.31.2/) | Native literal text, optional visual soft wrapping, no inferred Markdown. |
| Simple personal HTML | Low semantic loss; modest visual change if composed of paragraphs, emphasis, links and lists. | Normalize when all significant structures map cleanly; preserve link targets and hard breaks. |
| Quoted threads | Medium: quote attribution, nesting and interleaved replies can be misclassified or collapsed. MailMate itself separates quoted plain text from Markdown interpretation. [MailMate release notes](https://updates.mailmate-app.com/release_notes_archive/MailMate_r5936.html) | Typed quote nodes with attribution; folding is reversible; never deduplicate the stored body. |
| Mailing lists and digests | Low–medium: separators, footers and individual messages can be mistaken for formatting or junk. MIME also defines `multipart/digest`. [RFC 2046](https://www.rfc-editor.org/rfc/rfc2046) | Preserve list material; retain nested message boundaries rather than concatenate an archive into one Markdown string. |
| Patches | Critical if whitespace changes. Kernel guidance explicitly forbids automatic wrapping and `format=flowed` for patches. [Kernel guidance](https://docs.kernel.org/process/email-clients.html) | Literal monospace reading and byte-preserving copy/export; dedicated plain-text sending mode. |
| Prose newsletters, including Substack-like mail | Medium semantic risk, high intentional layout change: extraction can omit subscription information, captions or secondary items. HTML newsletters contain styles, images and tables. [HEY implementation account](https://world.hey.com/dhh/speeding-up-hey-s-the-feed-82e4d2ee) | Offer normalized reading after structural checks; article extraction is opt-in. Multi-story newsletters are less safe candidates. |
| Transactional mail and receipts | Medium–high: an amount can detach from its label; totals can disappear with layout cleanup. Mailgun’s billing fixture mixes presentation tables with an item/amount table. [Billing HTML](https://github.com/mailgun/transactional-email-templates/blob/master/templates/billing.html) | Preserve data grids; default to sender layout when classification or ordering is uncertain. |
| Marketing mail | High visual loss; potentially high semantic loss for image-based offers, coupon codes and button destinations. | Default complex campaigns to sender layout. A text projection must retain every action link and image placeholder. |
| Calendar invitations | Critical functional loss if flattened into prose. iMIP uses `text/calendar` and a matching `method` parameter, with organizer/attendee semantics. [RFC 6047](https://www.rfc-editor.org/rfc/rfc6047) | Keep calendar MIME data and provide a native invitation card; normalization applies only to the accompanying prose. |
| Code / preformatted text / ASCII diagrams | Low with literal blocks, high with whitespace collapse or font substitution. RFC 3676 recommends fixed lines for hand-aligned text. [RFC 3676](https://www.rfc-editor.org/rfc/rfc3676) | Preserve tabs, line breaks and copy semantics; wrap visually only when explicitly chosen. |
| RTL and CJK | Medium if direction, language or breaking behavior is lost. Direction metadata matters independently of language; Unicode defines bidi and line-breaking algorithms. [W3C direction](https://www.w3.org/International/questions/qa-html-dir), [UAX #9](https://unicode.org/reports/tr9/), [UAX #14](https://unicode.org/reports/tr14/) | Preserve `dir`/`lang` and inline direction boundaries; use native layout, test mixed scripts, ruby and vertical text, and fall back where unsupported. |
| Emoji | Low if Unicode is preserved; selection or truncation can split a multi-code-point grapheme. [UAX #29](https://unicode.org/reports/tr29/) | Keep source Unicode; use grapheme-aware editing and selection. Do not rewrite received text into emoji shortcodes. |
| Inline `cid:` images | Low if resource relationships survive; high for diagrams, QR codes or image-only information if images disappear. [RFC 2387](https://www.rfc-editor.org/rfc/rfc2387), [RFC 2392](https://www.rfc-editor.org/rfc/rfc2392) | Preserve local part references, alt text and dimensions; distinguish embedded resources from remote URLs. |
| Signatures and disclaimers | Medium–high if boilerplate removal deletes qualifications or contact details. The conventional `-- ` signature separator has special treatment in flowed text. [RFC 3676](https://www.rfc-editor.org/rfc/rfc3676) | Fold rather than delete; retain all text and offer expansion. Do not assume a logo is decorative. |
| Forms and AMP for Email | High functional loss: a static projection cannot reproduce interactive state. AMP provides a separate MIME alternative and requires HTML/plain fallback. [AMP integration](https://go.amp.dev/documentation/guides-and-tutorials/email/integrate/add-email) | Display the static fallback; preserve the AMP part. Mark unavailable interaction and offer an existing ordinary web link when supplied. |

**Concrete receipt example:** the consulted Mailgun template contains an invoice for **$33.98**, with services priced **$19.99, $9.99 and $4.00**. Its item table uses `<td>` cells without `<th>`, nested inside layout tables. A rule that treats “no header cells” as proof of layout would lose real relationships. The displayed invoice date is sample content, not the template’s publication date. [Actual billing source](https://raw.githubusercontent.com/mailgun/transactional-email-templates/master/templates/billing.html)

### 2.4 Detecting layout tables versus data tables

There is no reliable inference from the `<table>` tag alone. WAI distinguishes genuine data relationships from layout and explains that complex data tables need header associations. Mozilla Readability’s implementation uses heuristics including presentation roles, captions, header-like descendants, nesting and table dimensions. These are useful signals, not sender-independent truth. [WAI](https://www.w3.org/WAI/tutorials/tables/), [Readability implementation](https://raw.githubusercontent.com/mozilla/readability/main/Readability.js)

**Proposed classifier:**

- Strong data signals: `<th>`, `scope`, `headers`, a meaningful caption, repeated row/column types, and recognizable label/value or item/amount relationships.
- Layout signals: `role="presentation"`, spacer cells, large fixed widths, nested containers, image-only cells and button-like cells.
- Inspect each nested table separately. A presentation wrapper can contain a data table. Missing semantics is uncertainty, not permission to flatten.
- Preserve row and column spans and original order. Test linearization for paired values and independent columns; do not guess a new reading order.
- If signals conflict, keep the table in a structured representation or use sender layout. Treat explicit roles as hints because sender markup can be mistaken or malicious.

Capture these signals from a bounded parse **before stripping metadata**, while treating CSS as inert input data. Never fetch stylesheets or run scripts to classify a message. Conditional Outlook markup, CSS positioning, hidden alternatives and image-based content should lower confidence.

**Content-conservation checks** should compare meaningful text regions, link destinations, local resources and table cells against the chosen source part. Any removed region needs an explicit policy reason and provenance. Text coverage alone cannot prove correctness: retaining “19.99” is insufficient if it is now associated with the wrong item.

### 2.5 Privacy, security, accessibility and native rendering

The steelman is strongest when Sweetbriar’s normalized AST cannot express scripts, forms, arbitrary CSS or automatic remote loads. That narrows the rendering surface by design. However, Markdown itself is not a security boundary: image URLs can initiate requests, raw HTML is part of the syntax, and Comrak’s `unsafe` setting enables raw HTML and dangerous links. [GFM raw HTML](https://github.github.com/gfm/#raw-html), [Comrak renderer options](https://docs.rs/comrak/latest/comrak/options/struct.Render.html)

**Recommendation:** apply the same privacy policy to normalized reading, sender layout, previews and printing. No automatic remote images, CSS imports, fonts, media, forms or navigation. Keep images opt-in, including individual embedded images where desired; loading a CID raster needs no remote fetch. Convert resource references into scoped internal IDs, never arbitrary filesystem paths. A proxy hides an IP address but is a different policy from making no request at all; HEY documents stripping and proxying as separate defenses. [HEY tracker policy](https://www.hey.com/spy-trackers/), [CID references](https://www.rfc-editor.org/rfc/rfc2392)

For the existing WKWebView fallback, use `WKWebpagePreferences.allowsContentJavaScript = false`, resource rules/CSP, controlled navigation and a scoped resource loader. Apple documents `WKURLSchemeHandler` for custom resources and the JavaScript preference’s effect. These APIs remain documented without a deprecation flag in the metadata checked **2026-10-11**. A custom scheme is a resource-access mechanism, not by itself a network firewall. [JavaScript preference](https://developer.apple.com/tutorials/data/documentation/webkit/wkwebpagepreferences/allowscontentjavascript.json), [Resource handler](https://developer.apple.com/tutorials/data/documentation/webkit/wkurlschemehandler.json)

Normalization should provide scalable typography, recipient-selected colors and sensible line length, while preserving semantic headings, lists, table associations, link names and image descriptions. It cannot recover missing alternative text or guarantee accessibility after flattening meaningful structure. WAI’s table guidance makes this loss concrete. [Accessible table semantics](https://www.w3.org/WAI/tutorials/tables/)

**Native rendering is feasible for a declared subset.** Apple documents Markdown initialization and presentation intents in attributed strings. Its `NSAttributedString` documentation also cautions that HTML import uses WebKit and ignores unsupported constructs. Avoid using native HTML import as a substitute for Rust sanitization or as proof that the pipeline avoids WebKit. [AttributedString](https://developer.apple.com/documentation/foundation/attributedstring), [NSAttributedString import](https://developer.apple.com/documentation/foundation/nsattributedstring?language=objc)

As checked **2026-10-11**, Apple’s WWDC26 session documents `NSTextViewportRenderingSurface` and viewport delegate hooks for `NSTextView`/`UITextView`. It demonstrates collapsible sections and attachment reuse in UIKit; equivalent AppKit details need implementation verification. Prefer extending `NSTextView` over rebuilding selection, input, accessibility and undo. Bridge the Rust AST into attributed text and native attachment views; use WKWebView for structures outside the proven subset. [Current TextKit guidance](https://developer.apple.com/videos/play/wwdc2026/370/)

The expected speed/memory benefit is an **inference**, not a measured result: simple messages would avoid web-document layout and can share native text infrastructure. Actual gains depend on view reuse, image decoding, DOM/AST allocations and how many fallback web views remain alive. Keep parsing off the UI thread and make native-versus-WebKit choice independent from normalized-versus-sender-layout choice.

### 2.6 Fidelity, phishing and “show original”

Use two clearly named presentations: **Reading view** and **Sender layout**. The latter is sanitized HTML, with remote content still blocked. Also expose **Message source** and original `.eml` export. Sanitized layout cannot promise the exact pixels seen in the sender’s client: different renderers, blocked resources and dynamic content make that unattainable. Current Outlook documentation itself distinguishes Word-based processing from new Outlook’s WebView2 architecture. [Classic Outlook](https://learn.microsoft.com/en-us/troubleshoot/outlook/user-interface/formatting-lost-when-editing-the-htmlbody-property), [New Outlook](https://learn.microsoft.com/microsoft-365-apps/outlook/overview-new-outlook-windows)

**Phishing risk is bidirectional.** Removing attacker-controlled branding can help, but uniform, polished styling can also make an untrusted message look authoritative. Preserve link targets exactly; show their destination through accessible inspection; keep sender/authentication indicators outside message-controlled content. Never reconstruct an apparent button destination from its label or automatically remove tracking parameters from security tokens.

MIME alternatives are intended to express the same information, but Sweetbriar should not assume that supplied plain text is complete or equivalent. Retain each alternative, record which part the user is reading, and fall back on material disagreement. Do not silently merge contradictory versions. [RFC 2046 alternative semantics](https://www.rfc-editor.org/rfc/rfc2046)

For signed mail, verify the appropriate original MIME entity before transformation and attach verification provenance to the resulting view. DKIM body canonicalization and S/MIME signed MIME handling operate on message representations, not Sweetbriar’s normalized rendering. A valid signature authenticates covered content; it does not certify a converter’s fidelity. [RFC 6376](https://www.rfc-editor.org/rfc/rfc6376), [RFC 8551](https://www.rfc-editor.org/rfc/rfc8551)

**Legal/compliance implication:** an immutable, exportable original is essential engineering support, but no general compliance guarantee follows from it. As one concrete US example, E-SIGN retention provisions concern accurate information and later accessibility/reproduction when they apply. The official U.S. Code’s indexed text and GovInfo’s linked section were checked **2026-10-11**; the main Code page was under maintenance, so the accessible GovInfo text provided the provision. Applicability and current jurisdiction-specific obligations require separate review. [15 USC §7001(d)](https://uscode.house.gov/view.xhtml?req=%28title%3A15+section%3A7001+edition%3Aprelim%29), [GovInfo provision](https://www.govinfo.gov/link/uscode/15/7001)

### 2.7 Rust tooling: capabilities, quality and speed

The following **registry snapshot was fetched on 2026-10-11**. Versions are the highest non-yanked stable releases (`max_stable_version`) returned by crates.io, not necessarily the most recently published maintenance-branch releases or a recommendation to upgrade immediately. Licenses are registry declarations; dependency/license review remains necessary.

| Tool | Version; publication date; license | Appropriate role and limitation |
|---|---|---|
| `html2md` | [0.2.17; 2026-08-20; GPL-3.0+](https://crates.io/api/v1/crates/html2md) | HTML-to-Markdown with custom tag handlers. Candidate reference converter, but its GPL declaration requires a deliberate licensing decision. [API](https://docs.rs/html2md/latest/html2md/) |
| `htmd` | [0.5.5; 2026-07-27; Apache-2.0](https://crates.io/api/v1/crates/htmd) | Turndown-inspired conversion, table options and extensibility. Its faithful mode preserves otherwise unsupported structure as raw HTML: useful evidence of Markdown’s limits. Good paste/import experiment. [Documentation](https://docs.rs/htmd/latest/htmd/) |
| `html2text` | [0.17.2; 2026-10-11; MIT](https://crates.io/api/v1/crates/html2text) | Width-aware text rendering, annotations and an intermediate render tree. Useful terminal-style fallback/reference; not a lossless Markdown document model. [API](https://docs.rs/html2text/latest/html2text/) |
| `mail-parser` | [0.11.9; 2026-09-09; Apache-2.0 OR MIT](https://crates.io/api/v1/crates/mail-parser) | MIME, charset decoding and HTML/text conversion helpers. Convenience body access can synthesize a missing alternative: preserve whether a part was supplied or derived. [API](https://docs.rs/mail-parser/latest/mail_parser/) |
| `readability` | [0.3.0; 2023-12-20; MIT](https://crates.io/api/v1/crates/readability) | Article extraction, not whole-message normalization. An older release alone does not prove abandonment. [Project](https://github.com/kumabook/readability) |
| `dom_smoothie` | [0.18.2; 2026-09-21; MIT](https://crates.io/api/v1/crates/dom_smoothie) | A current Rust Readability-style candidate for optional newsletter extraction. Evaluate on mail separately from web articles. [Project](https://github.com/niklak/dom_smoothie) |
| `html5ever` | [0.40.1; 2026-09-14; MIT OR Apache-2.0](https://crates.io/api/v1/crates/html5ever) | HTML parsing and tree construction; a foundation for a mail-specific semantic pass, not a CSS engine or sanitizer. [API](https://docs.rs/html5ever/latest/html5ever/) |
| `lol_html` | [3.0.1; 2026-07-29; BSD-3-Clause](https://crates.io/api/v1/crates/lol_html) | Streaming selector-based rewriting with limited buffering. Useful for measured rewriting hot paths; global reading order/table classification needs more context. [API](https://docs.rs/lol_html/latest/lol_html/) |
| `comrak` | [0.56.0; 2026-10-06; BSD-2-Clause](https://crates.io/api/v1/crates/comrak) | CommonMark/GFM parsing, mutable AST, HTML and CommonMark output. Strong composer candidate; disable unsafe output and choose extensions explicitly. [API](https://docs.rs/comrak/latest/comrak/) |
| `pulldown-cmark` | [0.13.4; 2026-05-20; MIT](https://crates.io/api/v1/crates/pulldown-cmark) | Pull-parser events, source offsets and optional extensions. Attractive for source-to-view mapping; an editable AST needs an additional layer. [API](https://docs.rs/pulldown-cmark/latest/pulldown_cmark/) |
| `ammonia` | [4.2.3; 2026-10-11; MIT OR Apache-2.0](https://crates.io/api/v1/crates/ammonia) | HTML5-parser-based allowlist sanitizer. Current API filters allowed inline CSS properties, but does not sanitize complete `style`/`link` stylesheets. It is a component, not a complete email resource policy. [Builder API](https://docs.rs/ammonia/latest/ammonia/struct.Builder.html) |

For `ammonia`, 4.1.7 was published after 4.2.3 on the same day, explaining why the registry’s `newest_version` differs from its highest stable version. [Registry metadata](https://crates.io/api/v1/crates/ammonia)

Registry and documentation updates can differ: `html2text`’s newly published 0.17.2 was ahead of the 0.17.1 documentation encountered during collection. Pin an audited version and test the actual artifact rather than treating a “latest” documentation URL as its exact specification.

**Quality:** no consulted source establishes comparative accuracy on representative email. A converter passing another converter’s tests establishes compatibility with that suite, not correct receipt associations, quotes or bidi. Pandoc is a useful independent reference, for example `pandoc -f html -t gfm --wrap=none`; its documented conversion losses remain relevant. This command is illustrative, not an executed benchmark. [Pandoc manual](https://pandoc.org/MANUAL.html)

**Speed:** htmd’s published benchmark, updated **2026-03-15**, reports roughly **14.5 ms** for a **1.37 MB Wikipedia HTML page**, using htmd 0.5.0 on an Apple M4 machine and excluding I/O. This is a vendor benchmark on a web page, not Sweetbriar’s measured mail-opening latency. It omits MIME decoding, sanitization, classification, image decoding and native layout. [Benchmark methodology/results](https://github.com/letmutex/htmd/blob/main/benches/README.md), [Project page-size claim](https://raw.githubusercontent.com/letmutex/htmd/main/README.md)

**Recommended starting stack:** `mail-parser` for MIME; `html5ever` plus a bounded, mail-specific AST builder for incoming HTML; `ammonia` plus explicit URL/CSS/resource policy for HTML emission; `comrak` for authored Markdown. Evaluate `htmd` for paste conversion and `html2text` for reference projections. Keep Readability ports optional and `lol_html` conditional on profiling. Use one Rust parsing profile across platforms, rather than independently reparsing the same message with Swift and Rust grammars.

### 2.8 Writing in Markdown

#### Prior art and editing expectations

MailMate documents Markdown composition with HTML preview, themes, HTML signatures and preservation of existing HTML while replying/forwarding. Markdown Here turns a browser rich-text editor’s Markdown into styled HTML, with custom CSS; its change notes illustrate the problem of edits made after rendering not surviving a return to source. [MailMate preferences](https://manual.mailmate-app.com/preferences), [MailMate introduction](https://manual.mailmate-app.com/introduction), [Markdown Here features](https://markdown-here.com/features.html), [Markdown Here changes](https://github.com/adam-p/markdown-here/blob/master/src/common/CHANGES.md)

NeoMutt’s external-viewer mechanism and Pandoc’s writers make a mutt/NeoMutt-plus-Pandoc workflow possible. That is an assembled workflow, not verified built-in Markdown authoring in mutt. [NeoMutt](https://docs.neomutt.org/howto/mime.html), [Pandoc](https://pandoc.org/MANUAL.html)

Obsidian explicitly separates Source mode from Live Preview, which hides most syntax until the cursor enters it. Bear documents Markdown concealment and tables. iA Writer combines Markdown editing with formatted preview and controls. These are useful interaction references, not proof that any editor can round-trip arbitrary email HTML. [Obsidian modes](https://obsidian.md/help/edit-and-read), [Bear 2](https://bear.app/faq/whats-new-in-bear-2/), [iA Writer](https://ia.net/writer/how-to/format-text)

**Recommendation:** support native live styling, with toolbar/keyboard commands that write the same Markdown source, and an always-available source view. Keep explicit Plain text and Markdown modes, remembered per account or recipient when the user chooses. Mainstream users should be able to bold text or create a list without learning punctuation; purists should be able to inspect and edit every character. Mode switches must preserve the prior draft revision and expose any lossy change.

For the first implementation, the already planned plain/Markdown editor with preview is sufficient. A polished live editor should follow only after selection, undo, input methods and source mapping are reliable. Arbitrary WYSIWYG HTML is a separate scope: corporate signatures and preserved quoted HTML can be immutable sanitized fragments with resource manifests, rather than pretending to be editable Markdown.

The **2025-12-12** MailMate list response quotes a user’s **2025-11-25** complaint about workshop dates changing when interpreted as numbered lists. CommonMark uses the first list marker’s number and ignores subsequent marker numbers. Thus a Markdown list starting `1. January`, then `15. March`, then `4. July` can display as 1, 2, 3. [Reported dates problem](https://www.mail-archive.com/mailmate%40lists.freron.com/msg18490.html), [CommonMark ordered lists](https://spec.commonmark.org/0.31.2/#ordered-list-marker)

Prevent that failure through explicit modes, literal paste/quote nodes, and a preview generated from the same AST as the sent message. Escaping `1\. January` preserves a literal date marker in Markdown source. Detect unexpected renumbering and offer literal text; do not invent date-detection rules that silently rewrite prose.

Define a **Sweetbriar Markdown profile**, initially CommonMark 0.31.2 plus selected GFM tables, strikethrough and autolinks. Disable raw HTML; render task boxes statically if included. Decide explicitly whether ordinary newlines create hard breaks: MailMate’s documented mail-oriented behavior differs from standard paragraph soft breaks. Avoid executable extensions and keep grammar/profile versions in drafts. [CommonMark](https://spec.commonmark.org/0.31.2/), [GFM extensions](https://github.github.com/gfm/), [MailMate Markdown behavior](https://manual.mailmate-app.com/preferences)

#### What to send

Send **`multipart/alternative` with UTF-8 `text/plain` first and `text/html` second**, generated from one authored document. MIME orders alternatives by increasing preference; the receiver selects a supported representation. [RFC 2046 §5.1.4](https://www.rfc-editor.org/rfc/rfc2046)

| Plain-text strategy | Benefit | Cost / recommended use |
|---|---|---|
| Literal Markdown source | Preserves author syntax; familiar to Markdown users; transparent in terminal mail | Source punctuation, image syntax and pipe tables remain visible. Offer an explicit preference, including compatible contacts. |
| Rendered semantic plain text | Cleaner reading in plain clients; stable literal quote/code text; readable link destinations and table projections | Cannot reconstruct all source syntax. **Recommended default**, retaining the Markdown source in Sweetbriar’s drafts and Sent records. |
| `text/markdown` instead of ordinary alternatives | Identifies Markdown explicitly | Does not supply conventional HTML/plain interoperability. Use for optional `.md` export/attachment or specialized interchange. |

For the rendered text projection, keep emphasis wording, list values, code whitespace and quote depth; render links with destinations when they differ from labels; provide image descriptions and attachment names. If a table cannot be projected without ambiguity, use explicit label/value rows and let the author inspect both alternatives before sending.

RFCs **7763 and 7764, March 2016**, are Informational. `text/markdown` requires `charset`; `variant` is an optional interpretation hint. Neither RFC mandates that Gmail, Outlook or Apple Mail render Markdown. Their current direct Markdown-body support is **(unverified)** in this research; do not base interoperability on it. MailMate’s documented `markup=markdown` plain-text convention is product-specific, not a substitute for the registered media type or conventional alternatives. [RFC 7763](https://www.rfc-editor.org/rfc/rfc7763), [RFC 7764](https://www.rfc-editor.org/rfc/rfc7764), [MailMate convention](https://github.com/mailmate/mailmate_manual/wiki/Bundles)

**Flowed versus fixed:** default Markdown-derived mail to fixed plain text, especially tables, code and patches. Offer tested `format=flowed; delsp=yes` for prose when desired. RFC 3676 uses trailing spaces to mark soft line breaks, space-stuffing and quote-depth rules; it gives `-- ` special signature treatment and warns about RTL effects. `delsp=yes` also addresses languages without spaces. A generated flowed part must encode these rules, not merely wrap lines and add a MIME parameter. Visual soft wrapping should never mutate the draft. For patch mode, send a single fixed `text/plain` body and preserve decoded content exactly. [RFC 3676](https://www.rfc-editor.org/rfc/rfc3676), [Patch requirements](https://docs.kernel.org/process/email-clients.html)

Use an email-safe HTML writer: semantic paragraphs, headings, emphasis, links, lists, basic data tables and literal code; small inline CSS for essential appearance, sensible fallbacks, no scripts/forms or external styles/fonts. Sending the reader’s dark theme as fixed black backgrounds would impose the author’s preference on recipients; keep outbound themes restrained and distinct from local reading themes.

| Recipient | Verified evidence, with date | Design consequence |
|---|---|---|
| Gmail | CSS documentation **updated 2026-09-15**, checked 2026-10-11, supports style blocks, selected selectors and media queries; unsupported CSS may be ignored. [Google CSS](https://developers.google.com/workspace/gmail/design/css) | “Gmail strips all CSS” is false. Inline essentials remain a sensible compatibility choice; preserve meaningful HTML without styling. |
| Classic Outlook on Windows | Microsoft’s article **updated 2025-06-25** documents Word-editor HTML validation and formatting loss. [Microsoft](https://learn.microsoft.com/en-us/troubleshoot/outlook/user-interface/formatting-lost-when-editing-the-htmlbody-property) | Avoid relying on browser-style layout/CSS. Verify lists, tables and code in actual classic Outlook. |
| New Outlook on Windows | Current architecture documentation, checked **2026-10-11**, describes WebView2. [Microsoft architecture](https://learn.microsoft.com/microsoft-365-apps/outlook/overview-new-outlook-windows) | Test separately; “Outlook” is not one rendering implementation. |
| Apple Mail | Historical cross-client feature tests include Apple Mail, but are not a macOS 27 compatibility certification. [CSS test history](https://www.caniemail.com/features/css-white-space/) | Expect basic generated HTML to work; exact current wrapping, dark-mode and table behavior is **(unverified)** until tested on macOS 27 and iOS. |
| Plain-text clients | The conventional plain alternative is independently usable under MIME. [RFC 2046](https://www.rfc-editor.org/rfc/rfc2046) | Validate it as a first-class message, including links, code and nontrivial tables. |

Inline CSS reduces one compatibility dependency; it does not make unsupported properties supported. Code blocks need a reliable plain alternative and explicit tests for tabs, overflow and wrapping. Tables should be deliberately simple, not a gateway to composing arbitrary newsletter layouts.

For inline images and ordinary attachments, use a conventional nested MIME structure such as:

```text
multipart/mixed
├── multipart/alternative
│   ├── text/plain; charset=UTF-8
│   └── multipart/related; type="text/html"
│       ├── text/html; charset=UTF-8
│       └── image/png; Content-ID: <unique-image-id>
└── application/pdf; Content-Disposition: attachment
```

The related root is first unless `start` identifies it; `type` describes that root. HTML refers to the image with `cid:unique-image-id`. Preserve related-part scope when importing or quoting instead of resolving arbitrary CIDs globally. [RFC 2387](https://www.rfc-editor.org/rfc/rfc2387), [RFC 2392](https://www.rfc-editor.org/rfc/rfc2392)

Rust’s `mail-builder` supplies MIME construction, including alternatives, attachments and inline resources. Treat generated MIME and transfer encodings as independently testable output, especially in patch mode; do not build boundaries and encodings by string concatenation. [Builder documentation, checked 2026-10-11](https://docs.rs/mail-builder/latest/mail_builder/)

#### Paste, signatures, replies and forwarding

**HTML paste:** use the same inert Rust parsing/resource policy, then convert the editable subset to Markdown. Never fetch linked images on paste. Copy local image bytes into the draft’s durable attachment store before inserting references. If a complex table or signature cannot survive, offer paste-as-text or retain an explicitly noneditable sanitized fragment; do not silently flatten important structure.

**Signatures:** keep text/Markdown signatures and trusted, sanitized HTML signatures as separate supported objects. Include a meaningful plain counterpart. Do not automatically delete disclaimers on reading or sending; folding is a presentation preference.

**Reply:** import the chosen quotation as literal content inside typed quote nodes, then serialize safe Markdown blockquotes. Escape syntax from incoming text; quoted `*`, numbered dates or patch content must not become new formatting. Preserve attribution and selected exact code. Superhuman’s documented selected-text Quick Quote is a good interaction model. [Quick Quote](https://help.superhuman.com/hc/en-us/articles/46005692763661-Quick-Quote)

**Forward:** offer “forward as attachment” using `message/rfc822` for the preserved original. Inline forwarding is a transformation and should say so; complex original HTML can remain a sanitized quoted fragment with its resources rather than undergo a forced Markdown conversion. MIME explicitly defines encapsulated messages. For originals with UTF-8 headers, RFC 6532 (**February 2012**) defines `message/global` and permits transfer encoding; include that subtype in forwarding compatibility tests. [RFC 2046 §5.2](https://www.rfc-editor.org/rfc/rfc2046), [RFC 6532](https://www.rfc-editor.org/rfc/rfc6532)

### 2.9 Data model, durability and search

**Recommendation: store originals; derive and cache normalization.** Separate these records:

| Record | Status and contents |
|---|---|
| Original message | Authoritative immutable bytes, hash, retrieval provenance and MIME/resource relationships. Preserve provider-native payloads too when raw MIME is unavailable; do not label reconstructed MIME as the transport original. |
| Normalized document | Rebuildable, versioned AST cache: original hash, selected MIME part, parser/profile/converter versions, schema version, resource mapping, classification reasons and security-policy version. A versioned JSON serialization is an adequate initial choice. |
| Presentation preferences | Durable mutable settings: per-message choice, explicitly remembered sender/account choice, global defaults. Keep them outside the original and cache. |
| Draft | Authoritative Markdown/plain source, mode/profile, attachment blobs, quote/HTML-fragment objects, recipient/settings state and revision history. Parsed AST is derived from the current source revision, not a competing authority. |
| Outbound attempt | Immutable final MIME snapshot associated with its draft revision and send state. Retry that snapshot deliberately; conversion updates must not silently change an in-flight message. |
| Search index | Rebuildable semantic projections with source-part and document-version provenance. Never the only copy of message content. |

Cache expensive conversions after first opening or in background work, with cancellation and bounded memory. Themes should normally affect rendering, not the semantic cache. A security-policy update must invalidate unsafe emitted HTML immediately; a stale cache must never bypass the new policy. Preserve the previous valid draft when a parser or converter fails.

Preference precedence should be explicit message choice → explicit sender/account preference → global preference → classifier. “Remember for this sender” should refer to a mailbox/account identity, not a display name or automatic domain-wide trust. Choosing sender layout must never grant permission to fetch remote resources.

For durability, write attachment blobs durably before committing references; transactionally save draft revisions and settings, and recover interrupted writes. SQLite documents atomic-commit mechanisms and their filesystem/hardware assumptions; its presence alone does not prove application-level safety across external blobs. Test crash boundaries and recovery rather than claiming an absolute hardware-failure guarantee. [SQLite atomic commit](https://www.sqlite.org/atomiccommit.html)

**Search should use semantic text, not serialized Markdown.** Index body text, quote text, signatures, link labels/targets and attachment metadata separately. Collapsed quotes and disclaimers remain searchable. If a complex message defaults to sender layout, still derive a full-content text projection for search; article extraction must not be the only indexed body.

Treat text hidden by sender CSS and alternative MIME bodies with provenance rather than silently promoting everything to the primary visible body. Search hits should open the relevant part/view and explain where the match occurred. Normalize for matching without changing stored Unicode or patch text.

Tantivy supplies pluggable tokenizers, raw tokens and n-grams; its default punctuation/whitespace tokenizer also removes long tokens. Thus defaults alone are insufficient for addresses, identifiers, CJK or long URLs. Choose language-aware body tokenization and exact/tokenized identifier fields; validate retrieval against the intended languages. Markdown normalization is useful infrastructure, not proof of best-in-class search. [Tantivy tokenizer documentation, checked 2026-10-11](https://docs.rs/tantivy/latest/tantivy/tokenizer/index.html)

## 3. Options compared

| Reading option | Best case | Principal cost | Judgment |
|---|---|---|---|
| (a) Normalized-only, original toggle | Strong uniformity; simplest default experience | Every classification/conversion error is exposed by default; user must notice the need to switch | Too aggressive for general email. |
| (b) Adaptive with explicit sender memory | Simplicity for correspondence; preserved layout for uncertainty | Classifier engineering, inconsistent appearances and occasional conservative fallback | **Recommended long-term default.** |
| (c) Reader-mode toggle | Clear consent; fastest route to a testable feature | Users repeatedly opt in; benefits remain less discoverable | Good initial release/beta behavior while (b) is validated. |

| Representation | Strength | Boundary |
|---|---|---|
| CommonMark + selected GFM | Human-readable source, portability, established parsers | Table/bidi/quote provenance and unsupported structures require extensions or embedded HTML. |
| Sweetbriar semantic AST | One Rust model for native/HTML views, quotes, resources and search | Schema maintenance and per-platform rendering work; still cannot represent all email behavior. |
| Sanitized sender HTML | Retains more sender layout and branding | Web rendering/CSS complexity; remote-dependent and interactive content still cannot be reproduced exactly. |

These judgments are **design recommendations**, not measured product rankings.

## 4. Recommendations for Sweetbriar, with trade-offs

Adopt **Markdown-first composition, semantic normalization for reading, and immutable originals for storage**. The distinction lets Sweetbriar offer a coherent reading experience without treating received mail as author-controlled Markdown.

1. **Build the Rust semantic/security pipeline first.** Preserve MIME parts and resources; create literal plain-text nodes; normalize a declared HTML subset; emit native-view data or safe HTML. Keep the same rules on macOS and Linux. Trade-off: more design work than plugging in an HTML-to-Markdown converter.
2. **Ship reader mode with conservative defaults, then enable adaptive reading category by category.** Start with plain text and simple personal HTML. Unknown tables, specialized MIME, complex layouts and contradictory alternatives default to sender layout. Make the switch immediate and explicitly remember choices. Trade-off: uniformity arrives gradually.
3. **Keep plain text as a full composition mode.** Markdown should improve expression, not become a compulsory syntax lesson. Provide live styling when robust, source/preview access, selected-text quoting, safe paste and restrained themes. Trade-off: maintaining both literal and structured editing paths.
4. **Generate and preview both conventional outbound alternatives from the same revision.** Preserve authored source locally, provide original-message attachment forwarding, and maintain a literal patch path. Trade-off: separate writers and compatibility testing rather than one generic renderer.
5. **Treat normalization and search as rebuildable projections.** Originals, drafts, attachment references and preferences have independent durable storage. Trade-off: more provenance/version metadata and cache invalidation work.

For users seeking the control associated with **Mailsmith, Claris Emailer or MailMate**, offer a visible plain-first preference, exact source/copy/export, configurable quoting and keyboard workflows. This is a proposed audience fit, not a claim that those clients share identical features. For mainstream users, expose familiar formatting controls, reliable attachments and a readable preview; do not require them to know what MIME, an AST or Markdown variants are.

**Strongest counter-argument:** even adaptive normalization spends substantial engineering effort on a problem native clients can address with better plain-text display and sanitized HTML. A classifier can confidently be wrong; the user may trust a beautifully rendered but subtly altered receipt or date. Originals and toggles are recovery mechanisms, not evidence that the first view was correct. Supporting a richer AST also risks building a second browser without browser-level compatibility.

That objection warrants a firm limit: Sweetbriar should normalize only structures it can preserve and validate, with no product target requiring an arbitrary percentage of mail to use the reading view. If real users mostly choose sender layout, retain normalization as an excellent optional reader and quoting/search layer. Markdown composition remains valuable independently.

## 5. Risks, open questions and validation plan

The biggest unknown is **semantic fidelity on real mail**, followed by whether users prefer the resulting defaults. No consulted library benchmark answers either question. Establish a consented, private evaluation corpus before widening normalization; public fixtures are useful but not representative.

**Proposed evaluation, not existing results:**

- Collect approximately **1,000 messages**, stratified across the categories in §2.3 and several providers/locales. Add adversarial fixtures and malformed MIME/HTML. Keep a frozen holdout set; assess failures per category, not only an aggregate score.
- Treat altered numbers/dates/currencies, wrong table associations, lost actionable links, changed quoted code and missing substantive qualifications as **critical failures**. Any such holdout failure blocks that category’s automatic-normalization rollout until addressed. Zero observed failures is not proof of universal safety.
- Record text/resource coverage, table relationships, link targets, quote boundaries and user-rated readability. Measure classifier false-normalization decisions separately from conservative fallback rates. Do not optimize for maximum normalization coverage.
- Include the Mailgun receipt, irregularly numbered dates, tab-sensitive patches, nested/interleaved quotes, multi-story newsletters, rowspan/colspan, conflicting alternatives, CID collisions, blocked images, mixed RTL/CJK/emoji and calendar parts. Exercise VoiceOver navigation, selection/copy and IME composition.
- Measure cold/warm conversion and first visible content, scrolling latency, peak allocations and resident memory on supported Macs. Suggested initial investigation target: **p95 conversion under 50 ms for a 100 KB body**, with asynchronous fallback if exceeded. This is a proposed budget, not a measured capability or a user-visible deadline.
- Send a fixed MIME corpus to current Gmail, classic/new Outlook, Apple Mail on macOS 27, mobile clients and a plain-text client. Inspect received source plus screenshots, dark mode, accessibility and reply/forward behavior. Record exact tested versions and dates; rerun relevant cases after renderer/profile changes.
- Monitor attempted network/resource access in every reading mode, preview and print path. Fuzz parsers and constrain depth, decoded sizes and image dimensions. Crash-test autosave, blob/reference commits, converter upgrades and send-snapshot recovery.

**Open decisions:** how much native table/layout functionality is justified before WKWebView fallback; which sender CSS can be safely retained; whether custom paragraph hard-break rules improve mail composition; which HTML signatures/quote fragments remain opaque; and which international text features belong in the first native subset.

Sender memory also needs a simple reset and protection against spoofed identities. Image-only mail cannot be made fully accessible by normalization; optional OCR must be visibly derived, with its own privacy policy. AMP/forms need an explicit static-fallback UX, not fake reconstructed controls. Signed/encrypted messages need verification/decryption provenance through caches and exports.

The product should claim **readability with preserved originals**, not “lossless Markdown conversion” or guaranteed identical rendering. Security, archival fidelity, displayed meaning and recipient compatibility are separate acceptance conditions.

## 6. Sources

All sources below were actually consulted **2026-10-11**. Historical publication dates and current documentation update dates appear in the findings. Primary documentation supplies technical claims; the user-discussion links supply labeled anecdotes. The bibliography groups the inline sources for navigation.

### Standards and semantic guidance

- Markdown: [CommonMark 0.31.2](https://spec.commonmark.org/0.31.2/), [GFM specification](https://github.github.com/gfm/), [RFC 7763: media type](https://www.rfc-editor.org/rfc/rfc7763), [RFC 7764: usage guidance](https://www.rfc-editor.org/rfc/rfc7764).
- MIME: [RFC 2046: alternatives, digests and encapsulated messages](https://www.rfc-editor.org/rfc/rfc2046), [RFC 2387: related parts](https://www.rfc-editor.org/rfc/rfc2387), [RFC 2392: CID URLs](https://www.rfc-editor.org/rfc/rfc2392), [RFC 3676: flowed text](https://www.rfc-editor.org/rfc/rfc3676), [RFC 6532: internationalized messages](https://www.rfc-editor.org/rfc/rfc6532).
- Specialized mail: [RFC 6047: iMIP](https://www.rfc-editor.org/rfc/rfc6047), [RFC 6376: DKIM](https://www.rfc-editor.org/rfc/rfc6376), [RFC 8551: S/MIME](https://www.rfc-editor.org/rfc/rfc8551), [AMP email integration](https://go.amp.dev/documentation/guides-and-tutorials/email/integrate/add-email).
- Semantics: [WAI tables](https://www.w3.org/WAI/tutorials/tables/), [HTML direction](https://www.w3.org/International/questions/qa-html-dir), [Unicode bidi](https://unicode.org/reports/tr9/), [line breaking](https://unicode.org/reports/tr14/), [grapheme boundaries](https://unicode.org/reports/tr29/).

### Apple platform and reader implementations

- Apple: [macOS 27 compatibility](https://support.apple.com/en-ca/127255), [WWDC26 TextKit](https://developer.apple.com/videos/play/wwdc2026/370/), [AttributedString](https://developer.apple.com/documentation/foundation/attributedstring), [NSAttributedString](https://developer.apple.com/documentation/foundation/nsattributedstring?language=objc).
- WebKit API metadata: [JavaScript preference](https://developer.apple.com/tutorials/data/documentation/webkit/wkwebpagepreferences/allowscontentjavascript.json), [custom resource scheme handler](https://developer.apple.com/tutorials/data/documentation/webkit/wkurlschemehandler.json).
- Reader modes: [Safari Reader](https://support.apple.com/en-mide/guide/safari/sfri32632/mac), [Firefox Reader View](https://support.mozilla.org/en-US/kb/firefox-reader-view-clutter-free-web-pages), [Readability documentation/security](https://github.com/mozilla/readability), [Readability table heuristics](https://raw.githubusercontent.com/mozilla/readability/main/Readability.js).

### Email products, composition and compatibility

- MailMate: [introduction](https://manual.mailmate-app.com/introduction), [preferences](https://manual.mailmate-app.com/preferences), [viewer/bundles](https://github.com/mailmate/mailmate_manual/wiki/Bundles), [historical quoted-text behavior](https://updates.mailmate-app.com/release_notes_archive/MailMate_r5936.html).
- Thunderbird: [display modes](https://services.addons.thunderbird.net/EN-US/thunderbird/addon/toggle-html/), [sanitized MIME implementation](https://raw.githubusercontent.com/mozilla/releases-comm-central/master/mailnews/mime/src/mimethsa.cpp).
- Outlook: [plain-text reading](https://support.microsoft.com/en-us/outlook/read-email-messages-in-plain-text-in-classic-outlook), [Word HTML processing](https://learn.microsoft.com/en-us/troubleshoot/outlook/user-interface/formatting-lost-when-editing-the-htmlbody-property), [new Outlook architecture](https://learn.microsoft.com/microsoft-365-apps/outlook/overview-new-outlook-windows).
- Other clients: [HEY privacy](https://www.hey.com/spy-trackers/), [HEY Feed engineering](https://world.hey.com/dhh/speeding-up-hey-s-the-feed-82e4d2ee), [Superhuman formatting](https://help.superhuman.com/hc/en-us/articles/46005721681165-Formatting-Text), [Quick Quote](https://help.superhuman.com/hc/en-us/articles/46005692763661-Quick-Quote).
- Other clients continued: [Mimestream](https://mimestream.com/), [formatting guide](https://mimestream.com/help/user-guide/formatting), [Spark rich-text announcement](https://sparkmailapp.com/blog/introducing-rich-text-formatting-in-spark), [Newsletter Reader history](https://apps.apple.com/gb/app/newsletter-reader/id1549306009).
- Markdown editing: [Markdown Here features](https://markdown-here.com/features.html), [change history](https://github.com/adam-p/markdown-here/blob/master/src/common/CHANGES.md), [Obsidian modes](https://obsidian.md/help/edit-and-read), [Bear 2](https://bear.app/faq/whats-new-in-bear-2/), [iA Writer formatting](https://ia.net/writer/how-to/format-text).
- Terminal/reference workflows: [NeoMutt MIME viewers](https://docs.neomutt.org/howto/mime.html), [w3m manual](https://raw.githubusercontent.com/tats/w3m/master/doc/w3m.1), [Pandoc manual](https://pandoc.org/MANUAL.html), [kernel mail-client guidance](https://docs.kernel.org/process/email-clients.html).
- Recipient CSS: [Gmail CSS documentation](https://developers.google.com/workspace/gmail/design/css), [historical cross-client white-space tests](https://www.caniemail.com/features/css-white-space/).

### Rust dependencies and persistence

- `html2md`: [registry snapshot](https://crates.io/api/v1/crates/html2md), [API](https://docs.rs/html2md/latest/html2md/).
- `htmd`: [registry](https://crates.io/api/v1/crates/htmd), [API](https://docs.rs/htmd/latest/htmd/), [README](https://raw.githubusercontent.com/letmutex/htmd/main/README.md), [benchmark](https://github.com/letmutex/htmd/blob/main/benches/README.md).
- `html2text`: [registry](https://crates.io/api/v1/crates/html2text), [API](https://docs.rs/html2text/latest/html2text/).
- `mail-parser`: [registry](https://crates.io/api/v1/crates/mail-parser), [API](https://docs.rs/mail-parser/latest/mail_parser/).
- Article extraction: [`readability` registry](https://crates.io/api/v1/crates/readability), [project](https://github.com/kumabook/readability), [`dom_smoothie` registry](https://crates.io/api/v1/crates/dom_smoothie), [project](https://github.com/niklak/dom_smoothie).
- `html5ever`: [registry](https://crates.io/api/v1/crates/html5ever), [API](https://docs.rs/html5ever/latest/html5ever/).
- `lol_html`: [registry](https://crates.io/api/v1/crates/lol_html), [API](https://docs.rs/lol_html/latest/lol_html/).
- `comrak`: [registry](https://crates.io/api/v1/crates/comrak), [API](https://docs.rs/comrak/latest/comrak/), [unsafe rendering option](https://docs.rs/comrak/latest/comrak/options/struct.Render.html).
- `pulldown-cmark`: [registry](https://crates.io/api/v1/crates/pulldown-cmark), [API](https://docs.rs/pulldown-cmark/latest/pulldown_cmark/).
- `ammonia`: [registry](https://crates.io/api/v1/crates/ammonia), [sanitizer/CSS API](https://docs.rs/ammonia/latest/ammonia/struct.Builder.html).
- Additional infrastructure: [`mail-builder`](https://docs.rs/mail-builder/latest/mail_builder/), [Tantivy tokenizers](https://docs.rs/tantivy/latest/tantivy/tokenizer/index.html), [SQLite atomic commit](https://www.sqlite.org/atomiccommit.html).

### Fixtures, reported experiences and retention law

- Receipt fixture: [Mailgun billing template](https://github.com/mailgun/transactional-email-templates/blob/master/templates/billing.html), [raw source](https://raw.githubusercontent.com/mailgun/transactional-email-templates/master/templates/billing.html).
- Thunderbird users: [2023 simplification preference](https://www.reddit.com/r/Thunderbird/comments/16o9k3p/), [2024 image/typography trade-off](https://www.reddit.com/r/Thunderbird/comments/1d0up64/), [2024 normalization request](https://www.reddit.com/r/Thunderbird/comments/1f1lppe/).
- MailMate users: [2025 discussion](https://mjtsai.com/blog/2025/02/18/mailmate-2-0-beta/), [2025 date/list incident](https://www.mail-archive.com/mailmate%40lists.freron.com/msg18490.html).
- US retention example: [15 USC §7001](https://uscode.house.gov/view.xhtml?req=%28title%3A15+section%3A7001+edition%3Aprelim%29), [accessible GovInfo section](https://www.govinfo.gov/link/uscode/15/7001).
