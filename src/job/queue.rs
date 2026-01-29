// Copyright (c) 2025, Joe Drago <joedrago@gmail.com>
// SPDX-License-Identifier: BSD-2-Clause

use std::sync::Arc;

use crossbeam_channel::{bounded, Receiver, Sender, TryRecvError};

use super::types::Job;

const DEFAULT_CAPACITY: usize = 10_000;

/// Bounded job queue using crossbeam channel
pub struct JobQueue {
    sender: Sender<Arc<Job>>,
    receiver: Receiver<Arc<Job>>,
}

impl JobQueue {
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_CAPACITY)
    }

    pub fn with_capacity(capacity: usize) -> Self {
        let (sender, receiver) = bounded(capacity);
        JobQueue { sender, receiver }
    }

    /// Push a job to the queue (blocks if full)
    pub fn push(&self, job: Arc<Job>) -> bool {
        self.sender.send(job).is_ok()
    }

    /// Try to push without blocking
    pub fn try_push(&self, job: Arc<Job>) -> bool {
        self.sender.try_send(job).is_ok()
    }

    /// Pop a job from the queue (blocks if empty)
    pub fn pop(&self) -> Option<Arc<Job>> {
        self.receiver.recv().ok()
    }

    /// Try to pop without blocking
    pub fn try_pop(&self) -> Option<Arc<Job>> {
        match self.receiver.try_recv() {
            Ok(job) => Some(job),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => None,
        }
    }

    /// Check if queue is empty
    pub fn is_empty(&self) -> bool {
        self.receiver.is_empty()
    }

    /// Get current queue length
    pub fn len(&self) -> usize {
        self.receiver.len()
    }

    /// Clone the sender for use by producers
    pub fn sender(&self) -> Sender<Arc<Job>> {
        self.sender.clone()
    }

    /// Clone the receiver for use by consumers
    pub fn receiver(&self) -> Receiver<Arc<Job>> {
        self.receiver.clone()
    }
}

impl Default for JobQueue {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for JobQueue {
    fn clone(&self) -> Self {
        JobQueue {
            sender: self.sender.clone(),
            receiver: self.receiver.clone(),
        }
    }
}
