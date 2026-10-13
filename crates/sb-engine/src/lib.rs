//! `sb-engine` — L3. Runtime owner; sync scheduler; intents; outbox;
//! drafts; import; backup; index jobs; breaker; reconciliation;
//! appearance tokens. Depends only on `sb-resources`; leaf types reach
//! it through `sb-resources`' re-exports (spec/01 rules 1 and 3).
