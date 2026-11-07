//! Fixed-wire budget regressions. Peers deliberately keep the response
//! unfinished and the socket open: EOF cannot stand in for limit detection.
use super::*;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

async fn refuses_without_eof(payload: Vec<u8>, small_limit: Option<u32>) {
    let (mut client, mut peer) = tokio::io::duplex(8192);
    let reader = async move {
        let mut gate = Gate::new();
        if let Some(limit) = small_limit {
            gate.fragmentizer = Fragmentizer::new(limit);
        }
        assert_eq!(gate.read(&mut client).await.unwrap(), b"* OK ready\r\n");
        client.write_all(b"g").await.unwrap();
        for _ in 0..2 {
            assert!(matches!(
                gate.read(&mut client).await,
                Err(ImapError::FrameBudgetExceeded)
            ));
            assert!(gate.poisoned());
            assert!(!gate.reusable());
            assert!(gate.has_plaintext_residual());
        }
    };
    let sender = async move {
        peer.write_all(b"* OK ready\r\n").await.unwrap();
        let mut ack = [0];
        peer.read_exact(&mut ack).await.unwrap();
        assert_eq!(ack, *b"g");
        peer.write_all(&payload).await.unwrap();
        let mut byte = [0];
        assert_eq!(
            peer.read(&mut byte).await.unwrap(),
            0,
            "reader closes without a response end or EOF"
        );
    };
    tokio::time::timeout(Duration::from_secs(30), async {
        tokio::join!(reader, sender);
    })
    .await
    .expect("budget detection cannot wait for an unfinished frame");
}

#[tokio::test]
async fn an_oversized_literal_announcement_fails_before_any_literal_bytes() {
    let payload = format!(
        "* 1 FETCH (BODY[] {{{}}}\r\n",
        u64::from(MAX_MESSAGE_SIZE) + 1
    )
    .into_bytes();
    refuses_without_eof(payload, None).await;
}

#[tokio::test]
async fn a_budget_failure_cannot_release_a_preceding_buffered_completion() {
    let payload = format!(
        "A OK completed\r\n* 1 FETCH (BODY[] {{{}}}\r\n",
        u64::from(MAX_MESSAGE_SIZE) + 1
    )
    .into_bytes();
    refuses_without_eof(payload, None).await;
}

#[tokio::test]
async fn an_unfinished_line_fails_when_progress_returns_none() {
    let payload = format!("* OK {}", "x".repeat(65)).into_bytes();
    refuses_without_eof(payload, Some(64)).await;
}

#[tokio::test]
async fn a_complete_response_exactly_at_the_frame_limit_is_valid() {
    let mut gate = Gate::new();
    gate.fragmentizer = Fragmentizer::new(64);
    let line = format!("* OK {}\r\n", "x".repeat(57));
    assert_eq!(line.len(), 64);
    let bytes = [b"* OK ready\r\n".as_slice(), line.as_bytes()].concat();
    let mut wire = bytes.as_slice();
    assert_eq!(gate.read(&mut wire).await.unwrap(), b"* OK ready\r\n");
    assert_eq!(gate.read(&mut wire).await.unwrap(), line.as_bytes());
    assert!(!gate.poisoned());
    assert!(gate.read(&mut wire).await.unwrap().is_empty());
}
