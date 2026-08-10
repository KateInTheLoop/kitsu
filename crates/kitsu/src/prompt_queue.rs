//! A small bounded FIFO used by interactive frontends to own type-ahead.
//!
//! This module deliberately knows nothing about stdin, terminal rendering, or
//! agent turns. The frontend decides when a submitted line should run and uses
//! this queue only for explicit, visible buffering while another turn is busy.

use std::collections::VecDeque;
use std::error::Error;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};

use tokio::sync::mpsc;

/// Current queue occupancy, suitable for a prompt/status indicator.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PromptQueueStatus {
    pub queued: usize,
    pub capacity: usize,
}

impl PromptQueueStatus {
    pub fn remaining(self) -> usize {
        self.capacity.saturating_sub(self.queued)
    }

    pub fn is_empty(self) -> bool {
        self.queued == 0
    }

    #[cfg(test)]
    pub fn is_full(self) -> bool {
        self.queued == self.capacity
    }
}

impl fmt::Display for PromptQueueStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} / {} prompts queued", self.queued, self.capacity)
    }
}

/// A prompt refused because the queue was already at its configured capacity.
/// The refused value is retained so callers can display or otherwise recover it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PromptQueueFull {
    prompt: String,
    capacity: usize,
}

impl PromptQueueFull {
    pub fn prompt(&self) -> &str {
        &self.prompt
    }

    #[cfg(test)]
    pub fn into_prompt(self) -> String {
        self.prompt
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }
}

impl fmt::Display for PromptQueueFull {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "prompt queue is full (capacity {})", self.capacity)
    }
}

impl Error for PromptQueueFull {}

/// A queue position outside `0..len`, where zero is the next prompt to run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PromptQueueIndexError {
    pub index: usize,
    pub len: usize,
}

impl fmt::Display for PromptQueueIndexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "prompt queue index {} is out of range ({} queued)",
            self.index, self.len
        )
    }
}

impl Error for PromptQueueIndexError {}

/// An invalid edit that retains the replacement prompt for recovery.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PromptQueueEditError {
    pub position: PromptQueueIndexError,
    replacement: String,
}

impl PromptQueueEditError {
    pub fn replacement(&self) -> &str {
        &self.replacement
    }

    #[cfg(test)]
    pub fn into_replacement(self) -> String {
        self.replacement
    }
}

impl fmt::Display for PromptQueueEditError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.position.fmt(f)
    }
}

impl Error for PromptQueueEditError {}

/// Invalid source or destination supplied to [`PromptQueue::reorder`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PromptQueueMoveError {
    pub from: usize,
    pub to: usize,
    pub len: usize,
}

impl fmt::Display for PromptQueueMoveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "cannot move queued prompt from {} to {} ({} queued)",
            self.from, self.to, self.len
        )
    }
}

impl Error for PromptQueueMoveError {}

/// A bounded, first-in/first-out queue of submitted prompts.
///
/// Capacity is fixed at construction. A capacity of zero is valid and creates
/// an always-full queue, which can be useful when type-ahead is disabled.
#[derive(Clone, Debug)]
pub struct PromptQueue {
    capacity: usize,
    prompts: VecDeque<String>,
}

