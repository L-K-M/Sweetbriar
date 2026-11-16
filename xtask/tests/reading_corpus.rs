//! `cargo xtask reading-corpus` counts a synthetic corpus and, above
//! all, never answers with message text.

use std::fs;
use std::path::PathBuf;

use xtask::reading_corpus::count_path;

const ALPHA_SUBJECT: &str = "Corpus-Alpha-Subject";
const ALPHA_BODY: &str = "Corpus-Alpha-Body";
const BETA_SUBJECT: &str = "Corpus-Beta-Subject";
const BETA_BODY: &str = "Corpus-Beta-Body";

fn scratch() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("xtask-reading-corpus-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn a_two_message_mbox_reports_counts_and_no_message_text() {
    let dir = scratch();
    let mbox = dir.join("corpus.mbox");
    let source = [
        format!(
            "From sender@example.test Sat Oct 17 12:00:00 2026\n\
             Subject: {ALPHA_SUBJECT}\nContent-Type: text/plain\n\n{ALPHA_BODY}\n"
        ),
        format!(
            "From sender@example.test Sat Oct 17 12:00:00 2026\n\
             Subject: {BETA_SUBJECT}\nContent-Type: text/html\n\n\
             <table><tr><td>{BETA_BODY}</td></tr></table>\n"
        ),
    ]
    .concat();
    fs::write(&mbox, source).unwrap();

    let counts = count_path(&mbox).unwrap();
    assert_eq!(counts.total, 2);
    assert_eq!(counts.supported, 1);
    assert_eq!(counts.unsupported, 1);
    assert_eq!(counts.parse_failures, 0);

    let report = counts.report();
    assert_eq!(
        report,
        "total: 2\nsupported: 1\nunsupported: 1\nparse_failures: 0\n"
    );
    for secret in [ALPHA_SUBJECT, ALPHA_BODY, BETA_SUBJECT, BETA_BODY] {
        assert!(
            !report.contains(secret),
            "the counts report must not carry message text: {report:?}"
        );
    }
    let _ = fs::remove_dir_all(&dir);
}
