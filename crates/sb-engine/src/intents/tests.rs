//! The intent cursor's shape: minted by the engine, decoded back, and
//! a foreign or malformed cursor refused.

use sb_resources::IntentKey;

use super::*;

#[test]
fn a_cursor_round_trips_through_its_tag_and_key() {
    let cursor = mint_cursor(123, "intent-1");
    assert_eq!(cursor.as_str(), "sbi1|123|intent-1");
    assert_eq!(
        decode_cursor(&cursor).unwrap(),
        IntentKey {
            created_at: 123,
            intent_id: "intent-1".to_owned(),
        }
    );

    for bad in [
        "",
        "sbw1|1|a",
        "sbi1|not-a-number|a",
        "sbi1|1",
        "sbi1|1|a|b",
    ] {
        let cursor = Cursor::from(bad.to_owned());
        assert!(decode_cursor(&cursor).is_err(), "{bad}");
    }
}
