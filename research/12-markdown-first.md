# 12 — Markdown-first mail: reading and writing (decision D16)

The project owner's proposal (2026-10-11): *"Markdown as the primary mail
format. Convert incoming email to [a normalized form, rendered as] HTML to
ensure a consistent, pleasant reading experience (with ability to see the
original). Write in Markdown to send email."*

Synthesis of [raw/markdown/codex.md](raw/markdown/codex.md),
[raw/markdown/glm.md](raw/markdown/glm.md) and Devin's independent view in
[debate/01-review-devin.md](debate/01-review-devin.md) §8. All three
converged independently. Checked 2026-10-11.

## Verdict

The proposal bundles three ideas with very different risk:

| Idea | Verdict |
|---|---|
| **Markdown as the composer's source** | **Yes.** Proven (MailMate for a decade, Markdown Here, Mimestream's shortcuts). Low risk, high value, a few weeks to a credible v1. Keep plain text as an equal mode. |
| **Normalized reading** (one consistent rendering, "show original" toggle) | **Yes, adaptively.** Normalize what can be preserved and validated: plain mail, simple personal HTML, quoted threads, notifications. Keep sender layout (sanitized, remote content blocked) for newsletters, receipts, marketing, invites and anything uncertain. Instant switch; per-sender memory. Ship as a reader-mode toggle first, then make it the default category by category as fidelity tests pass. |
| **Markdown as the stored or universal format** | **No.** Originals stay immutable and authoritative ([04-data-safety](04-data-safety.md)). The normalized form is a derived, versioned, discardable projection. And the reading model is a richer document AST, not Markdown: GFM can't express table spans or header associations, `dir`/`lang`, quote provenance, or cid resource maps. |

So the owner's goal (consistent, pleasant, private reading; Markdown
writing) is achievable. The safe shape is **Markdown in, Markdown-like
out, a semantic AST in the middle, originals always one keystroke away**.

## Why it's attractive

- **Privacy and security by construction.** A normalized document can't
  express scripts, forms, arbitrary CSS or automatic remote loads. It is
  tracker-free and dark-mode-correct without fighting sender CSS. (Markdown
  itself isn't a security boundary: image links and raw HTML are legal
  Markdown, so the same resource policy applies in both modes.)
- **Consistency:** the reader's typography, sizes, colours and line length
  everywhere; accessibility of headings, lists and links; real dark mode.
  Mimestream sells "Dark Mode Reformatting" as a feature.
- **Speed and memory, probably:** most correspondence could render natively
  in TextKit 2 (AttributedString from the AST; WWDC26's viewport rendering
  hooks), with no WebContent process. This is a hypothesis to measure, not a
  given.
- **Better everything downstream:** quote folding, digest view (Eudora's
  Content Concentrator), copying, search text, and identical behavior on
  Linux (one Rust pipeline).
- **Fits the lineage:** Mailsmith and Claris Emailer users wanted text, not
  layout, and MailMate users get the same composer they love.

## Why "normalize everything" fails

| Mail category | Risk if normalized | Treatment |
|---|---|---|
| Personal plain text | Low, **unless** plain text is reinterpreted as Markdown (a 2025 MailMate report: dates became list numbers) | Literal text nodes; **never** parse incoming `text/plain` as Markdown automatically |
| Simple personal HTML | Low | Normalize when every structure maps cleanly |
| Quoted threads | Medium (attribution, interleaving) | Typed quote nodes; reversible folding |
| Mailing lists, digests | Low–medium | Keep message boundaries (`multipart/digest`) |
| **Patches** | **Critical** if whitespace changes; kernel docs forbid wrapping and format=flowed | Literal monospace; byte-exact copy; a dedicated plain sending mode |
| Prose newsletters | Medium meaning loss, big intentional layout change | Normalize after structural checks; article extraction opt-in |
| **Receipts and transactional mail** | **Medium–high: an amount can detach from its label.** Mailgun's billing template has an item/amount table with `<td>` cells and no `<th>`, nested inside layout tables | Preserve data grids; sender layout when uncertain |
| Marketing | High (image-only offers, coupon codes, buttons) | Sender layout by default |
| **Calendar invites** | **Critical** if flattened | Native invitation card from `text/calendar`; normalize only the prose |
| Code, ASCII art | High if whitespace collapses | Preserve tabs and line breaks |
| RTL, CJK, emoji | Medium if `dir`/`lang` are lost | Keep direction and language metadata; grapheme-safe editing |
| Inline cid images | High if images carry the information (QR codes, diagrams) | Keep resource references, alt text, dimensions |
| Signatures, disclaimers | Medium–high if removed | Fold, never delete |
| Forms, AMP | Functional loss | Static fallback, marked; keep the AMP part |

