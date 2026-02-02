// Copyright (c) 2025, Joe Drago <joedrago@gmail.com>
// SPDX-License-Identifier: BSD-2-Clause

use std::fmt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use parking_lot::Mutex;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Download,
    Upload,
}

/// Metadata for a file being transferred
pub struct FileMeta {
    pub remote_path: String,
    pub local_path: PathBuf,
    pub size: u64,
    pub mode: u32,
    pub mtime: u64,
    /// Number of chunks for this file (1 for small files)
    pub total_chunks: u32,
    /// Bitmap of completed chunks
    pub completed_chunks: Mutex<Vec<bool>>,
    /// Bytes transferred so far
    pub bytes_transferred: AtomicU64,
    /// Preallocation result - ensures only one chunk preallocates (downloads)
    /// Ok(()) on success, Err(message) on failure
    prealloc_result: OnceLock<Result<(), String>>,
    /// Truncation result - ensures only one chunk truncates remote file (uploads)
    /// Ok(()) on success, Err(message) on failure
    truncate_result: OnceLock<Result<(), String>>,
    /// Finalization result - ensures only one chunk finalizes the file
    /// Ok(()) on success, Err(message) on failure
    finalize_result: OnceLock<Result<(), String>>,
}

impl fmt::Debug for FileMeta {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FileMeta")
            .field("remote_path", &self.remote_path)
            .field("local_path", &self.local_path)
            .field("size", &self.size)
            .field("total_chunks", &self.total_chunks)
            .finish_non_exhaustive()
    }
}

impl FileMeta {
    pub fn new(
        remote_path: String,
        local_path: PathBuf,
        size: u64,
        mode: u32,
        mtime: u64,
        total_chunks: u32,
    ) -> Self {
        FileMeta {
            remote_path,
            local_path,
            size,
            mode,
            mtime,
            total_chunks,
            completed_chunks: Mutex::new(vec![false; total_chunks as usize]),
            bytes_transferred: AtomicU64::new(0),
            prealloc_result: OnceLock::new(),
            truncate_result: OnceLock::new(),
            finalize_result: OnceLock::new(),
        }
    }

    /// Ensure the temp file is preallocated. Only the first caller does the work;
    /// subsequent callers get the cached result.
    pub fn ensure_preallocated(&self) -> Result<(), String> {
        self.prealloc_result
            .get_or_init(|| {
                let temp_path = self.temp_path();

                // Ensure parent directory exists
                if let Some(parent) = temp_path.parent() {
                    if let Err(e) = std::fs::create_dir_all(parent) {
                        return Err(format!("Failed to create directory: {}", e));
                    }
                }

                // Create and preallocate the file
                let file = match std::fs::OpenOptions::new()
                    .create(true)
                    .write(true)
                    .open(&temp_path)
                {
                    Ok(f) => f,
                    Err(e) => return Err(format!("Failed to create temp file: {}", e)),
                };

                if self.size > 0 {
                    if let Err(e) = file.set_len(self.size) {
                        return Err(format!("Failed to preallocate: {}", e));
                    }
                }

                Ok(())
            })
            .clone()
    }

    /// Store truncation result. Only the first caller's result is stored;
    /// subsequent callers get the cached result.
    pub fn store_truncate_result(&self, result: Result<(), String>) -> Result<(), String> {
        self.truncate_result.get_or_init(|| result).clone()
    }

    /// Check if truncation has been attempted (regardless of result)
    pub fn truncate_attempted(&self) -> bool {
        self.truncate_result.get().is_some()
    }

    /// Execute finalization. Only the first caller does the work;
    /// subsequent callers get the cached result.
    /// The closure is only called if finalization hasn't been attempted yet.
    pub fn ensure_finalized<F>(&self, f: F) -> Result<(), String>
    where
        F: FnOnce() -> Result<(), String>,
    {
        self.finalize_result.get_or_init(f).clone()
    }

