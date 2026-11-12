//! Per-core test-only gates for real cancellation interleavings. No release
//! behavior or foreign export sees these pauses.

use std::future::{Future, poll_fn};
use std::pin::pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::Poll;

use tokio::sync::Notify;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Phase {
    Preparing,
    Prepared,
    Taken,
}

#[derive(Default)]
pub(crate) struct RestoreProbe {
    gate: Mutex<Option<(Phase, Arc<Gate>)>>,
}

#[derive(Default)]
pub(super) struct Gate {
    pub(super) reached: AtomicBool,
    release: Notify,
}

impl Gate {
    async fn pause(&self) {
        self.reached.store(true, Ordering::SeqCst);
        self.release.notified().await;
    }

    pub(super) fn release(&self) {
        self.release.notify_one();
    }
}

impl RestoreProbe {
    pub(super) fn arm(&self, phase: Phase) -> Arc<Gate> {
        let gate = Arc::new(Gate::default());
        *self.gate.lock().unwrap() = Some((phase, gate.clone()));
        gate
    }

    fn for_phase(&self, phase: Phase) -> Option<Arc<Gate>> {
        self.gate
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|(at, gate)| (*at == phase).then(|| gate.clone()))
    }

    pub(super) async fn wait(&self, phase: Phase) {
        if let Some(gate) = self.for_phase(phase) {
            gate.pause().await;
        }
    }

    /// Poll the real preparation once before pausing, retaining its future
    /// (or completed answer). A cancellation race must not abandon either.
    pub(super) async fn preparing<F: Future>(&self, work: F) -> F::Output {
        let Some(gate) = self.for_phase(Phase::Preparing) else {
            return work.await;
        };
        let mut work = pin!(work);
        let ready = poll_fn(|cx| {
            Poll::Ready(match work.as_mut().poll(cx) {
                Poll::Ready(value) => Some(value),
                Poll::Pending => None,
            })
        })
        .await;
        gate.pause().await;
        match ready {
            Some(value) => value,
            None => work.await,
        }
    }
}