- **Layout vs data tables:** the `<table>` tag alone proves nothing.
  - **Data signals:** `<th>`, `scope`, `headers`, captions, repeated
    types, label/value or item/amount patterns.
  - **Layout signals:** `role="presentation"`, spacers, fixed widths,
    nesting, image-only cells.

  Judge each nested table separately; conflicting signals mean sender
  layout.
- **Content-conservation check:** compare text regions, link targets,
  local resources and table cells against the source. Every removed region
  needs a policy reason. Keeping "19.99" isn't enough if it's now attached
  to the wrong item.

## Trust: two presentations, clearly named

- **Reading view** (normalized) and **Sender layout** (sanitized original
  HTML; remote content still blocked unless allowed). **Message source**
  and `.eml` export are always available.
- **Phishing risk runs both ways.** Normalization strips deceptive
  styling, but uniform polish can make an untrusted message look
  authoritative. Preserve link targets exactly and show their real
  destinations. Sender and authentication indicators stay outside
  message-controlled content. Never rebuild a button's target from its
  label.
- **Signed mail:** verify DKIM, S/MIME or PGP on the original MIME entity
  before transforming, and carry that provenance into the view. A valid
  signature doesn't certify the converter.
- **Contradictory MIME alternatives:** keep both, record which one is
  shown, and fall back to sender layout on material disagreement.
- **Claim "readability with preserved originals",** never "lossless
  conversion".

## Writing in Markdown

- **Editor:** WYSIWYM on TextKit 2 (live styling driven by comrak or
  pulldown-cmark source maps, in the iA Writer / Bear style), with source
  and preview always available. Plain text is a first-class mode:
  Markdown improves expression and is never a compulsory syntax lesson.
- **Pasting** HTML converts to Markdown (`htmd`).
- **Inline images:** a drag creates `![]()` with a `cid:` reference.
  GFM tables; code blocks with syntax highlighting (`syntect`).
- **Signatures** are written in Markdown too.
- **What gets sent:** `multipart/alternative`.
  - **Plain part first:** text rendered from the Markdown (format=flowed
    behind a preference), not raw source.
  - **HTML part second:** email-safe generated HTML with inlined CSS and a
    fixed, tested template, checked against Outlook's Word engine, new
    Outlook, Gmail's CSS stripping, Apple Mail and plain-text clients.
  - **No `text/markdown` part by default.** RFC 7763 parts are unused by
    mainstream clients, and leaking source to recipients is worse than
    clean HTML. Offer it per identity: MailMate's private
    `markup=markdown` parameter for MailMate correspondents, or a
    `text/markdown` attachment.
  - **Keep the Markdown source** in the draft revision locally.
- **Replies:** quote from the AST as Markdown blockquotes, with a "keep
  original formatting" fallback (MailMate does both). **Forwarding** sends
  the untouched `message/rfc822`.
- **Patches:** a literal plain-text mode, with no wrapping and no flowed
  text.

## Architecture

```
original MIME (immutable) ──► part selection ──► sanitize ──► semantic normalize ──► MailDocument AST (versioned, cached)
                                    │                                                     │            │
                                    └──► calendar/AMP/specialized parts ──► native cards  │            │
                                                                                          ▼            ▼
                                                        native TextKit 2 view (supported subset)   search text, quote
                                                        sanitized WKWebView (sender layout)        folding, digest view
```

- **The `MailDocument` AST** is a versioned Rust type: paragraphs,
  headings, emphasis, explicit line breaks, lists, literal and
  preformatted blocks, nested quotes with attribution, links, image
  references, tables with spans and header associations, `dir`/`lang`, and
  provenance back to MIME parts. Unsupported structure means "use sender
  layout".
- **Pandoc** is the architectural precedent: readers produce an AST, and
  writers emit outputs. It also warns that conversion loses information
  when the AST is less expressive than the input.
- **Four separate steps:** MIME selection → sanitization → semantic
  normalization → (optional) article extraction.
