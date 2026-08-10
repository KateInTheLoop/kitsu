//! A small bounded FIFO used by interactive frontends to own type-ahead.
//!
//! This module deliberately knows nothing about stdin, terminal rendering, or
//! agent turns. The frontend decides when a submitted line should run and uses
//! this queue only for explicit, visible buffering while another turn is busy.

use std::collections::VecDeque;
use std::error::Error;
use std::fmt;

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

    pub fn status(&self) -> PromptQueueStatus {
        PromptQueueStatus {
            queued: self.prompts.len(),
            capacity: self.capacity,
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
}
