// Copyright (c) 2025, Joe Drago <joedrago@gmail.com>
// SPDX-License-Identifier: BSD-2-Clause

use std::io;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum StrawsError {
    #[error("IO error: {0}")]
    Io(#[from] io::Error),

    #[error("Protocol error: {0}")]
    Protocol(String),

    #[error("Remote error: {0}")]
    Remote(String),

    #[error("Agent stalled (no data for {0} seconds)")]
    Stall(u64),

    #[error("Agent connection failed: {0}")]
    Connection(String),

    #[error("All agents unhealthy")]
    AllAgentsUnhealthy,

    #[error("MD5 verification failed for {path}: expected {expected}, got {actual}")]
    Md5Mismatch {
        path: String,
        expected: String,
        actual: String,
    },

    #[error("Max retries exceeded for {0}")]
    MaxRetries(String),

    #[error("Invalid path: {0}")]
    InvalidPath(String),

    #[error("SSH error: {0}")]
    Ssh(String),

    #[error("Configuration error: {0}")]
    Config(String),

    #[error("Operation aborted")]
    Aborted,

    #[error("File not found: {0}")]
    NotFound(String),

    #[error("Permission denied: {0}")]
    PermissionDenied(String),
}

impl StrawsError {
    /// Returns true if this error type is retryable
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            StrawsError::Stall(_)
                | StrawsError::Protocol(_)
                | StrawsError::Remote(_)
                | StrawsError::Connection(_)
                | StrawsError::Md5Mismatch { .. }
        )
    }

    /// Returns true if this indicates an agent failure (should try different agent)
    pub fn is_agent_failure(&self) -> bool {
        matches!(
            self,
            StrawsError::Stall(_) | StrawsError::Protocol(_) | StrawsError::Connection(_)
        )
    }

    /// Returns true if this error is fatal and transfer should abort
    pub fn is_fatal(&self) -> bool {
        matches!(
            self,
            StrawsError::AllAgentsUnhealthy
                | StrawsError::Config(_)
                | StrawsError::Aborted
                | StrawsError::MaxRetries(_)
        )
    }
}

pub type Result<T> = std::result::Result<T, StrawsError>;
