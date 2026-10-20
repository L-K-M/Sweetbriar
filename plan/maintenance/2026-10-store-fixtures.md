# Consolidate existing Store test fixtures

## Goal and evidence

Index/publication and rebuild tests duplicate migrated connections,
acquisition rows and default documents. List and list-row tests duplicate
archive-account lookup, mailbox and logical-message insertion. Share those
mechanics with their existing consumers before adding Search predicate proof.
This is an independently useful maintenance step, not a partial compiler.

The first predicate draft exceeded its review budget and remains archived.
Its passing focused baseline did not cover every required control. This step
does not move compiler acceptance into fixtures, promise a later size fit or
relax the final SQL/completeness/corruption contract. Re-size all of M1.6h
after this prerequisite is qualified.

## Scope and preservation boundary

- Use the existing test-only `sb-store::testing` infrastructure, with private
  or crate-private helpers. Every extraction has at least two existing test
  consumers immediately; no unused builders or configurable fixture framework.
- Share migrated in-memory connection construction and explicit migration
  depth for index/rebuild tests. Preserve pragma ordering, foreign-key and
  trusted-schema settings and each consumer's migration selection.
- Share duplicated acquisition/default-document factories with exact ids,
  content hashes, byte lengths, object states, versions and column defaults.
- Share identical account lookup, mailbox and logical-message insertion for
  list/list-row tests. Preserve arbitrary existing fixture ids and the
  optional-acquisition distinction; do not replace fixtures to suit future H.
- Keep occurrence adapters local: their transaction, projection refresh,
  representative election and generation responsibilities differ. Do not
  introduce hidden refresh/generation changes through a generic helper.
- Preserve every existing test name, case and assertion. No production code,
  public API, compiler, schema, dependency, toolchain, policy or workflow change.
  No relocation of future compiler source/corruption proof into this step.

## Acceptance and review budget

- Establish the affected Store suites' baseline before structural edits.
- Run the same index/rebuild/list/list-row proof afterward. Check shared
  factories' identities, hashes, lengths/states, pragma and migration behavior;
  preserve independent occurrence/projection/election/generation checks.
- Run `cargo xtask ci`, independent review and actual current named `ci` and
  `macos` gates before merge. No owner-Mac/Bridge/pilot qualification implied.
- Expected350-600 additions plus deletions including moved code twice, tests,
  call sites and this plan; hard1450 per focused plan-step PR. Do not remove
  proof or compress formatting to fit. A fresh complete-H estimate follows.
