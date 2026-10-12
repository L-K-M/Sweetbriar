//! `sb-core` — L2–L4. The core API the app and CLI talk to, split into
//! `service` (L2), `engine` (L3) and `resources` (L4). Each layer talks
//! only to the layer directly below it (spec/01).

// Only `service` is public: L1 crates may import nothing else (spec/01).
mod engine;
mod resources;
pub mod service;