impl PromptQueue {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            prompts: VecDeque::with_capacity(capacity),
        }
    }

    /// Add one prompt without evicting existing work.
    ///
    /// If full, the queue is unchanged and the error owns the refused prompt.
    pub fn enqueue(
        &mut self,
        prompt: impl Into<String>,
    ) -> Result<PromptQueueStatus, PromptQueueFull> {
        let prompt = prompt.into();
        if self.prompts.len() >= self.capacity {
            return Err(PromptQueueFull {
                prompt,
                capacity: self.capacity,
            });
        }
        self.prompts.push_back(prompt);
        Ok(self.status())
    }

    /// Remove the oldest queued prompt.
    pub fn dequeue(&mut self) -> Option<String> {
        self.prompts.pop_front()
    }

    /// Inspect queued prompts in execution order without exposing mutation.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = &str> {
        self.prompts.iter().map(String::as_str)
    }

    #[cfg(test)]
    pub fn get(&self, index: usize) -> Option<&str> {
        self.prompts.get(index).map(String::as_str)
    }

    /// Remove one queued prompt by its zero-based execution position.
    pub fn remove(&mut self, index: usize) -> Option<String> {
        self.prompts.remove(index)
    }

    /// Replace one prompt in place, preserving queue length and order.
    pub fn edit(
        &mut self,
        index: usize,
        replacement: impl Into<String>,
    ) -> Result<String, PromptQueueEditError> {
        let replacement = replacement.into();
        let len = self.prompts.len();
        let Some(prompt) = self.prompts.get_mut(index) else {
            return Err(PromptQueueEditError {
                position: PromptQueueIndexError { index, len },
                replacement,
            });
        };
        Ok(std::mem::replace(prompt, replacement))
    }

    /// Move one queued prompt to another zero-based execution position.
    /// New submissions still append at the back; only this explicit operation
    /// can alter FIFO order.
    pub fn reorder(&mut self, from: usize, to: usize) -> Result<(), PromptQueueMoveError> {
        let len = self.prompts.len();
        if from >= len || to >= len {
            return Err(PromptQueueMoveError { from, to, len });
        }
        if from == to {
            return Ok(());
        }
        let prompt = self
            .prompts
            .remove(from)
            .expect("validated prompt queue source");
        self.prompts.insert(to, prompt);
        Ok(())
    }

    /// Promote a queued prompt to run next, retaining relative order of every
    /// other prompt.
    pub fn promote(&mut self, index: usize) -> Result<(), PromptQueueIndexError> {
        let len = self.prompts.len();
        if index >= len {
            return Err(PromptQueueIndexError { index, len });
        }
        self.reorder(index, 0)
            .expect("validated promote destination");
        Ok(())
    }

    /// Clear the queue and return every removed prompt in prior FIFO order.
    pub fn clear(&mut self) -> Vec<String> {
        self.prompts.drain(..).collect()
    }

    pub fn status(&self) -> PromptQueueStatus {
        PromptQueueStatus {
            queued: self.prompts.len(),
            capacity: self.capacity,
        }
    }
}

/// Identity of the one foreground turn eligible to receive corrections.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SteeringTurnId(u64);

impl SteeringTurnId {
    pub fn get(self) -> u64 {
        self.0
    }
}

/// A correction explicitly targeted at the current foreground turn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SteeringCorrection {
    text: String,
}

impl SteeringCorrection {
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn into_text(self) -> String {
        self.text
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SteeringStatus {
    pub turn: SteeringTurnId,
    pub queued: usize,
    pub capacity: usize,
}

impl SteeringStatus {
    #[cfg(test)]
    pub fn remaining(self) -> usize {
        self.capacity.saturating_sub(self.queued)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SteeringCapacityError;

impl fmt::Display for SteeringCapacityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("steering channel capacity must be greater than zero")
    }
}

impl Error for SteeringCapacityError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SteeringBeginError {
    pub active_turn: SteeringTurnId,
}

impl fmt::Display for SteeringBeginError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "foreground turn {} is already accepting steering",
            self.active_turn.get()
        )
    }
}

impl Error for SteeringBeginError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SteeringSubmitErrorKind {
    NoActiveTurn,
    StaleTurn,
    Full,
    ReceiverClosed,
}

/// Rejected correction, retaining its text so a UI can edit it, retry it, or
/// explicitly move it to the next-turn prompt queue.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SteeringSubmitError {
    pub kind: SteeringSubmitErrorKind,
    pub requested_turn: SteeringTurnId,
    pub active_turn: Option<SteeringTurnId>,
    pub capacity: usize,
    correction: SteeringCorrection,
}

impl SteeringSubmitError {
    pub fn correction(&self) -> &SteeringCorrection {
        &self.correction
    }

    #[cfg(test)]
    pub fn into_correction(self) -> SteeringCorrection {
        self.correction
    }
}

impl fmt::Display for SteeringSubmitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind {
            SteeringSubmitErrorKind::NoActiveTurn => {
                f.write_str("no foreground turn is accepting steering")
            }
            SteeringSubmitErrorKind::StaleTurn => write!(
                f,
                "steering target {} is stale; active turn is {}",
                self.requested_turn.get(),
                self.active_turn.map(SteeringTurnId::get).unwrap_or(0)
            ),
            SteeringSubmitErrorKind::Full => write!(
                f,
                "steering queue for turn {} is full (capacity {})",
                self.requested_turn.get(),
                self.capacity
            ),
            SteeringSubmitErrorKind::ReceiverClosed => write!(
                f,
                "foreground turn {} no longer accepts steering",
                self.requested_turn.get()
            ),
        }
    }
}

