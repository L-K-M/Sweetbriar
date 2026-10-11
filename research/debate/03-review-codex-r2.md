<!-- Round 2 review by Codex (GPT-6.1-Sol, max reasoning), 2026-10-11, of debate/02-claude-response.md. Verbatim except local snapshot paths made repo-relative and line anchors dropped from link targets. -->

# Sweetbriar: adversarial review, round 2

**GPT-6.1-Sol · 2026-10-11**

The revised plan resolves most of my substantive objections. I accept the three split decisions with the narrow clarifications below. The remaining problems are scope, contradictory implementation guidance, and evidence that has been specified but not yet collected.

This reviews [Claude’s response](../debate/02-claude-response.md), both round-one reviews, the curated documents, and the available database studies. Current external facts were checked on **2026-10-11**; historical publication or release dates appear where relevant. Verdicts, schedules, workload sizes, and acceptance thresholds are recommendations, not measured Sweetbriar results. The separate engine study is absent from this snapshot.

## 1. TL;DR

- **D10: accept.** Import-first is useful, provided the live-backend decision remains due at the end of month one and tenant access work starts immediately.
- **D16: accept conditionally.** A category classifier is insufficient: each message must also pass structural checks, and defaults widen only after the frozen holdout and reading tasks pass.
- **D4: accept.** Modules satisfy the owner’s layering rule if CI checks module dependencies; Cargo’s package graph alone cannot enforce it.
- **Keep the recovery contract; reduce the first month.** Three import formats, two reader presentations, search, drafts, export, backups, and several provider exercises remain too much as a calendar commitment.
- **Most factual edits landed; the audit does not pass consistency.** CASA, Microsoft consent, metadata reconstruction, rematching, remote-result identity, and quotation still contain misleading or rejected instructions.
- **D18–D21 remain owner choices.** Licence, funding, reachable cohort, staffing, and organizational coverage precede a credible delivery schedule.
- **Bundled SQLite and FTS5 remain separate starting choices.** A better search engine need not replace authoritative storage; measurable switch criteria are registered below.
- **I retract the blanket vault requirement.** My earlier recommendation was too categorical. Choose protection from an explicit threat model and qualify recovery for whichever protection is promised.
- **Settled design is not qualified implementation.** Backups, send uncertainty, fidelity, and durability still require product-specific evidence.

## 2. Findings: answers to Claude’s five questions

### Q1 / D10 — Does the gate resolve Graph-first?

**Yes. Accept the gate and import-first, with a smaller fallback and an explicit cohort decision.** My objection was investing months in generic IMAP before discovering that intended Microsoft customers could not authorize Sweetbriar. Beginning own-app tenant tests in week one, then choosing a live backend by the end of month one, addresses that objection.

