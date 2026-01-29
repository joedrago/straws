// Copyright (c) 2026, Joe Drago
// SPDX-License-Identifier: BSD-2-Clause

use std::sync::Arc;

use md5::{Digest, Md5};
use tokio::fs::File;
use tokio::io::AsyncReadExt;

use crate::agent::protocol::Request;
use crate::agent::Agent;
use crate::debug_log;
use crate::error::{Result, StrawsError};
use crate::job::types::{Job, JobResult};
use crate::progress::tracker::ProgressTracker;

const IO_BUFFER_SIZE: usize = 524288; // 512KB

/// Execute an upload job
pub async fn upload_job(
    job: &Arc<Job>,
    agent: &Arc<Agent>,
    tracker: &Arc<ProgressTracker>,
) -> Result<JobResult> {
    let local_path = &job.file_meta.local_path;

    // Ensure remote file is truncated/created (only first chunk to reach here does the work)
    // This avoids race conditions where chunk N runs before chunk 0
    if !job.file_meta.truncate_attempted() {
        let result = async {
            let response = agent
                .request(&Request::truncate(&job.file_meta.remote_path, job.file_meta.size))
                .await?;
            if !response.is_success() {
                return Err(StrawsError::Remote(
                    response
                        .error_message()
                        .unwrap_or_else(|| "Truncate failed".to_string()),
                ));
            }
            Ok(())
        }
        .await;

        job.file_meta
            .store_truncate_result(result.map_err(|e| e.to_string()))
            .map_err(|e| StrawsError::Remote(e))?;
    } else {
        // Another chunk already attempted truncation, get the cached result
        job.file_meta
            .store_truncate_result(Ok(()))
            .map_err(|e| StrawsError::Remote(e))?;
    }

    // Read local file and upload in chunks
    let mut file = File::open(local_path)
        .await
        .map_err(|e| StrawsError::Io(e))?;

    if job.offset > 0 {
        use tokio::io::AsyncSeekExt;
        file.seek(std::io::SeekFrom::Start(job.offset))
            .await
            .map_err(|e| StrawsError::Io(e))?;
    }

    let mut remaining = job.length;
    let mut current_offset = job.offset;
    let mut total_written = 0u64;
    let mut buf = vec![0u8; IO_BUFFER_SIZE];

    while remaining > 0 {
        let to_read = std::cmp::min(remaining as usize, buf.len());
        let n = file
            .read(&mut buf[..to_read])
            .await
            .map_err(|e| StrawsError::Io(e))?;

        if n == 0 {
            break;
        }

        // Send write request
        let write_req = Request::write(
            &job.file_meta.remote_path,
            current_offset,
            buf[..n].to_vec(),
        );
        let response = agent.request(&write_req).await?;

        if !response.is_success() {
            return Err(StrawsError::Remote(
                response
                    .error_message()
                    .unwrap_or_else(|| "Write failed".to_string()),
            ));
        }

        remaining -= n as u64;
        current_offset += n as u64;
        total_written += n as u64;
        job.file_meta.add_bytes(n as u64);
        tracker.add_bytes(n as u64);
    }

    // Mark chunk complete
    // For chunked files, mark the specific chunk; for non-chunked, mark chunk 0
    let chunk_idx = job.chunk_index.unwrap_or(0);
    job.file_meta.mark_chunk_complete(chunk_idx);

    // MD5 verification if requested
    let md5_hash = if job.verify_md5 {
        // Compute local MD5
        let local_md5 = compute_local_md5(local_path, job.offset, job.length).await?;

        // Request remote MD5
        let remote_response = agent
            .request(&Request::md5(
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
                expected: local_md5,
                actual: remote_md5,
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
        "Uploaded {} bytes for {} (offset {})",
        total_written,
        job.description(),
        job.offset
    );

    Ok(JobResult::success(job.id, total_written, md5_hash))
}

async fn compute_local_md5(path: &std::path::Path, offset: u64, length: u64) -> Result<String> {
    let mut file = File::open(path).await.map_err(|e| StrawsError::Io(e))?;

    if offset > 0 {
        use tokio::io::AsyncSeekExt;
        file.seek(std::io::SeekFrom::Start(offset))
            .await
            .map_err(|e| StrawsError::Io(e))?;
    }

    let mut hasher = Md5::new();
    let mut remaining = length;
    let mut buf = vec![0u8; IO_BUFFER_SIZE];

    while remaining > 0 {
        let to_read = std::cmp::min(remaining as usize, buf.len());
        let n = file
            .read(&mut buf[..to_read])
            .await
            .map_err(|e| StrawsError::Io(e))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        remaining -= n as u64;
    }

    Ok(format!("{:x}", hasher.finalize()))
}
