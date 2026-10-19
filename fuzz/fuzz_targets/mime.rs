#![no_main]

use libfuzzer_sys::fuzz_target;
use sb_mime::{
    AttachmentError, Limits, ParseFailure, ParseOutcome, extract_attachment, import_flags, parse,
    render_input,
};

fuzz_target!(|bytes: &[u8]| {
    let outcome = parse(bytes, &Limits::default());

    // Containment protects the app, but a caught panic is still a fuzz finding.
    assert!(
        !matches!(outcome, ParseOutcome::Failed(ParseFailure::Panic)),
        "parse panicked inside the containment boundary"
    );

    let reader = render_input(bytes, &Limits::default());
    assert!(!matches!(reader, Err(ParseFailure::Panic)));
    if let Ok(reader) = reader
        && let Some(attachment) = reader.attachments().first()
    {
        let extracted =
            extract_attachment(bytes, attachment.part_id().as_str(), &Limits::default());
        assert!(!matches!(
            extracted,
            Err(AttachmentError::Parse(ParseFailure::Panic))
        ));
    }

    for limits in [
        Limits::default(),
        Limits::default()
            .with_max_header_bytes(bytes.first().copied().unwrap_or(0) as usize)
            .with_max_header_fields(bytes.get(1).copied().unwrap_or(0) as usize),
    ] {
        let flags = std::hint::black_box(import_flags(bytes, &limits));
        assert!(
            flags.seen().is_some()
                || !(flags.flagged() || flags.answered() || flags.deleted() || flags.draft())
        );
    }
});
