use super::Error;
use crate::PresentationError;

use crate::{Backend, Checkpoint, Frame};

use tokio::sync::Notify;

use std::sync::{Arc, Mutex, MutexGuard};

/// Selects repaint damage for frames combined before reception.
#[derive(Debug, Clone, Copy, Default)]
pub enum Coalescing {
    /// Requests a full repaint whenever pending frames are combined.
    #[default]
    FullRepaint,
    /// Combines incremental damage, falling back to a full repaint when needed.
    ///
    /// Uses [`Frame::coalesce_incremental()`] with the handler's event and grid
    /// limits. Each surviving grid has at most one bounding rectangle.
    Incremental,
}

struct Pending {
    frame: Option<Frame>,
    checkpoint: Option<Checkpoint>,
    error: Option<Error>,
    closed: bool,
    receiver_alive: bool,
    coalescing: Coalescing,
}

struct Shared {
    pending: Mutex<Pending>,
    ready: Notify,
}

impl Shared {
    fn pending(&self) -> MutexGuard<'_, Pending> {
        self.pending.lock().unwrap_or_else(|e| e.into_inner())
    }
}

pub(super) struct Sender {
    shared: Arc<Shared>,
    max_events: usize,
    max_grids: usize,
}

impl Sender {
    pub(super) fn send(&self, frame: Frame, flushed: bool) -> Result<(), Error> {
        let mut pending = self.shared.pending();
        if !pending.receiver_alive {
            return Err(Error::ReceiverClosed);
        }
        if pending.closed {
            return Err(Error::Protocol(crate::ProtocolError::Failed));
        }
        let coalescing = pending.coalescing;
        let frame = if let Some(previous) = &mut pending.frame {
            match coalescing {
                Coalescing::FullRepaint => previous.coalesce(frame, self.max_events),
                Coalescing::Incremental => {
                    previous.coalesce_incremental(frame, self.max_events, self.max_grids)
                }
            }
            .map_err(Error::Protocol)?;
            previous
        } else {
            pending.frame.insert(frame)
        };
        if flushed {
            pending.checkpoint = Some(Checkpoint::from_committed(Arc::clone(&frame.state)));
        }
        drop(pending);
        self.shared.ready.notify_one();
        Ok(())
    }

    pub(super) fn fail(&self, error: Error) {
        let mut pending = self.shared.pending();
        if pending.error.is_none() {
            pending.error = Some(error);
        }
        pending.closed = true;
        drop(pending);
        self.shared.ready.notify_one();
    }
}

impl Drop for Sender {
    fn drop(&mut self) {
        self.shared.pending().closed = true;
        self.shared.ready.notify_one();
    }
}

/// Receives stable UI frames from [`super::UiHandler`].
///
/// Frames remain in receive order. Intermediate states may be coalesced when
/// presentation is slower than redraw processing; their ordered events are
/// preserved. Coalescing requests a full repaint by default. Select
/// [`Coalescing::Incremental`] through [`Self::set_coalescing()`] to preserve
/// bounded incremental damage. Await the plugin driver separately for
/// connection failures.
pub struct Frames {
    shared: Arc<Shared>,
    finished: bool,
}

impl Frames {
    pub(super) fn channel(
        max_events: usize,
        max_grids: usize,
        checkpoint: Option<Checkpoint>,
    ) -> (Sender, Self) {
        let shared = Arc::new(Shared {
            pending: Mutex::new(Pending {
                frame: None,
                checkpoint,
                error: None,
                closed: false,
                receiver_alive: true,
                coalescing: Coalescing::default(),
            }),
            ready: Notify::new(),
        });
        (
            Sender {
                shared: Arc::clone(&shared),
                max_events,
                max_grids,
            },
            Self {
                shared,
                finished: false,
            },
        )
    }

    /// Exports the latest successfully queued flush or the restored checkpoint.
    ///
    /// Returns `None` before the first flush of a fresh UI. The checkpoint can
    /// be newer than the last frame received by the application and remains
    /// available after the stream ends.
    pub fn checkpoint(&self) -> Option<Checkpoint> {
        self.shared.pending().checkpoint.clone()
    }

