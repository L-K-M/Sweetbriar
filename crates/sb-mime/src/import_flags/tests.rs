use super::*;

#[test]
fn extracts_read_and_extended_flags() {
    let message = b"From: sender@example.test\r\nsTaTuS: RO\r\nX-sTaTuS: FADT\r\n\r\nBody\r\n";
    let flags = import_flags(message, &Limits::default());
    assert_eq!(flags.seen(), Some(true));
    assert!(flags.flagged() && flags.answered() && flags.deleted() && flags.draft());
    assert!(flags.is_complete());
}

#[test]
fn absent_headers_differ_from_present_unread_headers() {
    for value in [b"O".as_slice(), b"", b"unknown"] {
        let mut message = b"Status: ".to_vec();
        message.extend_from_slice(value);
        message.extend_from_slice(b"\n\nBody\n");
        assert_eq!(
            import_flags(&message, &Limits::default()).seen(),
            Some(false)
        );
    }
    let absent = import_flags(b"Subject: archive\n\nBody\n", &Limits::default());
    assert_eq!(absent.seen(), None);
    assert!(absent.is_complete());
    let extended = import_flags(b"X-Status: F\r\n\r\nbody\n", &Limits::default());
    assert_eq!(extended.seen(), Some(false));
    assert!(extended.flagged());
    assert!(extended.is_complete());
    assert_eq!(
        import_flags(b"X-Status: R\n\n", &Limits::default()).seen(),
        Some(true)
    );
    assert_eq!(
        import_flags(b" \tStatus: O\n\n", &Limits::default()).seen(),
        Some(false)
    );
}

#[test]
fn combines_duplicates_and_folds_as_fields_not_physical_lines() {
    let message = b"Status: O\n\tR\nX-Status: F\n A\n\tD T\nStatus: O\n\n";
    let flags = import_flags(message, &Limits::default().with_max_header_fields(3));
    assert_eq!(flags.seen(), Some(true));
    assert!(flags.flagged() && flags.answered() && flags.deleted() && flags.draft());
    assert!(flags.is_complete());
    let limited = import_flags(message, &Limits::default().with_max_header_fields(1));
    assert_eq!(limited.seen(), Some(true));
    assert!(!limited.flagged());
    assert!(!limited.is_complete());
}

#[test]
fn body_nested_and_continuation_lookalikes_do_not_set_flags() {
    for message in [
        b"Subject: test\n\nStatus: R\nX-Status: FADT\n".as_slice(),
        b"Subject: test\n X-Status: FADT\n\n",
        b"Bad field\n Status: R\n\n",
        b"X-Status-Other: FADT\n\n",
        b"Subject: test\n \nStatus: R\n\n",
    ] {
        let flags = import_flags(message, &Limits::default());
        assert_eq!(flags.seen(), None);
        assert!(!flags.flagged() && !flags.answered() && !flags.deleted() && !flags.draft());
        assert!(flags.is_complete());
    }
    let status_only = import_flags(b"Status: RFADT\n\n", &Limits::default());
    assert_eq!(status_only.seen(), Some(true));
    assert!(!status_only.flagged() && !status_only.answered());
}

#[test]
fn byte_field_and_message_limits_preserve_only_admitted_facts() {
    let message = b"Status: O\nX-Status: FADT\n\n";
    for limits in [
        Limits::default().with_max_header_bytes(b"Status: O\nX-Status: FA".len()),
        Limits::default().with_max_message_bytes(b"Status: O\nX-Status: FA".len()),
        Limits::default().with_max_header_fields(1),
    ] {
        let flags = import_flags(message, &limits);
        assert_eq!(flags.seen(), Some(false));
        assert!(!flags.flagged() && !flags.answered() && !flags.deleted() && !flags.draft());
        assert!(!flags.is_complete());
    }
    let limited = import_flags(message, &Limits::default().with_max_header_fields(0));
    assert_eq!(limited.seen(), None);
    assert!(!limited.is_complete());
    let header = b"Status: R\n\n";
    let flags = import_flags(
        b"Status: R\n\nBody",
        &Limits::default()
            .with_max_header_bytes(header.len())
            .with_max_header_fields(1),
    );
    assert_eq!(flags.seen(), Some(true));
    assert!(flags.is_complete());
}

#[test]
fn truncated_empty_and_non_ascii_values_are_not_complete_headers() {
    for message in [b"".as_slice(), b"Status: R", b"X-Status: FADT"] {
        let flags = import_flags(message, &Limits::default());
        assert_eq!(flags.seen(), None);
        assert!(!flags.is_complete());
    }
    let flags = import_flags(b"Status: \xff\0r\nX-Status: fad\n\n", &Limits::default());
    assert_eq!(flags.seen(), Some(false));
    assert!(!flags.flagged() && !flags.answered() && !flags.deleted());
    assert!(flags.is_complete());
}