impl Error for SteeringSubmitError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SteeringTurnError {
    pub requested_turn: SteeringTurnId,
    pub active_turn: Option<SteeringTurnId>,
}

impl fmt::Display for SteeringTurnError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "steering turn {} is not active",
            self.requested_turn.get()
        )
    }
}

impl Error for SteeringTurnError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SteeringCloseStatus {
    pub turn: SteeringTurnId,
    /// Corrections still owned by the turn receiver. They never flow into a
    /// later turn; drop/drain the receiver or explicitly requeue their text.
    pub pending: usize,
}

struct SteeringActive {
    turn: SteeringTurnId,
    sender: mpsc::Sender<SteeringCorrection>,
}

struct SteeringState {
    active: Option<SteeringActive>,
}

struct SteeringInner {
    capacity: usize,
    next_turn: AtomicU64,
    state: Mutex<SteeringState>,
}

/// Bounded, typed correction channel for the current foreground turn.
///
/// This channel is deliberately separate from [`PromptQueue`]. Closing a turn
/// or dropping its receiver invalidates its token; undelivered corrections stay
/// with that receiver and are never promoted to future user prompts implicitly.
#[derive(Clone)]
pub struct SteeringChannel {
    inner: Arc<SteeringInner>,
}

impl SteeringChannel {
    pub fn new(capacity: usize) -> Result<Self, SteeringCapacityError> {
        if capacity == 0 {
            return Err(SteeringCapacityError);
        }
        Ok(Self {
            inner: Arc::new(SteeringInner {
                capacity,
                next_turn: AtomicU64::new(0),
                state: Mutex::new(SteeringState { active: None }),
            }),
        })
    }

    /// Open steering for exactly one new foreground turn.
    pub fn begin_turn(&self) -> Result<(SteeringTurnId, SteeringReceiver), SteeringBeginError> {
        let mut state = self.inner.state.lock().expect("steering channel");
        if let Some(active) = &state.active {
            return Err(SteeringBeginError {
                active_turn: active.turn,
            });
        }
        let turn = SteeringTurnId(self.inner.next_turn.fetch_add(1, Ordering::Relaxed) + 1);
        let (sender, receiver) = mpsc::channel(self.inner.capacity);
        state.active = Some(SteeringActive { turn, sender });
        Ok((
            turn,
            SteeringReceiver {
                turn,
                receiver,
                owner: Arc::downgrade(&self.inner),
            },
        ))
    }

    pub fn active_turn(&self) -> Option<SteeringTurnId> {
        self.inner
            .state
            .lock()
            .expect("steering channel")
            .active
            .as_ref()
            .map(|active| active.turn)
    }

    /// Submit a correction to a known active turn without blocking.
    pub fn steer(
        &self,
        turn: SteeringTurnId,
        correction: impl Into<String>,
    ) -> Result<SteeringStatus, SteeringSubmitError> {
        let correction = SteeringCorrection {
            text: correction.into(),
        };
        let mut state = self.inner.state.lock().expect("steering channel");
        let Some(active) = &state.active else {
            return Err(SteeringSubmitError {
                kind: SteeringSubmitErrorKind::NoActiveTurn,
                requested_turn: turn,
                active_turn: None,
                capacity: self.inner.capacity,
                correction,
            });
        };
        if active.turn != turn {
            return Err(SteeringSubmitError {
                kind: SteeringSubmitErrorKind::StaleTurn,
                requested_turn: turn,
                active_turn: Some(active.turn),
                capacity: self.inner.capacity,
                correction,
            });
        }
        let result = active.sender.try_send(correction);
        let remaining = active.sender.capacity();
        match result {
            Ok(()) => Ok(SteeringStatus {
                turn,
                queued: self.inner.capacity.saturating_sub(remaining),
                capacity: self.inner.capacity,
            }),
            Err(mpsc::error::TrySendError::Full(correction)) => Err(SteeringSubmitError {
                kind: SteeringSubmitErrorKind::Full,
                requested_turn: turn,
                active_turn: Some(turn),
                capacity: self.inner.capacity,
                correction,
            }),
            Err(mpsc::error::TrySendError::Closed(correction)) => {
                state.active = None;
                Err(SteeringSubmitError {
                    kind: SteeringSubmitErrorKind::ReceiverClosed,
                    requested_turn: turn,
                    active_turn: None,
                    capacity: self.inner.capacity,
                    correction,
                })
            }
        }
    }