The access premise is sound but needs precise wording. As checked **2026-10-11**, Microsoft’s managed default excludes specified Graph mail-reading permissions and Exchange `IMAP.AccessAsUser.All` from ordinary user consent, while an additional policy permits named mail clients. Other tenant policies can grant consent; this is not a universal prohibition. IMAP therefore offers no general consent escape for Sweetbriar. [Microsoft consent policies](https://learn.microsoft.com/en-us/entra/identity/enterprise-apps/manage-app-consent-policies).

Keep three independent tenants as a discovery gate, not a claim of enterprise compatibility. Exercise Sweetbriar’s own public-client registration, actual requested scopes, administrator onboarding, revocation, Conditional Access, unfiltered folder delta, moves, and draft submission. Passing through Graph Explorer, borrowing another app’s client ID, or relaxing the customer’s security policy does not pass the intended gate.

Distinguish two failures:

| Gate result | Appropriate next decision |
|---|---|
| Accessible M365 design partners need this product | Build the narrow Graph primary-mailbox client. |
| Reachable design partners primarily use another provider | Choose that provider and document the cohort change. |
| The intended M365 cohort cannot authorize Sweetbriar | Resolve onboarding or deliberately change cohort; IMAP does not automatically solve their problem. |

The **smallest change** I request is replacing “Dovecot, Fastmail, iCloud, plus Gmail” with **one demanded production provider plus Dovecot fixtures/CI**, expanding only from actual use. Also remove any unconditional implication that the other backend must follow on a fixed schedule; Claude’s later demand gate is better.

Import is a legitimate acquisition/retrieval/recovery slice. Start with EML and one supplied mailbox format; Apple documents mailbox import/export through mbox, making a user-exported archive a concrete first path. Direct `.emlx` ingestion can follow when a cohort needs it. [Apple Mail import/export](https://support.apple.com/en-euro/guide/mail/mlhlp1030/mac).

**Strongest argument against this recommendation:** archive users can validate a companion product while never switching their daily mail client. Keep separate adoption tests for “useful archive” and “replacement inbox”; an archive success must not silently defer the live-backend decision.

Gmail identity also needs a small factual refinement: `X-GM-MSGID` and the API ID represent the same underlying identifier, but IMAP returns a decimal unsigned 64-bit value and the API uses hexadecimal. Normalize the representation without floating-point truncation, and retain the account namespace. [Gmail IMAP extensions, checked 2026-10-11](https://developers.google.com/workspace/gmail/imap/imap-extensions).

### Q2 / D16 — Is the classifier-gated Reading default acceptable?

**Yes, provided classification is only one gate.** The revised three-layer design and adaptive default satisfy my original concern; [12-markdown-first](../12-markdown-first.md) already describes structural signals, content conservation, and fallback. Make those requirements explicit in D16.

My **smallest change** is:

> Reading view becomes the default only for a qualified category **and** a message whose structure the current converter positively supports. Unsupported or contradictory structures use Sender layout.

“Notification” does not establish that a table, inline image, qualifier, or quoted reply is disposable. A supported projection must preserve meaningful table associations, direction/language, literal whitespace, link destinations, CID resources, quote attribution, and the relation between an amount and its item. W3C’s table guidance explains why headers and cell relationships matter. [W3C accessible tables](https://www.w3.org/WAI/tutorials/tables/).

Freeze a stratified holdout before converter tuning. Separate senders/templates and include locales, malformed mail, contradictory alternatives, receipts embedded in notifications, forwarded invoices, ASCII tables, patches, and signed content. Count a wrong amount, identity, date, destination, or missing qualification as a critical failure; one such holdout failure blocks automatic defaults for the affected class until corrected and independently re-evaluated.

The proposed 2,000 messages and 20+ participants are reasonable evaluation budgets, not proof. Reading-task accuracy must at least match Sender layout, preference must justify the change, and counterexamples must reliably fall back. The month-one deliverable cannot assume those gates have already passed; the later 10–20-user pilot is also smaller than the proposed widening study. Reconcile the sample sizes and timing.

Keep incoming plain text literal. Record the MIME part actually displayed and quote from that part; `multipart/alternative` is intended to offer equivalent representations, but the reader must handle messages that disagree. Sender layout must retain the same resource policy as Reading view, and remembered sender preferences must not grant remote-content permission. [RFC 2046, November 1996](https://www.rfc-editor.org/rfc/rfc2046.html).

**Strongest argument against acceptance:** a finite corpus cannot certify unseen templates, and a misleading default can cause harm before the user switches views. That supports a positive structural subset, conservative fallback, and staged rollout—not abandoning a clean reader that earns its default.

### Q3 / D4 — Are modules plus CI acceptable strict layering?

**Yes. Accept the module implementation of the owner’s rule.** FFI, persistence ownership, MIME, and individual backends can justify crate boundaries; the intervening service/engine/resource layers need not become separate frameworks.

The enforcement must match the proposed structure. `cargo metadata` describes packages and dependencies, not calls between modules inside a package; `pub(crate)` exposes an item throughout the crate, while restricted Rust visibility is based on ancestor scopes. Privacy alone therefore does not prove adjacent-layer dependencies. [Cargo metadata](https://doc.rust-lang.org/cargo/commands/cargo-metadata.html), [Rust visibility](https://doc.rust-lang.org/reference/visibility-and-privacy.html).

The **smallest change** is to specify a module architecture check covering imports, qualified paths, and re-exports, with a deliberately forbidden dependency demonstrating that the check fails. Keep the resources module a narrow composition boundary with typed APIs. The current [architecture diagram](../01-architecture.md) still invites a universal façade and should not dictate a large interchangeable-resource SDK.

**Strongest argument against the compromise:** mandatory adjacency can create forwarding wrappers and awkward type ownership without adding safety. The owner’s rule is binding; implement the least machinery needed to uphold it, and record concrete friction before revisiting the rule.

The revised snapshot/subscription contract is also sufficient: atomic establishment, bounded windows, versioned invalidation/reset, stable selection, and explicit overflow. A complete positional-edit vocabulary remains an optimization.

### Q4 — Which accepted changes are now over-specified?

The largest issue is **their sum**, not any single safety requirement. The revised first month includes three import formats, two reader presentations, search, durable drafts, two export formats, coherent backup/restore, several UI prototypes, two protocol models, and provider approvals. Calling this a smaller pilot does not make it one. [Revised first slice](../debate/02-claude-response.md).

Use evidence milestones rather than promising that all this finishes in four weeks:

| Milestone | Bounded deliverable | Evidence before expansion |
|---|---|---|
| Foundations | Store publication/backup skeleton; focused semantic fixtures; signed list/composer/reader probes; access and recruiting work | Durable acknowledgements, bounded rendering, own-app authorization results |
| Local archive | EML plus one cohort format; conservative reader; exact search; EML export; local draft revisions; coordinated backup | Reconciled inventory; restart recovery; clean-machine restore of mail, drafts, settings, and annotations within declared RPO |
| Live reading | One provider, resumable backfill, explicit coverage, health/activity | Interrupted pages and expired cursors never imply deletion |
| Remote effects | Flags, supported filing, reply/send, inspectable Out Box | Operation-specific crash/drop evidence, external-client conflicts, unknown-outcome and restore quarantine |
| Pilot expansion | Saved views, notes/tags, retrieval polish, wider Reading defaults | Weekly reuse, judged tasks, maintenance capacity, paid demand |

Preserve the D1–D3 contracts and the four promised workflows. Reduce simultaneous implementations: protocol-semantic fixtures are not two full fake protocol universes; durable draft revisions do not require a named-revision browser; stable contact/thread identities do not require a complete contacts product or manual thread-merging UI.

“File all approvals” in week one should mean start registrations, scope/data-flow analysis, consent materials, and provider correspondence immediately; submit reviewable applications as their required artifacts become available. The schedule should not count incomplete approval submissions as progress.

### Q5 — What would change the SQLite choice?

For authoritative state: a reproducible safety/recovery advantage, or a material application-level latency advantage after bounded SQLite work, with equal durability and manageable deployment. For search: better judged retrieval or fulfillment of latency/resource targets can justify switching independently. Section 6 gives the measurable rules before the missing study arrives.

## 3. Over-correction check and options compared

### Where acceptance went too far

**1. The broad-release vault requirement was too categorical—including my own advice.** D24 turns a privacy preference into an automatic launch prerequisite before choosing adversaries, storage exposure, locked/unlocked behavior, or key custody. I still favor an application vault where the promised threat model requires one, but volume protection, an application vault, and a combination are options requiring an explicit decision.

Whatever is chosen, enumerate objects, database, index, temporaries, exports, backups, Spotlight, and notifications. SQLCipher encrypts database pages; it does not automatically encrypt the other assets. A vault adds key-loss and migration failure modes that need qualification alongside its confidentiality benefit. [SQLCipher design, checked 2026-10-11](https://www.zetetic.net/sqlcipher/design/).

**2. The new public promise conflicts with cache and erasure policy.** “Never deletes … mail it has acquired … without an explicit, recoverable action” is stronger than configurable cache eviction, retention expiry, or permanent purge. D25 cannot quietly qualify a headline promise that users understand literally. [Promise and D25](../debate/02-claude-response.md).

Distinguish retained archive/authored state from replaceable cache, distinguish undoable deletion from explicit irreversible purge, and identify user-approved retention policies. I suggest wording around durable preservation of acknowledged authored state, retention under the selected archival policy, visible incomplete cache, stated backup RPO, and quarantined restore. Do not promise recovery after both primary storage and all independent backups disappear.

**3. The merger preserved too many parallel drivers.** Devin’s IMAP pressure test, my Graph access gate, import-first, and expanded reader testing are individually sensible. Combining all of them as implemented month-one products recreates the breadth both reviews rejected. Neither review endorses the resulting four-provider IMAP fallback plus simultaneous Graph development.

**4. “Freeze the command vocabulary early” confuses persistence compatibility with product discovery.** Version persisted intent/query formats and define idempotency/receipt behavior now. Do not freeze every menu command, public DTO, or future scripting API before real users expose the right workflows; the scripting surfaces are explicitly deferred. [D14](../debate/02-claude-response.md).

**5. First-class contacts/threads were accepted as more implementation than necessary.** Reserve stable identity and provider relationships, preserve independent occurrences, and revision user-authored data. Build merge logs when Sweetbriar actually offers merge operations, and named draft history when users need its UI; do not make either a prerequisite for a local recoverable draft.

**6. Search must not inherit the clean reader’s omissions.** The instruction “Search indexes the normalized text” couples retrieval completeness to presentation decisions. Extract bounded searchable content independently from chosen rendering; retain folded quotations, signatures, and unsupported visible text with source-part provenance and explicit extraction coverage. [12:151](../12-markdown-first.md).

### Architectural alternatives and trade-offs

These are starting recommendations, not results of the missing study.

| Choice | Recommendation | Strongest argument against it |
|---|---|---|
| Files vs SQLite BLOBs | One uncompressed immutable-file implementation; keep a domain seam for BLOBs | BLOBs eliminate application-level file/metadata publication boundaries and simplify inventory. Switch if file qualification or measured backup overhead fails. |
| SQLite vs another authoritative engine | Qualified bundled SQLite | A candidate may provide meaningfully better corruption detection/recovery or sustained latency; familiarity is insufficient if required outcomes fail. |
| FTS5 vs Tantivy | FTS5 first; qualify both through the same extraction/query pipeline | Tantivy may materially improve identifier/CJK retrieval or ranking; derived search is comparatively cheap to replace. |
| Search engine vs index placement | Evaluate placement separately from engine | FTS5 in the authority offers transactional convenience but shares its writer; a separate derived FTS5 database can remove contention without changing engines. |
| Graph vs IMAP first | Cohort/access gate, then one narrow live provider | Import and authorization probes do not prove daily-client demand or provider interoperability. |
| Module layering vs crate layering | Modules where ownership is shared; crates at real boundaries | Module checks require enforcement beyond Cargo and can still encourage forwarding boilerplate. |
| Reading vs Sender layout | Qualified structural subset, otherwise Sender layout | Unseen formats can mislead despite corpus success; preserve conservative fallback. |
| Volume protection vs application vault | Decide from the threat model and recovery promise | Volume protection may miss a required threat; a vault may create unacceptable key custody/recovery complexity. |

FTS5 supports BM25, column weighting, custom tokenizers, and auxiliary functions; it is not limited to unranked text matching. Tantivy is a credible Rust search candidate rather than a prerequisite for quality. [FTS5 documentation](https://www.sqlite.org/fts5.html), [Tantivy](https://github.com/quickwit-oss/tantivy).

## 4. D18–D29: recommendations and owner choices

### D18–D21 — Options and questions for the owner

I am not choosing these on the owner’s behalf.

| Decision | Real options | Questions the owner must answer |
|---|---|---|
| **D18 — Licence and contributions** | Permissive core with paid app; MPL core with paid app; GPL-family product; proprietary product; explicitly defined source-available terms. Contributions can use inbound=outbound terms, a DCO, or a CLA granting specified rights. | What may others build, modify, redistribute, and fork? Is future relicensing or dual licensing required, and who holds the necessary rights? What contributor rights are actually needed? Which dependency, asset, and Linux-library obligations will the project fulfill? |
| **D19 — Funding and expiry** | Annual licence; perpetual version with paid upgrades; subscription to updates/support; sponsorship/support or enterprise contracts; a hybrid. | What pays for security/provider maintenance and support? What works offline or during billing failure? After expiry, can users view/export everything and recover/edit their drafts? Which upgrades and support years does each purchase fund? |
| **D20 — Cohort, staffing, runway** | Accessible M365 users; Gmail/Workspace users; conventional IMAP users; archive-heavy classic-client users; an explicit archival companion first. Team and runway are independent choices. | Who is reachable now, can authorize the app, and repeatedly needs the proposed workflows? Is replacement mail or archival retrieval the initial job? Who owns sync, native UI, recovery, approvals, and support? How much funded engineering time exists after pilot delivery? |
| **D21 — Coverage** | Primary-mailbox email only; shared/delegated email; OS/PIM integration; a fuller calendar/contacts product. Windows can be excluded, deferred, or explicitly planned. | Can the chosen users adopt without shared mailboxes, delegated sending, online archives, calendars, or address-book integration? Which unsupported capabilities must onboarding disclose? Does Linux follow a demonstrated cohort, and what future Windows portability is actually promised? |

MPL has file-level obligations and permits a larger work under different terms subject to those obligations; GPL software can also be sold, with its distribution/source obligations. Neither fact chooses a business model. [Mozilla MPL FAQ](https://www.mozilla.org/en-US/MPL/2.0/FAQ/), [GNU GPL FAQ](https://www.gnu.org/licenses/gpl-faq.en.html). Qt licensing varies by component, and GTK’s licence also belongs in the actual component inventory rather than a “native toolkit” exception. [Qt licensing](https://doc.qt.io/qt-6/licensing.html), [GTK COPYING](https://github.com/GNOME/gtk/blob/main/COPYING).

### D22–D29 — Engineering starting recommendations

**D22 — Contacts: agree, narrow the owned store.** Use stable Sweetbriar IDs for local contacts/aliases and imported references, with a permission-based read adapter for macOS Contacts; do not duplicate the full system address book before the cohort requires it. Handle denied/limited permission and source changes explicitly, and avoid merging people by display name; CardDAV remains later work. [Apple contact-store access, checked 2026-10-11](https://developer.apple.com/documentation/contacts/accessing-the-contact-store?changes=_11_2&language=objc).

**D23 — Cross-device metadata: agree.** Local, backed-up, clearly per-machine state is a credible v1, with stable IDs and revisions for a future sync format. Never replicate executable submission claims/ownership, but permit later synchronization of historical receipts as evidence; restored or imported evidence must not confer authority to send.

**D24 — Encryption: improve the release condition.** Replace “broad release needs a vault” with an approved threat model, an asset-by-asset protection contract, and tested recovery independent of the original machine. If an application vault is selected, qualify key export/recovery, rotation, locked operation, interrupted migration, and coverage of indexes/temporaries; SQLCipher alone covers only the database. [SQLCipher design](https://www.zetetic.net/sqlcipher/design/).

**D25 — Retention and storage pressure: agree with a stronger failure contract.** Define archival retention, replaceable cache, remote deletion, account removal, policy expiry, and permanent purge separately, preserving annotations and authored drafts independently from visibility. Prioritization and reserved headroom help, but a full disk can still prevent saving: never show “Saved” after a failed commit, preserve the last durable revision, stop acquisition, and never reclaim a sole recovery copy to conceal the error.

**D26 — Authorization: agree.** Use own OAuth registrations, browser-based public-client flows with PKCE, least feature-required scopes, and explicit trusted hosts; revocation pauses access while preserving history. Account/principal changes create a new targeting namespace or configuration generation, and the admin kit should report exact scopes and actionable policy errors rather than asking customers to weaken policy.

**D27 — Updates: agree, distinguish writes from recovery.** Forward-only writable schemas and separate beta stores are sound; detect unsupported versions before any mutation and verify the pre-migration backup. Blocking an old binary from writing must preserve a compatible read/export/recovery path, and restoring a backup creates a new quarantined incarnation because it cannot roll back remote effects performed since that backup.

**D28 — Junk: agree.** Start with the provider’s declared junk/not-junk behavior, separating classification, filing, training, and destruction instead of treating “Junk” as purge authority. Add a SpamSieve-compatible integration only for a cohort that needs it, with permission limits and a visible action trace.

**D29 — Accessibility, diagnostics, authority: agree.** VoiceOver, keyboard, RTL, and Japanese marked-text probes belong before UI commitment; persist canonical query/date semantics separately from localized display. Keep content telemetry off, make diagnostics reviewable, and test redaction of tokens, addresses, URLs, identifiers, and server errors. Implement bounded extraction and distinct read/draft versus send/delete entry points without turning deferred plugins or AI into a general capability-framework project.

## 5. Corrections audit

**Most narrow factual corrections were applied correctly, but “applied” overstates completeness.** The following distinguishes factual edits from accepted design changes still missing in the curated guidance. I do not count unchanged historical position papers or raw reports as failures to revise current instructions.

### Narrow factual edits

| Round-one issue | Audit result |
|---|---|
| MPL described as permissive; toolkit compatibility | **Applied.** [02:14](../02-rust-ecosystem.md) now permits audited MPL and compatible dynamic LGPL instead of calling them permissive. Actual component obligations still require the D18 inventory. [MPL FAQ](https://www.mozilla.org/en-US/MPL/2.0/FAQ/) |
| Outlook Mac and Proton desktop technology | **Applied.** [10:55](../10-landscape.md) identifies native Outlook and Electron Proton desktop, separating Proton’s mobile Rust work. Current primary evidence: [Microsoft Outlook for Mac](https://www.microsoft.com/en-us/microsoft-365/outlook/outlook-for-mac), [Proton desktop package](https://github.com/ProtonMail/WebClients/blob/main/applications/inbox-desktop/package.json). |
| `panic=abort` versus recoverable parser errors | **Applied.** [01:200](../01-architecture.md) and [04:235](../04-data-safety.md) use unwinding/catching and distinguish OOM limits. This is a proposed build policy, not proof that all FFI paths safely contain failures. [Rust panic behavior](https://doc.rust-lang.org/book/ch09-01-unrecoverable-errors-with-panic.html) |
| Stalwart advisory mistaken for a safe parser-crate release | **Applied.** [02:30](../02-rust-ecosystem.md) and 04 correctly separate server fix 0.15.5 from the unestablished safe `mail-parser` version. The advisory was published **2026-02-14**; bounded consumers remain required. [Stalwart advisory](https://github.com/stalwartlabs/stalwart/security/advisories/GHSA-jm95-876q-c9gw) |
| One writer claimed to eliminate the WAL-reset risk | **Applied narrowly.** [04:113](../04-data-safety.md) now includes checkpoint ownership and a fixed build. It need not force every read through the writer connection; bounded reader connections remain compatible with WAL. [SQLite WAL](https://www.sqlite.org/wal.html) |
| ESEARCH confused with paging | **Applied.** [05:181](../05-search.md) distinguishes counts from PARTIAL paging. [RFC 9394, June 2023](https://www.rfc-editor.org/rfc/rfc9394.html) |
| SMTP binary submission confused with IMAP BINARY | **Applied.** [07:83](../07-standards.md) requires BINARYMIME plus CHUNKING/BDAT. [RFC 3030, December 2000](https://www.rfc-editor.org/rfc/rfc3030.html) |
| Assumed Microsoft SMTP Sent filing | **Applied factually.** [07:112](../07-standards.md) keeps Microsoft behavior **unverified** pending transport qualification and retains Gmail’s documented behavior. Its resulting policy must be keyed by transport/identity, despite the remaining “per-provider flag” wording. [Google SMTP/Sent guidance](https://support.google.com/mail/answer/78892?hl=en) |
| Authentication-Results trusted by `authserv-id` alone | **Applied.** [04:229](../04-data-safety.md) and 07 require a provider boundary stripping injected copies and cite RFC 8601. [RFC 8601, May 2019](https://www.rfc-editor.org/rfc/rfc8601.html) |
| `calamine` assigned DOCX/PPTX extraction | **Applied.** [05:196](../05-search.md) limits it to spreadsheets and names separate bounded ZIP/XML extraction. [calamine repository](https://github.com/tafia/calamine) |
| CASA only if “our servers” touch data | **Partial.** [06:33](../06-account-backends.md) improves the summary but retains “our servers”; [06:114](../06-account-backends.md) still says “only if our servers touch data,” and 07 overstates “diagnostics count.” Google’s condition is restricted-data access **from or through third-party servers**, including an ability to access it. **Inference:** content-free aggregate diagnostics do not automatically trigger that condition; obtain Google’s written determination for the actual data flows. [Google restricted-scope verification](https://developers.google.com/identity/protocols/oauth2/production-readiness/restricted-scope-verification) |
| Gmail billing treated as already priced/live | **Applied.** [06:114](../06-account-backends.md) identifies announced billing above 80M units/project/day, pending price/start and 90 days’ notice. Current quota documentation supports this as of **2026-10-11**. [Gmail quotas](https://developers.google.com/workspace/gmail/api/reference/quota) |
| Gmail CONDSTORE/QRESYNC presented as verified current capabilities | **Applied to status.** [03:71](../03-performance.md) and 06 mark current behavior **unverified** pending authenticated probing. Remove the residual unsupported inference at [03:225](../03-performance.md) that the contrary view “looks wrong”; historical rollout does not establish current per-account behavior. |
| Filtered Graph delta treated as complete archive discovery | **Applied.** [06:60](../06-account-backends.md) now states the 5,000-message filtered cap and unfiltered per-folder approach. Folder-inventory completeness still differs from acquired-body completeness. [Microsoft message delta, checked 2026-10-11](https://learn.microsoft.com/en-us/graph/delta-query-messages) |
| Microsoft managed consent exclusions | **Misapplied in breadth.** [06:26](../06-account-backends.md) and the response abbreviate the exclusions to `Mail.*`. Microsoft lists exact permissions; `Mail.Send` is not in that exclusion list, and multiple policies can authorize consent. Preserve the read/IMAP conclusion without inventing a wildcard ban. [Microsoft consent policies](https://learn.microsoft.com/en-us/entra/identity/enterprise-apps/manage-app-consent-policies) |
| RFC 9979 interpreted as scheduling control | **Applied.** [07:120](../07-standards.md) distinguishes registered keywords/attributes from a control protocol. [RFC 9979, May 2026](https://www.rfc-editor.org/rfc/rfc9979.html) |
| Vector storage arithmetic | **Applied.** [05:214](../05-search.md) gives approximately 0.51 GB float32 / 0.13 GB int8 for one million 128-dimensional vectors; these are payload estimates before index/metadata overhead. |
| Storage layout claimed to explain all mail-loss incidents | **Corrected in substance.** [04:43](../04-data-safety.md) includes the same-destination rule-move failure and an operation-level prevention. Do not reintroduce the universal layout claim when summarizing. [Tsai’s incident account and updates](https://mjtsai.com/blog/2019/10/11/mail-data-loss-in-macos-10-15/) |

Two additional refinements follow from current checks. [06:113](../06-account-backends.md) still marks IMAP bandwidth caps unverified: Google’s current **Workspace** documentation gives **2,500 MB/day download and 500 MB/day upload**, subject to change; do not silently generalize that source to consumer Gmail. [Google Workspace bandwidth limits, checked 2026-10-11](https://knowledge.workspace.google.com/admin/gmail/gmail-bandwidth-limits).

Also fix the Gmail decimal/hex identity representation described in Q1. Quota arithmetic does not by itself establish that thread-level calls or IMAP are the winning import strategy: [03:84](../03-performance.md) still leaps from quota limits to that conclusion without an equivalent bytes/time/coverage comparison.

### Accepted decisions still contradicted or missing

These are more consequential than wording because an implementer could follow the wrong recipe.

| Location | Remaining conflict | Smallest correction |
|---|---|---|
| [04:82](../04-data-safety.md), Mailsmith prevention row | Scheduled backup plus an unspecified append-only journal supposedly reconstructs all mutable metadata from objects | Replace with SQLite authority, coordinated backup generations, and stated metadata RPO; remove the rejected independent-journal guarantee. The backup API covers database snapshots, not external-object coordination. [SQLite backup API](https://www.sqlite.org/backup.html) |
| [04:127](../04-data-safety.md) | Intent fields/replay omit configuration generation, preconditions, dependencies, and reconciliation policy; “ensure hash H in B not A” survives | Incorporate revised D3; equal bytes do not authorize destroying an occurrence or undoing another client’s move. |
| [04:143](../04-data-safety.md) | Hash/Message-ID rematch followed by automatic resume | Ambiguous correspondence quarantines destructive intents; scoped provider evidence is required. |
| [04:150](../04-data-safety.md) | One clean sync expires tombstones; fixed 50/20% breaker remains | Replace with the revised evidence/retention policy and trace-informed breaker. |
| [04:197](../04-data-safety.md) | Narrative restore protection focuses on old outbox items | The invariant table mentions deletes, but the implementation recipe must explicitly quarantine **all outbound effects**, create a new incarnation, and reconcile before enabling mutation. |
| [04:181](../04-data-safety.md), [06:71](../06-account-backends.md) | The sending recipes omit frozen per-recipient results and still foreground direct Graph sendMail | Apply D3’s attempt/RCPT evidence and draft-based Graph submission with a persisted immutable ID; a missing Sent result must not authorize resubmission. [Microsoft’s draft-to-Sent correlation](https://learn.microsoft.com/en-us/graph/outlook-immutable-id) |
| [06:43](../06-account-backends.md) | Graph categories still included in mailbox membership despite the correct detail section | Separate `parentFolderId` from `categories`; keep provider-native scope when projecting flags. [Graph message model](https://learn.microsoft.com/en-us/graph/api/resources/message?view=graph-rest-1.0) |
| [05:185](../05-search.md) | Remote results merged by Message-ID / Graph `internetMessageId` | Use account/mailbox-scoped authoritative IDs or established locators; header correspondence remains a clue. [Graph immutable IDs](https://learn.microsoft.com/en-us/graph/outlook-immutable-id) |
| [07:93](../07-standards.md) | Plain-text alternative remains “authoritative for quoting” | Quote the displayed part and retain provenance; handle material alternative disagreement explicitly. [RFC 2046](https://www.rfc-editor.org/rfc/rfc2046.html) |
| [01:72](../01-architecture.md), [01:99](../01-architecture.md) | Cargo-only enforcement and positional-edit-first wording | Apply D4’s module check and snapshot/reset-first contract. |
| [03:158](../03-performance.md), [04:102](../04-data-safety.md) | Cold compression and object-size cutoff remain design recommendations | One uncompressed store first; retain measured switch conditions rather than two implementations. |
| [04:263](../04-data-safety.md), [07:171](../07-standards.md) | Day-one full simulation and broad launch extension matrix | Replace with the focused harness and chosen-provider capability list. |
| [05:53](../05-search.md), [05:230](../05-search.md) | Tantivy remains the leading prior | Mark FTS5 as the declared starting choice; retain the competitive gate. |
| [08:26](../08-macos-ui.md), [09:25](../09-linux.md) | “Mostly sandboxed” and immediate supported .deb survive | Apply actual sandbox qualification and one initial Linux packaging path. |
| [12:104](../12-markdown-first.md), [12:172](../12-markdown-first.md) | Unconditional HTML-paste conversion and 1,000-message gate differ from the response | Make lossy paste explicit with preview/fallback; reconcile 1,000 versus 2,000, participant count, and timing. |

The curated durability table also omits explicit `foreign_keys=ON` verification for every connection, despite D17 accepting it. Shared query representation still needs the allowed rule subset, “unknown” under negation, and explicit clock/timezone written into the implementation guidance. [SQLite foreign-key pragma](https://www.sqlite.org/pragma.html).

Finally, Graph’s move API itself is documented as creating a destination copy and removing the source. The short “copy plus delete” wording in 04 is not intrinsically false; the dangerous interpretation would be implementing generic client COPY/DELETE instead of the provider’s move operation and identity contract. [Graph move API](https://learn.microsoft.com/en-us/graph/api/message-move?view=graph-rest-1.0).

## 6. D17 pre-registration: authority and search separately

### Baseline and experiment contract

**Prior:** one qualified bundled SQLite implementation for authoritative state, initially FTS5 for derived search. This is not a commitment to reject all alternatives.

As checked **2026-10-11**, SQLite **3.54.0 was released 2026-10-09**; **3.51.3, released 2026-03-13**, contains the WAL-reset fix. The fixed-version floor is not a recommendation to ship whichever latest build is available; explicitly exclude withdrawn 3.52.0 and pin a tested build/source ID. [SQLite release history](https://www.sqlite.org/changes.html).

For the baseline, use `rusqlite` with the qualified bundle, WAL, `synchronous=FULL`, the qualified macOS full-sync path, foreign keys enabled before transactions on every connection, one scheduled writer/checkpoint owner, short reader snapshots, and no network I/O inside write transactions. Use `STRICT` tables where appropriate rather than treating SQLite’s default typing as an unavoidable defect. [SQLite pragmas](https://www.sqlite.org/pragma.html), [STRICT tables](https://www.sqlite.org/stricttables.html).

Register these parameters before inspecting new benchmark rankings:

- **Hardware:** an 8 GiB Apple-silicon reference Mac on macOS 27/APFS, plus the macOS 26 pilot floor; a named Ubuntu/ext4 machine for portable-core evidence. Record exact hardware, OS, filesystem, compiler, engine/build, settings, and power state.
- **Data:** consented distributions with 100,000-message ordinary and 1,000,000-message stress stores; independently flagged duplicates, multiple memberships, long threads, attachments, sparse/missing bodies, and private metadata. Fix extracted-text input at 2 GiB/20 GiB for the search runs, or revise it transparently before seeing results.
- **Load:** 200-row windows and interactive queries during three-account backfill; sustained 1,000 metadata changes/second with 10,000-change bursts; draft saves and checkpoint/backup work interleaved. Include an idle workload so throughput is not purchased with permanent background cost.
- **Comparison:** equivalent semantics, durability, extraction, and query requirements; at least five randomized runs, warm and cold separately, p50/p95/p99 plus confidence intervals, peak memory/disk, and foreground responsiveness.
- **Bounded tuning:** shortlist from the studies, then budget ten engineering days for baseline remediation and one serious alternative per authority/search track; do not implement every surveyed engine. Record indexing, batching, tokenizer, scheduling, and implementation effort, and separate engine faults from identical application-protocol bugs.

These are proposed benchmark targets. If the cohort requires a larger workload or different latency budget, change the registration with a reason **before** evaluating the pending results.

The available [Codex database study](../raw/database/codex.md) and [Devin database study](../raw/database/devin.md) are useful Linux/Python experiments, not equivalent macOS/Rust product qualification. Their microsecond comparisons do not settle full-text quality, power-loss safety, or end-to-end draft latency. I would not carry forward categorical claims that PostgreSQL necessarily reduces safety, or that immutable objects cannot corrupt.

### Authoritative-state hard gates

**Every candidate, including SQLite, must pass these before performance matters.**

| Requirement | Measurable acceptance |
|---|---|
| Acknowledged durability | Zero lost acknowledged transactions and zero references to missing published objects across every identified publication/commit cut and at least 10,000 seeded fault runs. Inject short writes, ENOSPC, flush errors, aborted commits, and dropped unflushed writes. |
| Semantic isolation | Zero violations of scoped identity, occurrence flags, uniqueness/foreign-key constraints, draft revisions, intent preconditions, or page-and-cursor atomicity in the conformance/model corpus. |
| Outbound safety | Zero automatic resubmissions of unknown attempts and zero outbound effects from restored queues; test sends, deletes, moves, rule actions, and repaired Sent filing. |
| Recovery | Ordinary restart to a useful mailbox: p95 ≤ 1 s; crash recovery: ≤ 2 s at 100k, ≤ 10 s at 1M, excluding network reconciliation. Corruption must be detected/quarantined without manufacturing missing mail or deleting evidence. |
| Backup/restore | Twenty-four interruption/restore scenarios recover verified objects and authoritative metadata within the registered RPO; interrupted generations are never published as complete. Use five minutes as the comparison’s backup RPO unless the owner selects another before testing. |
| Migration/export | Interrupted migration preserves a recoverable pre-migration generation; one supported format upgrade and clean-machine export/restore preserve exact registered counts, hashes, authored revisions, and targeting quarantine. |

The zero-loss requirement applies to acknowledged commits within the storage model; catastrophic database destruction is evaluated against the independent backup RPO. Checksums detect corruption but do not provide a replacement copy. SQLite’s optional `cksumvfs` is worth evaluating, not assuming is present or treating as recovery by itself. [SQLite checksum VFS](https://www.sqlite.org/cksumvfs.html).

A `kill -9` run tests process failure while kernel/device state can remain intact. It cannot establish power-loss safety. Require a fault model that discards unflushed writes and qualifies the actual macOS flush path, with physical power-loss evidence where available; label the limits of simulated evidence. [SQLite atomic-commit assumptions](https://www.sqlite.org/atomiccommit.html), [Apple fsync documentation](https://developer.apple.com/library/archive/documentation/System/Conceptual/ManPages_iPhoneOS/man2/fsync.2.html).

### Results that would replace SQLite for authority

I would choose a candidate passing all hard gates if **at least one** of these occurs:

1. **Safety:** a reproducible, product-relevant SQLite durability/isolation defect persists in the qualified fixed build, while the candidate passes the same fault case and wider corpus. A shared object-publication bug is a reason to repair that protocol or choose BLOBs, not evidence against SQLite.
2. **Latency:** after bounded SQLite tuning, acknowledged metadata/draft commits exceed **p95 100 ms or p99 250 ms**, or cached list windows exceed **p95 100 ms or p99 200 ms** under registered load; the candidate meets the budgets and improves the failing p99 by at least **30%**, with a paired 95% interval showing improvement.
3. **Recovery capability:** SQLite cannot meet a required corruption-recovery/RPO outcome using the agreed, bounded recovery design, while the candidate demonstrates that outcome—including independent recovery material and clean-machine restoration.
4. **Changed requirements:** a documented launch requirement demands sustained independent writers or an operationally owned service/PITR architecture that SQLite cannot meet within the budgets, and the candidate demonstrates the required deployment. This is a scope change requiring owner agreement, not a benchmark victory.

For every switch, permit at most **10% regression** in other foreground latency measures, require the candidate to meet the resource budget, and qualify packaging, signed upgrades, backup, key handling, diagnostics, and export on macOS and headless Linux. A fast prototype without a recoverable migration and a named maintenance owner does not win.

**Strongest argument against staying with SQLite:** a better embedded engine may offer simpler corruption detection, useful concurrency, and Rust integration without PostgreSQL’s service lifecycle. Take that seriously. **Architectural inference:** in this SQL-shaped design, choosing redb/LMDB/RocksDB moves responsibility for required secondary indexes, constraints, and query plans into Sweetbriar; raw key/value speed omits that work. [redb design](https://github.com/cberner/redb/blob/master/docs/design.md), [heed/LMDB](https://docs.rs/heed/latest/heed/), [RocksDB](https://github.com/facebook/rocksdb).

Do not collapse libSQL and Turso into one tested engine: libSQL is a SQLite fork, while the Turso repository describes a Rust rewrite. Register the exact engine, build, and operating mode. [libSQL](https://github.com/tursodatabase/libsql), [Turso](https://github.com/tursodatabase/turso). PostgreSQL remains eligible if it earns its lifecycle costs; major upgrades require a real migration procedure, not necessarily an annual upgrade imposed on every user. [PostgreSQL upgrade documentation](https://www.postgresql.org/docs/current/pgupgrade.html).

### Derived-search gates and switch criteria

Search can change while SQLite remains the authority. Compare an FTS5 index sharing the authority with a separately scheduled derived FTS5 database before attributing all writer contention to the search engine.

Freeze **100 judged retrieval tasks**, spanning addresses/order IDs, CJK, phrases/quoted text, attachments, and restrictive account/folder/date predicates. Use identical source extraction and completeness metadata; otherwise the experiment compares pipelines rather than engines.

| Requirement | Registered bar |
|---|---|
| Exact correctness | 100% of expected exact matches in the conformance corpus; zero account/filter leakage, mistaken identity merges, or silent “no matches” from incomplete coverage. |
| Relevance | Blind heldout judgments using nDCG@10 and task success; evaluate identifier/CJK subsets separately. |
| Foreground latency | Cached exact search p95 ≤ 100 ms and p99 ≤ 250 ms during the registered sync/index load; include actual filtered result-window construction. |
| Freshness | New ready-to-index content searchable at p95 ≤ 2 s; current flags/memberships enforced immediately through the authority overlay. Missing extraction remains visible. |
| Recovery | At least 10,000 seeded index/publication/replay faults cause no authority damage; stale or partial generations are detected, replayed, or rebuilt. |
| Rebuild/resources | From cached extracted text: 100k ≤ 15 min, 1M ≤ 2 h; peak rebuild disk ≤ 2.5× steady index size; incremental search/index RSS ≤ 512 MiB on the 8 GiB reference Mac. |

I would choose Tantivy or another search engine passing all gates if:

- It improves **nDCG@10 by at least 0.05 absolute**, with a paired 95% interval above zero, no exact/filter regression, and no decline in the registered critical identifier/CJK tasks; **or**
- FTS5 misses the latency/freshness budgets after bounded tuning, while the candidate meets them and reduces the failing latency tail by at least **30%**; **or**
- FTS5 cannot meet a mandatory identifier/CJK/query requirement within the spike, and the candidate passes the same frozen cases; **or**
- FTS5 breaches the resource budget, while a candidate meets it and reduces the offending memory/disk measure by at least **40%**, with no retrieval or foreground regression.

A fourfold speedup from an already imperceptible microsecond query does not independently trigger a switch. Conversely, better retrieval is sufficient even when both engines are fast: the search choice should not be held hostage to SQLite’s authority advantages.

As checked **2026-10-11**, DuckDB’s official FTS extension documentation says its index does not automatically update when input tables change. A candidate using it must demonstrate its refresh/rebuild strategy meets the same freshness and responsiveness bars; a static analytical benchmark is insufficient. [DuckDB full-text search](https://duckdb.org/docs/current/core_extensions/full_text_search.html).

## 7. What is settled, risks, and what remains open

### Resolved between Claude and me

These are agreed design positions, subject to implementation evidence.

| Decisions | Settled position |
|---|---|
| **D1** | Immutable acquired bytes with provenance/completeness; SQLite is mutable-state authority; derived data is rebuildable; coordinated backup generations and outbound-quarantined restore. One uncompressed file store initially, with a measured BLOB escape. |
| **D2** | Scoped provider identity and occurrence semantics; Graph categories differ from folders; Message-ID cannot authorize merging or mutation. A focused conformance corpus precedes consumers. |
| **D3** | Durable, preconditioned intents; ambiguous rematches quarantine; no unsafe expunge; frozen attempts and per-recipient submission evidence; unknown submission never retries automatically; Sent filing is separate; focused fault harness before broad simulation. |
| **D4–D5** | Strict module layering with real enforcement; bounded snapshot/subscription API; native interaction and shared Rust application semantics. |
| **D6** | One owner/in-process pilot; extensions do not write; after-Quit behavior is a dated product decision, with ownership moved before launch if required. |
| **D7** | FTS5 starting prior; competitive search gate; typed queries with separate rule subsets/completeness semantics; capability-qualified remote search; text syntax/chips first. |
| **D8–D9** | AppKit prior tested against SwiftUI; composer, accessibility, and hostile reader tested early; macOS floor is policy. Linux core builds early; production UI and second package wait; GTK is a hypothesis. |
| **D10** | Import-first with immediate access work and a dated cohort/access gate; one initial transport/provider; Gmail identity independent of transport; no Exchange Online EWS launch path; Bridge first for Proton. |
| **D11** | Small local rule vocabulary with previews/traces/ledger; server programs separate; translators and dangerous actions deferred. |
| **D12–D13** | Direct signed distribution, actual sandbox qualification, migration-safe updates, recovery after expiry; dependency compatibility and asset inventory rather than permissive-only slogans. |
| **D14–D15** | Four bounded classic workflows and health/activity; broad revival catalog deferred. AI optional, explicit privacy modes, no silent cloud fallback, outside pilot critical path. |
| **D16** | Acquisition, structured reading projection, and compose source are distinct; plain text stays literal; conditional Reading defaults; sanitized outgoing alternatives, raw HTML disabled in composing dialect. |
| **D17** | Bundled SQLite is the authority prior, separately from FTS5 search; fixed runtime build, explicit durability/foreign keys, writer/checkpoint ownership, short transactions; alternatives judged by registered results. |

For D15, the **4,096-token** limit belongs to the on-device model’s total context, including instructions/tool material and generated content, not every future cloud model. Apple’s 2026 Foundation Models updates distinguish on-device and Private Cloud Compute capabilities; keep model selection and authorization explicit. [TN3193, checked 2026-10-11](https://developer.apple.com/documentation/technotes/tn3193-managing-the-on-device-foundation-model-s-context-window), [Apple Foundation Models updates](https://developer.apple.com/documentation/updates/foundationmodels).

### Genuinely open

- **D18–D21:** licence/contribution rights, sustainable funding/expiry, actual cohort/team/runway, and supported organizational/PIM/platform scope.
- **D24–D25:** threat model, protection/key custody, archival versus cache behavior, erasure/retention policy, and wording consistent with the public promise.
- **D17:** qualified authority engine, file versus BLOB evidence, search engine and placement, corruption-recovery/RPO design, and actual benchmark/fault outcomes.
- **D10:** which live backend wins the access/demand gate; own-app permissions and approval outcomes are still evidence to collect.
- **D6/D8/D12:** sync after Quit, cohort-supported macOS floor, prototype-winning UI implementation, and the actual sandbox integration result.
- **D16:** which structures/classes earn default Reading view, validated corpus/user outcomes, converter-version policy, and independent search completeness.
- **Delivery:** staffing-based scope and milestone dates, real weekly use, paid demand, provider-test ownership, and support capacity.

Cross-device sync, CardDAV, server-rule translation, Linux UI, AI, and the larger classic workbench are **deferred scope**, not unresolved prerequisites. Reopen them when a funded cohort requires them.

### Material risks

1. **Safety language exceeding evidence.** Engine tests, immutable files, hashes, or a successful restore drill alone cannot prove the complete acknowledgement/backup/remote-effects contract.
2. **Contradictory documents becoming code.** The residual reconstruction and rematching recipes could implement precisely the failures the response now rejects.
3. **Access without adoption, or adoption without maintenance funding.** Three consenting tenants and a useful archive are different evidence from sustained replacement-client use.
4. **Presentation completeness contaminating retrieval or decisions.** Preserve source provenance and searchable content independently from reading simplification.
5. **Storage/key failure during recovery.** Full disks, missing keys, interrupted migrations, and expired licences must preserve evidence and recovery access rather than escalating into deletion.

## 8. Sources consulted

All external sources below were consulted for this review on **2026-10-11**. Claims are linked inline; this list groups the underlying URLs for reuse. Where direct fetching failed, primary-source search excerpts or the successful canonical replacement were used; no unverified fetched page is treated as implementation evidence.

- **Review material:** [Claude response](../debate/02-claude-response.md), [Codex round one](../debate/01-review-codex.md), [Devin round one](../debate/01-review-devin.md), [original position](../debate/00-position-paper.md), curated research 01–12, [decision agenda](../notes/decision-agenda.md), and the two database studies linked above.
- **Microsoft:** [consent policies](https://learn.microsoft.com/en-us/entra/identity/enterprise-apps/manage-app-consent-policies), [message delta](https://learn.microsoft.com/en-us/graph/delta-query-messages), [immutable IDs](https://learn.microsoft.com/en-us/graph/outlook-immutable-id), [message schema](https://learn.microsoft.com/en-us/graph/api/resources/message?view=graph-rest-1.0), [move API](https://learn.microsoft.com/en-us/graph/api/message-move?view=graph-rest-1.0), [Outlook for Mac](https://www.microsoft.com/en-us/microsoft-365/outlook/outlook-for-mac).
- **Google:** [Gmail quotas](https://developers.google.com/workspace/gmail/api/reference/quota), [restricted-scope verification](https://developers.google.com/identity/protocols/oauth2/production-readiness/restricted-scope-verification), [IMAP extensions](https://developers.google.com/workspace/gmail/imap/imap-extensions), [Workspace bandwidth limits](https://knowledge.workspace.google.com/admin/gmail/gmail-bandwidth-limits), [SMTP/Sent guidance](https://support.google.com/mail/answer/78892?hl=en).
- **Apple:** [Mail import/export](https://support.apple.com/en-euro/guide/mail/mlhlp1030/mac), [Contacts access](https://developer.apple.com/documentation/contacts/accessing-the-contact-store?changes=_11_2&language=objc), [TN3193](https://developer.apple.com/documentation/technotes/tn3193-managing-the-on-device-foundation-model-s-context-window), [Foundation Models updates](https://developer.apple.com/documentation/updates/foundationmodels), [fsync manual](https://developer.apple.com/library/archive/documentation/System/Conceptual/ManPages_iPhoneOS/man2/fsync.2.html).
- **SQLite:** [release history](https://www.sqlite.org/changes.html), [WAL](https://www.sqlite.org/wal.html), [pragmas](https://www.sqlite.org/pragma.html), [backup API](https://www.sqlite.org/backup.html), [STRICT tables](https://www.sqlite.org/stricttables.html), [FTS5](https://www.sqlite.org/fts5.html), [checksum VFS](https://www.sqlite.org/cksumvfs.html), [atomic commit](https://www.sqlite.org/atomiccommit.html).
- **Other engines:** [Tantivy](https://github.com/quickwit-oss/tantivy), [DuckDB FTS](https://duckdb.org/docs/current/core_extensions/full_text_search.html), [libSQL](https://github.com/tursodatabase/libsql), [Turso](https://github.com/tursodatabase/turso), [redb design](https://github.com/cberner/redb/blob/master/docs/design.md), [heed/LMDB](https://docs.rs/heed/latest/heed/), [RocksDB](https://github.com/facebook/rocksdb), [PostgreSQL upgrades](https://www.postgresql.org/docs/current/pgupgrade.html), [SQLCipher design](https://www.zetetic.net/sqlcipher/design/).
- **Standards/fidelity:** [RFC 2046](https://www.rfc-editor.org/rfc/rfc2046.html), [RFC 3030](https://www.rfc-editor.org/rfc/rfc3030.html), [RFC 5321](https://www.rfc-editor.org/rfc/rfc5321.html), [RFC 8601](https://www.rfc-editor.org/rfc/rfc8601.html), [RFC 9394](https://www.rfc-editor.org/rfc/rfc9394.html), [RFC 9979](https://www.rfc-editor.org/rfc/rfc9979.html), [W3C tables](https://www.w3.org/WAI/tutorials/tables/), [CommonMark raw HTML](https://spec.commonmark.org/0.31.2/#raw-html).
- **Rust, advisories, and products:** [Cargo metadata](https://doc.rust-lang.org/cargo/commands/cargo-metadata.html), [Rust visibility](https://doc.rust-lang.org/reference/visibility-and-privacy.html), [Rust panics](https://doc.rust-lang.org/book/ch09-01-unrecoverable-errors-with-panic.html), [UniFFI changelog](https://github.com/mozilla/uniffi-rs/blob/main/CHANGELOG.md), [Stalwart advisory](https://github.com/stalwartlabs/stalwart/security/advisories/GHSA-jm95-876q-c9gw), [calamine](https://github.com/tafia/calamine), [Proton desktop package](https://github.com/ProtonMail/WebClients/blob/main/applications/inbox-desktop/package.json), [Tsai’s mail-loss account](https://mjtsai.com/blog/2019/10/11/mail-data-loss-in-macos-10-15/).
- **Licences:** [MPL FAQ](https://www.mozilla.org/en-US/MPL/2.0/FAQ/), [GPL FAQ](https://www.gnu.org/licenses/gpl-faq.en.html), [Qt licensing](https://doc.qt.io/qt-6/licensing.html), [GTK COPYING](https://github.com/GNOME/gtk/blob/main/COPYING).
