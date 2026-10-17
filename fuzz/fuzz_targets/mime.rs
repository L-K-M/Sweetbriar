#![no_main]

use libfuzzer_sys::fuzz_target;
use sb_mime::{Limits, ParseFailure, ParseOutcome, parse};

fuzz_target!(|bytes: &[u8]| {
    let outcome = parse(bytes, &Limits::default());

    // Containment protects the app, but a caught panic is still a fuzz finding.
    assert!(
        !matches!(outcome, ParseOutcome::Failed(ParseFailure::Panic)),
        "parse panicked inside the containment boundary"
    );
});