    /// Selects how future pending-frame merges combine repaint damage.
    ///
    /// The default is [`Coalescing::FullRepaint`]. Changing the policy does not
    /// recompute damage already queued; a pending full repaint remains required.
    /// Events retain their receive order and the same configured event limit.
    pub fn set_coalescing(&mut self, coalescing: Coalescing) {
        self.shared.pending().coalescing = coalescing;
    }

    /// Waits for the next frame or terminal UI processing error.
    ///
    /// Returns `None` after the handler is dropped and the pending frame has
    /// been drained, or after returning a terminal UI processing error. A
    /// pending valid frame is delivered before that error. Transport errors
    /// and handler failures are reported by the plugin driver instead.
    /// Cancelling this wait does not consume a frame.
    pub async fn recv(&mut self) -> Option<Result<Frame, Error>> {
        if self.finished {
            return None;
        }
        loop {
            let ready = self.shared.ready.notified();
            {
                let mut pending = self.shared.pending();
                if let Some(frame) = pending.frame.take() {
                    return Some(Ok(frame));
                }
                if let Some(error) = pending.error.take() {
                    self.finished = true;
                    return Some(Err(error));
                }
                if pending.closed {
                    self.finished = true;
                    return None;
                }
            }
            // notify_one retains a permit when no receiver is waiting; a
            // publication between checking the slot and awaiting is observed.
            ready.await;
        }
    }

    /// Receives and presents one frame on the calling thread.
    ///
    /// Returns `false` when the stream has ended. A backend error consumes that
    /// frame and leaves the connection active. Handle the frame's events before
    /// returning an error if they must be retained. Recover with a full repaint
    /// or [`crate::State::diff_since()`] relative to the last painted state.
    pub async fn present_next<B: Backend + ?Sized>(
        &mut self,
        backend: &mut B,
    ) -> Result<bool, PresentationError<B::Error>> {
        let Some(frame) = self.recv().await else {
            return Ok(false);
        };
        backend
            .present(frame.map_err(|e| PresentationError::Session(e.into()))?)
            .map_err(PresentationError::Backend)?;
        Ok(true)
    }
}