    /// Stop accepting corrections for a turn. Pending corrections remain owned
    /// solely by its receiver and cannot leak into the next turn.
    pub fn close_turn(
        &self,
        turn: SteeringTurnId,
    ) -> Result<SteeringCloseStatus, SteeringTurnError> {
        let mut state = self.inner.state.lock().expect("steering channel");
        let Some(active) = state.active.as_ref() else {
            return Err(SteeringTurnError {
                requested_turn: turn,
                active_turn: None,
            });
        };
        if active.turn != turn {
            return Err(SteeringTurnError {
                requested_turn: turn,
                active_turn: Some(active.turn),
            });
        }
        let pending = self.inner.capacity.saturating_sub(active.sender.capacity());
        state.active = None;
        Ok(SteeringCloseStatus { turn, pending })
    }
}

/// Consumer owned by the current foreground turn/provider adapter.
pub struct SteeringReceiver {
    turn: SteeringTurnId,
    receiver: mpsc::Receiver<SteeringCorrection>,
    owner: Weak<SteeringInner>,
}

impl SteeringReceiver {
    pub fn turn(&self) -> SteeringTurnId {
        self.turn
    }

    pub async fn recv(&mut self) -> Option<SteeringCorrection> {
        self.receiver.recv().await
    }

    pub fn try_recv(&mut self) -> Result<SteeringCorrection, mpsc::error::TryRecvError> {
        self.receiver.try_recv()
    }

    /// Drain corrections that were not injected before the turn ended. The UI
    /// may show or explicitly requeue them, but no automatic conversion occurs.
    pub fn drain_pending(&mut self) -> Vec<SteeringCorrection> {
        let mut pending = Vec::with_capacity(self.receiver.len());
        while let Ok(correction) = self.try_recv() {
            pending.push(correction);
        }
        pending
    }
}

