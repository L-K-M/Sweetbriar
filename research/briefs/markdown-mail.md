# Topic: Markdown as Sweetbriar's primary mail format (reading and writing)

The project owner proposes: "Markdown as the primary mail format. Convert
incoming email [into a normalized form, rendered] to HTML to ensure a
consistent, pleasant reading experience (with the ability to see the
original). Write in Markdown to send email."

Evaluate this seriously, steelman it, find its limits, and propose the best
design. Current plan for context: immutable original messages are always
kept; HTML mail is sanitized in Rust and shown in a locked-down WKWebView
(JavaScript off, remote content blocked); plain text uses a native TextKit 2
view; the composer supports plain text and Markdown (generating
multipart/alternative).

## Questions

1. **Reading: normalize, then render with Sweetbriar's own design.**
   Convert incoming HTML and plain-text mail into a canonical lightweight
   representation (CommonMark + GFM, or an intermediate document AST) and
   render it with Sweetbriar's typography, dark mode and accessibility, with
   one-click "show original".
   - Prior art and outcomes: reader modes (Safari Reader / Readability,
     Firefox Reader View), clients' simplified views (Thunderbird "Simple
     HTML" and plain text, Outlook plain-text view, MailMate rendering
     everything through its own HTML, terminal clients via w3m/pandoc,
     HEY, Superhuman, Mimestream, Spark, and any 2024-2026 client that
     normalizes mail). What did users think?
   - Feasibility per mail category, using real examples where possible:
     personal plain mail, quoted threads, mailing lists and patches,
     newsletters (Substack and similar), transactional mail and receipts
     (layout tables vs data tables), marketing mail, calendar invites,
     code and preformatted text, RTL and CJK, emoji, inline (cid) images,
     signatures and disclaimers, AMP for Email. Estimate fidelity loss and
     failure modes; how to detect layout tables vs data tables.
   - Tooling, Rust preferred: html2md, htmd, html2text, mail-parser's
     HTML-to-text, readability ports, html5ever, lol_html, comrak and
     pulldown-cmark for rendering; pandoc as a reference. Quality and speed.
   - Benefits: privacy and security by construction (no remote CSS, JS,
     tracking pixels; images opt-in), consistent typography and dark mode,
     accessibility, speed and memory (could most mail render natively with
     TextKit 2 / AttributedString instead of WKWebView?), better quoting,
     digest views and search.
   - Risks: broken newsletters and receipts, legal and compliance (the user
     must be able to see exactly what was sent), phishing (rendering
     differs from the sender's intent), branding, forms.
   - Design options: (a) normalized-only with an "original" toggle;
     (b) adaptive: classify each message and choose normalized vs original
     by default, with per-sender memory; (c) a reader-mode toggle.
     Recommend one.
2. **Writing in Markdown.**
   - Prior art: MailMate (Markdown to multipart/alternative, themes),
     Markdown Here, mutt + pandoc, Mimestream's Markdown shortcuts,
     live-styled editors (iA Writer, Bear, Obsidian live preview).
   - What to send: multipart/alternative with text/plain (Markdown source,
     or rendered plain text; format=flowed?) and text/html (generated,
     inlined CSS, email-safe). How do Outlook (Word engine), Gmail (CSS
     stripping), Apple Mail and plain-text clients display it? Does
     RFC 7763/7764 `text/markdown` matter in practice?
   - Editing UX: raw Markdown vs live styling (WYSIWYM) vs WYSIWYG on
     TextKit 2; pasting HTML (convert to Markdown); inline images and
     attachments; signatures; tables; code blocks.
   - Replying to and forwarding HTML mail: quoting as Markdown blockquotes;
     faithful forwarding of the original.
3. **Data model:** originals immutable; normalized form stored or derived?
   Per-message and per-sender rendering choice; search over normalized
   text.
4. **Recommendation** for Sweetbriar with trade-offs and the strongest
   counter-argument. How does it fit both plain-text purists (Mailsmith,
   Claris Emailer, MailMate users) and mainstream users?
