// Copyright (c) 2026, Joe Drago
// SPDX-License-Identifier: BSD-2-Clause

use std::sync::Arc;

use tokio::io::{AsyncWriteExt, BufWriter};

use super::io::{compute_local_md5, open_write_at};
pub use super::io::compute_file_md5;
use crate::agent::Agent;
use crate::debug_log;
use crate::error::{Result, StrawsError};
use crate::job::types::{Job, JobResult};
use crate::progress::tracker::ProgressTracker;

const IO_BUFFER_SIZE: usize = 524288; // 512KB

/// Execute a download job
pub async fn download_job(
    job: &Arc<Job>,
    agent: &Arc<Agent>,
    tracker: &Arc<ProgressTracker>,
) -> Result<JobResult> {
    let temp_path = job.file_meta.temp_path();

    // Ensure file is preallocated (only first chunk does the work, others wait/get cached result)
    job.file_meta
        .ensure_preallocated()
        .map_err(|e| StrawsError::Io(std::io::Error::new(std::io::ErrorKind::Other, e)))?;

    // Open temp file at correct offset
    let file = open_write_at(&temp_path, job.offset).await?;
    let mut writer = BufWriter::with_capacity(IO_BUFFER_SIZE, file);

    // Download data from remote, reporting progress incrementally
    let tracker_clone = Arc::clone(tracker);
    let bytes_written = agent
        .stream_read(
            &job.file_meta.remote_path,
            job.offset,
            job.length,
            &mut writer,
            Some(move |bytes: u64| {
                tracker_clone.add_bytes(bytes);
            }),
        )
        .await?;

    writer.flush().await.map_err(|e| StrawsError::Io(e))?;

    // Sync to disk before marking chunk complete to ensure durability on crash
    writer
        .get_mut()
        .sync_all()
        .await
        .map_err(|e| StrawsError::Io(e))?;

    // Update file meta
    job.file_meta.add_bytes(bytes_written);

    // Mark chunk complete
    // For chunked files, mark the specific chunk; for non-chunked, mark chunk 0
    let chunk_idx = job.chunk_index.unwrap_or(0);
    job.file_meta.mark_chunk_complete(chunk_idx);

    // MD5 verification if requested (for non-chunked or final chunk)
    let md5_hash = if job.verify_md5 {
        // Compute local MD5
        let local_md5 = compute_local_md5(&temp_path, job.offset, job.length).await?;

        // Request remote MD5
        let remote_response = agent
            .request(&crate::agent::Request::md5(
                &job.file_meta.remote_path,
                job.offset,
                job.length,
            ))
            .await?;

        if !remote_response.is_success() {
            return Err(StrawsError::Remote(
                remote_response
                    .error_message()
                    .unwrap_or_else(|| "MD5 failed".to_string()),
            ));
        }

        let remote_md5 = String::from_utf8_lossy(&remote_response.data).to_string();

        if local_md5 != remote_md5 {
            return Err(StrawsError::Md5Mismatch {
                path: job.file_meta.remote_path.clone(),
                expected: remote_md5,
                actual: local_md5,
            });
        }

        debug_log!(
            "Verified {} ({} = {}) mode={:04o} mtime={}",
            job.file_meta.remote_path,
            local_md5,
            remote_md5,
            job.file_meta.mode & 0o7777,
            job.file_meta.mtime
        );

        Some(local_md5)
    } else {
        None
    };

    debug_log!(
        "Downloaded {} bytes for {} (offset {})",
        bytes_written,
        job.description(),
        job.offset
    );

    Ok(JobResult::success(job.id, bytes_written, md5_hash))
}