impl Drop for SteeringReceiver {
    fn drop(&mut self) {
        let Some(owner) = self.owner.upgrade() else {
            return;
        };
        let mut state = owner.state.lock().expect("steering channel");
        if state
            .active
            .as_ref()
            .is_some_and(|active| active.turn == self.turn)
        {
            state.active = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dequeues_in_submission_order_and_reports_status() {
        let mut queue = PromptQueue::new(3);
        assert_eq!(
            queue.status(),
            PromptQueueStatus {
                queued: 0,
                capacity: 3
            }
        );

        let status = queue.enqueue("first").unwrap();
        assert_eq!(status.queued, 1);
        assert_eq!(status.remaining(), 2);
        queue.enqueue("second").unwrap();
        queue.enqueue("third").unwrap();
        assert!(queue.status().is_full());

        assert_eq!(queue.dequeue().as_deref(), Some("first"));
        assert_eq!(queue.dequeue().as_deref(), Some("second"));
        assert_eq!(queue.dequeue().as_deref(), Some("third"));
        assert_eq!(queue.dequeue(), None);
        assert!(queue.status().is_empty());
    }

    #[test]
    fn overflow_refuses_new_prompt_without_evicting_or_reordering() {
        let mut queue = PromptQueue::new(2);
        queue.enqueue("one").unwrap();
        queue.enqueue("two").unwrap();

        let error = queue.enqueue("refused").unwrap_err();
        assert_eq!(error.prompt(), "refused");
        assert_eq!(error.capacity(), 2);
        assert_eq!(queue.status().queued, 2);
        assert_eq!(queue.dequeue().as_deref(), Some("one"));
        assert_eq!(queue.dequeue().as_deref(), Some("two"));
    }

    #[test]
    fn dequeue_reopens_exactly_one_slot() {
        let mut queue = PromptQueue::new(1);
        queue.enqueue("first").unwrap();
        assert_eq!(queue.dequeue().as_deref(), Some("first"));
        queue.enqueue("replacement").unwrap();
        assert_eq!(queue.dequeue().as_deref(), Some("replacement"));
    }

    #[test]
    fn zero_capacity_is_an_explicit_always_full_queue() {
        let mut queue = PromptQueue::new(0);
        let error = queue.enqueue(String::from("kept by error")).unwrap_err();
        assert_eq!(error.into_prompt(), "kept by error");
        assert_eq!(queue.dequeue(), None);
        assert_eq!(queue.status().remaining(), 0);
        assert!(queue.status().is_empty());
        assert!(queue.status().is_full());
    }

    #[test]
    fn queue_management_is_explicit_bounded_and_order_preserving() {
        let mut queue = PromptQueue::new(5);
        for prompt in ["one", "two", "three", "four"] {
            queue.enqueue(prompt).unwrap();
        }
        assert_eq!(
            queue.iter().collect::<Vec<_>>(),
            vec!["one", "two", "three", "four"]
        );
        assert_eq!(queue.get(2), Some("three"));
        assert_eq!(queue.edit(1, "TWO").unwrap(), "two");
        assert_eq!(queue.remove(2).as_deref(), Some("three"));
        queue.reorder(2, 1).unwrap();
        assert_eq!(queue.iter().collect::<Vec<_>>(), vec!["one", "four", "TWO"]);
        queue.promote(2).unwrap();
        assert_eq!(queue.iter().collect::<Vec<_>>(), vec!["TWO", "one", "four"]);
        assert_eq!(queue.status().queued, 3);
        assert_eq!(queue.status().capacity, 5);
        assert_eq!(queue.clear(), vec!["TWO", "one", "four"]);
        assert!(queue.status().is_empty());
    }

    #[test]
    fn invalid_queue_management_retains_prompts_and_replacements() {
        let mut queue = PromptQueue::new(2);
        queue.enqueue("one").unwrap();
        queue.enqueue("two").unwrap();

        let edit = queue.edit(9, "replacement").unwrap_err();
        assert_eq!(edit.position, PromptQueueIndexError { index: 9, len: 2 });
        assert_eq!(edit.into_replacement(), "replacement");
        assert_eq!(
            queue.reorder(0, 2).unwrap_err(),
            PromptQueueMoveError {
                from: 0,
                to: 2,
                len: 2
            }
        );
        assert_eq!(
            queue.promote(2).unwrap_err(),
            PromptQueueIndexError { index: 2, len: 2 }
        );
        assert_eq!(queue.iter().collect::<Vec<_>>(), vec!["one", "two"]);
        assert!(queue.status().is_full());
    }

    #[tokio::test]
    async fn steering_is_bounded_scoped_to_one_turn_and_never_leaks_forward() {
        let channel = SteeringChannel::new(2).unwrap();
        let (first_turn, mut first_receiver) = channel.begin_turn().unwrap();
        assert_eq!(first_receiver.turn(), first_turn);
        let active = match channel.begin_turn() {
            Err(error) => error.active_turn,
            Ok(_) => panic!("a second steering turn must be rejected"),
        };
        assert_eq!(active, first_turn);

        assert_eq!(channel.steer(first_turn, "first").unwrap().queued, 1);
        let status = channel.steer(first_turn, "second").unwrap();
        assert_eq!(status.queued, 2);
        assert_eq!(status.remaining(), 0);
        let full = channel.steer(first_turn, "retained").unwrap_err();
        assert_eq!(full.kind, SteeringSubmitErrorKind::Full);
        assert_eq!(full.into_correction().into_text(), "retained");

        assert_eq!(first_receiver.recv().await.unwrap().into_text(), "first");
        channel.steer(first_turn, "third").unwrap();
        let close = channel.close_turn(first_turn).unwrap();
        assert_eq!(close.pending, 2);
        assert_eq!(channel.active_turn(), None);

        let (second_turn, second_receiver) = channel.begin_turn().unwrap();
        let stale = channel.steer(first_turn, "never-forwarded").unwrap_err();
        assert_eq!(stale.kind, SteeringSubmitErrorKind::StaleTurn);
        assert_eq!(stale.active_turn, Some(second_turn));
        assert_eq!(stale.into_correction().into_text(), "never-forwarded");
        assert_eq!(
            first_receiver
                .drain_pending()
                .into_iter()
                .map(SteeringCorrection::into_text)
                .collect::<Vec<_>>(),
            vec!["second", "third"]
        );

        drop(second_receiver);
        assert_eq!(channel.active_turn(), None);
        let no_active = channel.steer(second_turn, "too late").unwrap_err();
        assert_eq!(no_active.kind, SteeringSubmitErrorKind::NoActiveTurn);
    }

    #[test]
    fn steering_rejects_zero_capacity() {
        assert!(SteeringChannel::new(0).is_err());
    }
}
