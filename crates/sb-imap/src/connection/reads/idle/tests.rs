use super::*;
use std::num::NonZeroU32;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::sync::oneshot;

fn context() -> MailboxContext {
    MailboxContext {
        name: "INBOX".into(),
        uid_validity: NonZeroU32::new(4100).unwrap(),
    }
}

#[test]
fn repeated_hints_borrow_one_long_name_but_are_delivered_each_time() {
    let context = MailboxContext {
        name: "m".repeat(100_000),
        ..context()
    };
    let frame = Frame {
        bytes: b"* 2 EXISTS\r\n".to_vec(),
        kind: FrameKind::Untagged,
    };
    let mut cache = Vec::new();
    let mut pointer = None;
    let mut delivered = 0;
    let mut notify = |event: IdleEvent<'_>| {
        if let IdleEvent::Notice(MailboxNotice::Changed {
            mailbox: Some(mailbox),
        }) = event
        {
            let address = mailbox.name.as_ptr() as usize;
            assert_eq!(*pointer.get_or_insert(address), address);
            assert_eq!(mailbox.name.len(), 100_000);
            delivered += 1;
        }
    };
    for _ in 0..1_000 {
        observe(&frame, &context, &mut cache, &mut notify).unwrap();
    }
    assert_eq!(cache.len(), 1);
    assert_eq!(delivered, 1_000);
}

#[tokio::test]
async fn an_active_idle_flood_hits_the_original_round_budget_without_done_or_eof() {
    let (mut client, peer) = tokio::io::duplex(1024);
    let peer = tokio::spawn(async move {
        let mut peer = BufReader::new(peer);
        peer.write_all(b"* OK ready\r\n").await.unwrap();
        let mut line = String::new();
        peer.read_line(&mut line).await.unwrap();
        assert!(line.ends_with(" IDLE\r\n"));
        peer.write_all(b"+ watching\r\n").await.unwrap();
        for _ in 0..32 {
            if peer.write_all(b"* 2 EXISTS\r\n").await.is_err() {
                return;
            }
        }
        // Keep the peer open; neither an EOF nor completion can cause failure.
        std::future::pending::<()>().await;
    });
    let mut gate = Gate::new();
    gate.read_frame(&mut client).await.unwrap().unwrap();
    let mut stop = std::pin::pin!(std::future::pending());
    let mut delivered = 0;
    let mut notify = |event: IdleEvent<'_>| {
        if matches!(event, IdleEvent::Notice(MailboxNotice::Changed { .. })) {
            delivered += 1;
        }
    };
    let result = tokio::time::timeout(
        Duration::from_secs(2),
        watch(
            &mut gate,
            &mut client,
            &context(),
            stop.as_mut(),
            &mut notify,
            128,
        ),
    )
    .await
    .unwrap();
    assert!(matches!(
        result,
        Err(ImapError::ResponseBudgetExceeded { operation: "IDLE" })
    ));
    assert!(
        delivered >= 4,
        "active-wait frames must deliver repeated hints"
    );
    peer.abort();
}

#[tokio::test(start_paused = true)]
async fn a_received_partial_tag_cannot_become_a_done_completion() {
    let (mut client, peer) = tokio::io::duplex(1024);
    let (stop_tx, stop_rx) = oneshot::channel();
    let (started_tx, started_rx) = oneshot::channel();
    let exchange = async move {
        let mut gate = Gate::new();
        gate.read_frame(&mut client).await.unwrap().unwrap();
        let mut stop = std::pin::pin!(async {
            let _ = stop_rx.await;
        });
        let mut signals = Some((stop_tx, started_tx));
        let mut notify = |event: IdleEvent<'_>| {
            if matches!(event, IdleEvent::Started { .. }) {
                let (stop, started) = signals.take().unwrap();
                stop.send(()).unwrap();
                started.send(()).unwrap();
            }
        };
        let result = watch(
            &mut gate,
            &mut client,
            &context(),
            stop.as_mut(),
            &mut notify,
            MAX_METADATA_BYTES,
        )
        .await;
        assert!(matches!(result, Err(ImapError::UnexpectedCompletion)));
    };
    let server = async move {
        let mut peer = BufReader::new(peer);
        peer.write_all(b"* OK ready\r\n").await.unwrap();
        let mut line = String::new();
        peer.read_line(&mut line).await.unwrap();
        let tag = line.split_whitespace().next().unwrap();
        peer.write_all(format!("+ watching\r\n{tag}").as_bytes())
            .await
            .unwrap();
        started_rx.await.unwrap();
        // The old tag prefix was received before DONE. Finish it only after
        // proving that the driver has not written through the partial frame.
        assert!(
            tokio::time::timeout(Duration::from_millis(1), peer.read_u8())
                .await
                .is_err()
        );
        peer.write_all(b" OK premature\r\n").await.unwrap();
        let mut rest = Vec::new();
        peer.read_to_end(&mut rest).await.unwrap();
        assert!(rest.is_empty(), "no DONE crossed the old completion");
    };
    tokio::time::timeout(Duration::from_secs(32), async {
        tokio::join!(exchange, server);
    })
    .await
    .unwrap();
}