- **Storage:** the normalized form is cached with a converter-version stamp
  and rebuilt on upgrade. For encrypted mail, derive on open and never
  cache. Per-sender and per-message presentation choices are user data.
  Search indexes the normalized text.
- **Native vs WebKit** is a separate choice from normalized vs sender
  layout.
- **The same Rust pipeline serves macOS and Linux** (WebKitGTK for sender
  layout, GTK text views for reading).
- **Tooling:**
  - comrak 0.56 (652/652 CommonMark, 670/670 GFM tests) or pulldown-cmark
    0.13
  - htmd 0.5.5 (about 16 ms for a 1.37 MB page)
  - html2text, ammonia, lol_html, html5ever
  - `dom_smoothie` (Readability port) for opt-in article extraction; it
    doesn't sanitize
  - syntect for highlighting

## Rollout

1. **Composer first** (weeks): Markdown and plain modes, the
   multipart/alternative writer, a recipient-client compatibility matrix.
2. **The Rust semantic and security pipeline,** with a **reader-mode
   toggle**: plain text and simple personal HTML only.
3. **Evaluation corpus before any automatic default** (codex):
   - About 1,000 consented messages, stratified by category, provider and
     locale, plus adversarial and malformed fixtures and a frozen holdout.
   - **Critical failures:** altered numbers, dates or currencies; wrong
     table associations; lost action links; changed quoted code; missing
     qualifications. Any one of them in the holdout blocks automatic
     normalization for that category.
   - Measure fidelity separately from user preference.
   - Performance budget to investigate: **p95 conversion under 50 ms for a
     100 KB body**, falling back asynchronously if exceeded.
4. **Adaptive defaults,** category by category, with per-sender memory
   (resettable, and safe against spoofed senders).
5. **No target percentage of "normalized" mail.** If users mostly choose
   sender layout, normalization stays an excellent optional reader plus a
   quoting and search layer. The composer stands on its own.

**Effort** (GLM's estimate): composer and wire format, about 4–6 weeks.
Reading pipeline: about one engineer-year to be good, then an ongoing
fidelity treadmill.

## Strongest counter-argument

This is a fidelity war disguised as a feature. Mail is the most hostile
HTML corpus there is. Every normalized view that misleads a user about a
receipt total, a legal disclaimer or a phishing cue becomes "Sweetbriar
changed my mail". Thunderbird, after 20 years, still renders originals and
leaves adaptation to an add-on. The answer: conservative, adaptive
defaults, instant originals, immutable storage, a corpus with blocking
critical-failure criteria, and a reader-mode fallback that keeps 80% of
the benefit at 20% of the exposure.

## Open questions

- How much native table and layout support is worth building before
  falling back to WKWebView?
- Which sender CSS (if any) to keep in Reading view?
- Should Reading view be the default for unknown senders (less phishing
  styling) or only for known correspondents?
- Image-only mail and optional OCR (visibly derived, own privacy policy).

## Key sources

- [MailMate introduction](https://manual.mailmate-app.com/introduction), [MailMate plain-text-as-Markdown incident](https://www.mail-archive.com/mailmate%40lists.freron.com/msg18490.html)
- [CommonMark](https://spec.commonmark.org/0.31.2/), [GFM tables](https://github.github.com/gfm/#tables-extension-), [RFC 7763](https://www.rfc-editor.org/rfc/rfc7763), [RFC 2046](https://www.rfc-editor.org/rfc/rfc2046), [RFC 3676](https://www.rfc-editor.org/rfc/rfc3676)
- [Pandoc manual](https://pandoc.org/MANUAL.html), [WAI table tutorials](https://www.w3.org/WAI/tutorials/tables/), [Readability](https://github.com/mozilla/readability)
- [Mailgun billing template](https://github.com/mailgun/transactional-email-templates/blob/master/templates/billing.html), [Kernel email-client guidance](https://docs.kernel.org/process/email-clients.html), [RFC 6047 iMIP](https://www.rfc-editor.org/rfc/rfc6047)
- [WWDC26 TextKit](https://developer.apple.com/videos/play/wwdc2026/370/), [AttributedString](https://developer.apple.com/documentation/foundation/attributedstring)
- [comrak](https://github.com/kivikakk/comrak), [htmd](https://github.com/letmutex/htmd), [caniemail](https://www.caniemail.com/)