    /// Check if finalization has been attempted (regardless of result)
    pub fn finalize_attempted(&self) -> bool {
        self.finalize_result.get().is_some()
    }

    pub fn mark_chunk_complete(&self, chunk_index: u32) {
        let mut completed = self.completed_chunks.lock();
        if (chunk_index as usize) < completed.len() {
            completed[chunk_index as usize] = true;
        }
    }

    pub fn is_complete(&self) -> bool {
        let completed = self.completed_chunks.lock();
        completed.iter().all(|&c| c)
    }

    pub fn add_bytes(&self, bytes: u64) {
        self.bytes_transferred.fetch_add(bytes, Ordering::Relaxed);
    }

    pub fn temp_path(&self) -> PathBuf {
        let mut path = self.local_path.clone();
        let file_name = path
            .file_name()
            .map(|n| format!("{}.straws.tmp", n.to_string_lossy()))
            .unwrap_or_else(|| ".straws.tmp".to_string());
        path.set_file_name(file_name);
        path
    }
}

/// A single job (file or chunk)
#[derive(Debug)]
pub struct Job {
    pub id: u64,
    pub file_meta: Arc<FileMeta>,
    pub direction: Direction,
    pub offset: u64,
    pub length: u64,
    pub chunk_index: Option<u32>,
    pub verify_md5: bool,
    pub retry_count: AtomicU32,
}

impl Job {
    pub fn new(
        id: u64,
        file_meta: Arc<FileMeta>,
        direction: Direction,
        offset: u64,
        length: u64,
        chunk_index: Option<u32>,
        verify_md5: bool,
    ) -> Self {
        Job {
            id,
            file_meta,
            direction,
            offset,
            length,
            chunk_index,
            verify_md5,
            retry_count: AtomicU32::new(0),
        }
    }

    /// Create a whole-file job
    pub fn whole_file(
        id: u64,
        file_meta: Arc<FileMeta>,
        direction: Direction,
        verify_md5: bool,
    ) -> Self {
        Job::new(
            id,
            Arc::clone(&file_meta),
            direction,
            0,
            file_meta.size,
            None,
            verify_md5,
        )
    }

    /// Create a chunk job
    pub fn chunk(
        id: u64,
        file_meta: Arc<FileMeta>,
        direction: Direction,
        chunk_index: u32,
        offset: u64,
        length: u64,
        verify_md5: bool,
    ) -> Self {
        Job::new(
            id,
            file_meta,
            direction,
            offset,
            length,
            Some(chunk_index),
            verify_md5,
        )
    }

    pub fn increment_retry(&self) -> u32 {
        self.retry_count.fetch_add(1, Ordering::Relaxed) + 1
    }

    pub fn retries(&self) -> u32 {
        self.retry_count.load(Ordering::Relaxed)
    }

    pub fn description(&self) -> String {
        let file_name = self
            .file_meta
            .local_path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "unknown".to_string());

        match self.chunk_index {
            Some(idx) => {
                let total = self.file_meta.total_chunks;
                let width = total.to_string().len();
                format!(
                    "[{:0>width$}/{}] {}",
                    idx + 1,
                    total,
                    file_name,
                    width = width
                )
            }
            None => file_name,
        }
    }
}

/// Result of a completed job
#[derive(Debug)]
pub struct JobResult {
    pub job_id: u64,
    pub success: bool,
    pub bytes_transferred: u64,
    pub md5: Option<String>,
    pub error: Option<String>,
}

impl JobResult {
    pub fn success(job_id: u64, bytes_transferred: u64, md5: Option<String>) -> Self {
        JobResult {
            job_id,
            success: true,
            bytes_transferred,
            md5,
            error: None,
        }
    }

    pub fn failure(job_id: u64, error: String) -> Self {
        JobResult {
            job_id,
            success: false,
            bytes_transferred: 0,
            md5: None,
            error: Some(error),
        }
    }
}