struct CountReads {
    inner: tokio::io::ReadHalf<tokio::io::DuplexStream>,
    bytes: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}
impl AsyncRead for CountReads {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let before = buf.filled().len();
        let result = Pin::new(&mut self.inner).poll_read(cx, buf);
        if matches!(&result, Poll::Ready(Ok(()))) {
            self.bytes.fetch_add(
                buf.filled().len() - before,
                std::sync::atomic::Ordering::SeqCst,
            );
        }
        result
    }
}

#[tokio::test(start_paused = true)]
async fn stopping_cancels_an_active_partial_read_without_losing_its_prefix() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let (raw, peer) = tokio::io::duplex(1024);
    let (reader, writer) = tokio::io::split(raw);
    let bytes = Arc::new(AtomicUsize::new(0));
    let mut client = tokio::io::join(
        CountReads {
            inner: reader,
            bytes: bytes.clone(),
        },
        writer,
    );
    let (stop_tx, stop_rx) = oneshot::channel();
    let (started_tx, started_rx) = oneshot::channel();
    let exchange = async move {
        let mut gate = Gate::new();
        gate.read_frame(&mut client).await.unwrap().unwrap();
        let mut stop = std::pin::pin!(async {
            let _ = stop_rx.await;
        });
        let mut ready = Some(started_tx);
        let mut hints = 0;
        let mut notify = |event: IdleEvent<'_>| match event {
            IdleEvent::Started { .. } => ready.take().unwrap().send(()).unwrap(),
            IdleEvent::Notice(MailboxNotice::Changed { .. }) => hints += 1,
            _ => {}
        };
        watch(
            &mut gate,
            &mut client,
            &context(),
            stop.as_mut(),
            &mut notify,
            MAX_METADATA_BYTES,
        )
        .await
        .unwrap();
        assert_eq!(hints, 1);
    };
    let server = async move {
        let mut peer = BufReader::new(peer);
        peer.write_all(b"* OK ready\r\n").await.unwrap();
        let mut line = String::new();
        peer.read_line(&mut line).await.unwrap();
        let tag = line.split_whitespace().next().unwrap().to_owned();
        peer.write_all(b"+ watching\r\n").await.unwrap();
        started_rx.await.unwrap();
        let prefix = b"* 2 EX";
        let consumed = bytes.load(Ordering::SeqCst) + prefix.len();
        peer.write_all(prefix).await.unwrap();
        while bytes.load(Ordering::SeqCst) < consumed {
            tokio::task::yield_now().await;
        }
        stop_tx.send(()).unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(1), peer.read_u8())
                .await
                .is_err()
        );
        peer.write_all(b"ISTS\r\n").await.unwrap();
        line.clear();
        peer.read_line(&mut line).await.unwrap();
        assert_eq!(line, "DONE\r\n");
        peer.write_all(format!("{tag} OK stopped\r\n").as_bytes())
            .await
            .unwrap();
    };
    tokio::time::timeout(Duration::from_secs(32), async {
        tokio::join!(exchange, server);
    })
    .await
    .unwrap();
}
