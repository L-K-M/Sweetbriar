//! The Recovery cursor's shape: minted by the engine, decoded back,
//! and a foreign or malformed cursor refused.

use sb_resources::RecoveryKey;

use super::*;

#[test]
fn a_cursor_round_trips_through_its_tag_and_key() {
    let cursor = mint_cursor(123, 45);
    assert_eq!(cursor.as_str(), "sbr1|123|45");
    assert_eq!(
        decode_cursor(&cursor).unwrap(),
        RecoveryKey {
            last_seen_at: 123,
            occurrence_id: 45,
        }
    );

    for bad in [
        "",
        "sbw1|1|2",
        "sbr1|not-a-number|2",
        "sbr1|1",
        "sbr1|1|2|3",
    ] {
        let cursor = Cursor::from(bad.to_owned());
        assert!(decode_cursor(&cursor).is_err(), "{bad}");
    }
}