impl Drop for Frames {
    fn drop(&mut self) {
        let mut pending = self.shared.pending();
        pending.receiver_alive = false;
        pending.frame = None;
        pending.error = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::{Damage, Event, GridId};

    use tokio::time::{Duration, timeout};

    use std::ptr;

    fn frame(sequence: u64) -> Frame {
        Frame {
            sequence,
            state: Arc::default(),
            damage: Damage::default(),
            events: vec![Event::SetTitle(sequence.to_string().into())],
        }
    }

    #[tokio::test]
    async fn coalescing_policy_preserves_cursor_updates_and_events() {
        for policy in [
            None,
            Some(Coalescing::FullRepaint),
            Some(Coalescing::Incremental),
        ] {
            let (sender, mut frames) = Frames::channel(2, 2, None);
            if let Some(policy) = policy {
                frames.set_coalescing(policy);
            }
            let mut first = frame(1);
            first.damage.cursor = true;
            let mut next = frame(2);
            next.damage.cursor = true;
            let state = Arc::clone(&next.state);
            sender.send(first, true).unwrap();
            sender.send(next, true).unwrap();

            let latest = frames.recv().await.unwrap().unwrap();
            assert_eq!(latest.sequence, 2);
            assert!(Arc::ptr_eq(&latest.state, &state));
            assert_eq!(
                latest.damage.full,
                !matches!(policy, Some(Coalescing::Incremental))
            );
            assert!(latest.damage.cursor);
            assert!(!latest.damage.layout);
            assert!(!latest.damage.widgets);
            assert_eq!(latest.damage.grids, []);
            assert_eq!(latest.damage.removed_grids, []);
            assert!(matches!(
                latest.events.as_slice(),
                [Event::SetTitle(first), Event::SetTitle(next)]
                    if first.as_ref() == "1" && next.as_ref() == "2"
            ));
        }
    }

    #[tokio::test]
    async fn policy_changes_affect_future_merges_without_recomputing_queued_damage() {
        let (sender, mut frames) = Frames::channel(3, 2, None);
        sender.send(frame(1), true).unwrap();
        sender.send(frame(2), true).unwrap();
        frames.set_coalescing(Coalescing::Incremental);
        sender.send(frame(3), true).unwrap();
        let latest = frames.recv().await.unwrap().unwrap();
        assert_eq!(latest.sequence, 3);
        assert_eq!(latest.events.len(), 3);
        assert!(latest.damage.full);

        sender.send(frame(4), true).unwrap();
        sender.send(frame(5), true).unwrap();
        frames.set_coalescing(Coalescing::FullRepaint);
        let latest = frames.recv().await.unwrap().unwrap();
        assert_eq!(latest.sequence, 5);
        assert!(!latest.damage.full);

        sender.send(frame(6), true).unwrap();
        sender.send(frame(7), true).unwrap();
        assert!(frames.recv().await.unwrap().unwrap().damage.full);
    }

    #[tokio::test]
    async fn incremental_event_limit_preserves_the_pending_frame() {
        let (sender, mut frames) = Frames::channel(1, 2, None);
        frames.set_coalescing(Coalescing::Incremental);
        let mut first = frame(1);
        first.damage.cursor = true;
        let state = Arc::clone(&first.state);
        sender.send(first, true).unwrap();
        assert!(matches!(
            sender.send(frame(2), true),
            Err(Error::Protocol(crate::ProtocolError::Limit(
                "pending events"
            )))
        ));
        assert!(ptr::eq(
            frames.checkpoint().unwrap().state(),
            state.as_ref()
        ));
        let pending = frames.recv().await.unwrap().unwrap();
        assert_eq!(pending.sequence, 1);
        assert!(Arc::ptr_eq(&pending.state, &state));
        assert!(pending.damage.cursor);
        assert!(!pending.damage.full);
        assert!(
            matches!(pending.events.as_slice(), [Event::SetTitle(title)] if title.as_ref() == "1")
        );
    }

    #[tokio::test]
    async fn incremental_grid_limit_falls_back_to_full_repaint() {
        let (sender, mut frames) = Frames::channel(2, 1, None);
        frames.set_coalescing(Coalescing::Incremental);
        let mut first = frame(1);
        first.damage.removed_grids.push(GridId(1));
        first.damage.layout = true;
        let mut next = frame(2);
        next.damage.removed_grids.push(GridId(2));
        next.damage.layout = true;
        sender.send(first, true).unwrap();
        sender.send(next, true).unwrap();
        let latest = frames.recv().await.unwrap().unwrap();
        assert_eq!(latest.sequence, 2);
        assert!(latest.damage.full);
        assert!(latest.damage.layout);
        assert!(latest.damage.removed_grids.len() <= 1);
    }

    #[tokio::test]
    async fn concurrent_delivery_never_overtakes_pending_events() {
        let (sender, mut frames) = Frames::channel(2000, 512, None);
        let producer = std::thread::spawn(move || {
            for sequence in 1..=2000 {
                sender.send(frame(sequence), true).unwrap();
            }
        });
        let mut events = Vec::new();
        let mut previous = 0;
        timeout(Duration::from_secs(2), async {
            while let Some(frame) = frames.recv().await {
                let frame = frame.unwrap();
                assert!(frame.sequence > previous);
                previous = frame.sequence;
                events.extend(frame.events.into_iter().map(|event| {
                    let Event::SetTitle(title) = event else {
                        panic!("expected title event");
                    };
                    title.to_string()
                }));
            }
        })
        .await
        .expect("pending frame was stranded after its sender stopped");
        producer.join().unwrap();
        assert_eq!(previous, 2000);
        assert_eq!(
            events,
            (1..=2000).map(|n| n.to_string()).collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn cancelling_reception_preserves_later_frames_and_pending_close() {
        let (sender, mut frames) = Frames::channel(2, 512, None);
        tokio::select! {
            result = frames.recv() => panic!("unexpected frame: {result:?}"),
            () = tokio::task::yield_now() => {}
        }
        sender.send(frame(1), true).unwrap();
        sender.send(frame(2), true).unwrap();
        drop(sender);
        let frame = frames.recv().await.unwrap().unwrap();
        assert_eq!(frame.sequence, 2);
        assert_eq!(frame.events.len(), 2);
        assert!(frames.recv().await.is_none());
        assert!(frames.recv().await.is_none());
    }
}
