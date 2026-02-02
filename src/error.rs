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

    #[error("Transfer stalled (no data received for {0} seconds, network or remote issue)")]
    Stall(u64),

    #[error("Agent connection failed: {0}")]
    Connection(String),

    #[error("All SSH connections failed or became unavailable")]
    AllAgentsUnhealthy,

    #[error("MD5 verification failed for {path}: expected {expected}, got {actual}")]
    Md5Mismatch {
        path: String,
        expected: String,
        actual: String,
    },

    #[error("{0}")]
    TransferFailed(String),

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
                | StrawsError::TransferFailed(_)
        )
    }
}

pub type Result<T> = std::result::Result<T, StrawsError>;
