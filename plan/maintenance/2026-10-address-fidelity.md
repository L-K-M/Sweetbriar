# Preserve address source identity before predicate evaluation

## Evidence and goal

MailboxAddress::address promises the addr-spec as written. The owner selected
local-as-written search/seen-address keys, including quoting and quoted pairs.
The current mail-parser0.11.9 recovery path changes valid quoted locals: a
bare quoted local loses its local part; quoted pairs inside angle addresses
lose backslashes/content. A synthetic public sb-mime probe reproduced three
changes without warnings. First reproduce with the repository's exact lock and
containment profile; do not treat a differently resolved research graph as that
regression proof. Fix this producer before trusting exact-address predicates.

## Scope

- Preserve valid modern quoted-local addr-spec spelling from source, including
  quotes, spaces, quoted-pair backslashes, case and Unicode. Keep display-name
  decoding, address-list/group order and ordinary mailbox recovery compatible.
- Repair the narrow upstream tokenizer behavior rather than introducing a
  second RFC header parser in the application. Latest published mail-parser
  remains0.11.9; upstream's broad unreleased1.0 rewrite is outside this slice.
  A minimal release-based backport must have immutable provenance, complete
  patch review and actual parser/MIME tests before application integration.
- If a Git patch is needed, pin its tested full revision, allow only its named
  repository, regenerate lock/notices and retain the release baseline. No
  mutable branch pin, local path patch, broad Git allowance or advisory ignore.
  Include the external patch in the1450 additions-plus-deletions scope.
  Release baseline:918cc0c891f5e2a0009ccab4e935e91f68c673be (v0.11.9,
  registry VCS provenance verified). Tested immutable backport:
  https://github.com/BigBoyDevBox/mail-parser at
  41cbc85ce90b52ec33c7c7285f714fcdbcbc2928; two files,253 additions/7 deletions,
  including15 exact-source fixtures. The existing source/name/list/group
  fixtures stay unchanged. Retain this branch until a fixed registry update.
- Keep mail-parser's existing containment profile and safety budgets; no
  SMTP/IMAP behavior, sending, schema layout, source-byte mutation or unrelated
  parser/body/rendering cleanup. Source inputs and fixtures are synthetic.

## Version and compatibility path

Changing address extraction changes summaries and search columns. Bump MIME
PARSER_VERSION and SEARCH_EXTRACTOR_VERSION from1to2. Existing source blobs,
authoritative rows and old summaries remain; no destructive migration. The
existing target reconciliation/rebuild driver recaptures FTS under the newer
extractor with its current fences, holds, crash and no-downgrade guarantees.

Exact-address predicates must not trust legacy version1 summaries whose
spelling may already be lost: those fields remain unknown until refreshed.
The M1.6m summary/address backfill owns bounded, resumable parser-version refresh
from retained acquisitions before ledger population. Complete synced-header
sources retain their existing precedence. General readers may keep displaying
legacy metadata during this compatibility window; raw bytes remain available.
Rolling back to an older extractor must not downgrade newer target state.

## Acceptance

- Observe the exact-lock regression failing before the repair and passing after
  it; include bare/angle quoted locals, escapes, spaces, Unicode, group/list and
  display-name controls. Assert original values, not only syntactic validity.
- Run actual upstream address fixtures/tests and product MIME/hostile/fuzz
  regressions without weakening checks; preserve the existing parser profile.
- Verify the new extractor reconciles/rebuilds without changing source bytes or
  authoritative message identity, and retains newer-target/legacy safety.
- Exact-source diff/provenance, pinned dependency, notices and strict deny pass.
  Full local CI plus current named ci/macOS and completed reviews gate merge.
  No owner/physical-Mac/Bridge qualification is implied by CI.
