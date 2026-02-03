// Copyright (c) 2026, Joe Drago
// SPDX-License-Identifier: BSD-2-Clause

use std::collections::VecDeque;
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

/// Maximum number of write requests to have in flight at once.
/// This enables pipelining to hide network latency.
/// 16 * 512KB = 8MB in flight, which fits comfortably in SSH buffers.
const PIPELINE_DEPTH: usize = 16;

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

    // Track bytes sent for each pending request (for progress reporting)
    let mut pending: VecDeque<u64> = VecDeque::with_capacity(PIPELINE_DEPTH);
    let mut done_reading = false;

    // Pipelined upload loop:
    // 1. Send requests until we hit PIPELINE_DEPTH or run out of data
    // 2. Flush once to send the batch
    // 3. Read responses until pipeline is half empty (or fully drained if done)
    // 4. Repeat until all data sent and all responses received
    while !done_reading || !pending.is_empty() {
        // Fill the pipeline with write requests
        while !done_reading && pending.len() < PIPELINE_DEPTH {
            let to_read = std::cmp::min(remaining as usize, buf.len());
            if to_read == 0 {
                done_reading = true;
                break;
            }

            let n = file
                .read(&mut buf[..to_read])
                .await
                .map_err(|e| StrawsError::Io(e))?;

            if n == 0 {
                done_reading = true;
                break;
            }

            // Send write request without waiting for response
            let write_req = Request::write(
                &job.file_meta.remote_path,
                current_offset,
                buf[..n].to_vec(),
            );
            agent.send_request(&write_req, n as u64).await?;

            pending.push_back(n as u64);
            remaining -= n as u64;
            current_offset += n as u64;
        }

        // Flush all batched requests before reading responses
        if !pending.is_empty() {
            agent.flush_requests().await?;
        }

        // Read responses - drain to half capacity to make room for more writes,
        // or drain completely if we're done reading the file
        let drain_to = if done_reading { 0 } else { PIPELINE_DEPTH / 2 };
        while pending.len() > drain_to {
            let bytes_sent = pending.pop_front().unwrap();
            let response = agent.read_response().await?;

            if !response.is_success() {
                // On error, mark agent unhealthy since we have orphaned requests in flight
                // The agent's state will be inconsistent (pending responses we won't read)
                agent.mark_unhealthy("Write failed mid-pipeline");
                return Err(StrawsError::Remote(
                    response
                        .error_message()
                        .unwrap_or_else(|| "Write failed".to_string()),
                ));
            }

            total_written += bytes_sent;
            job.file_meta.add_bytes(bytes_sent);
            tracker.add_bytes(bytes_sent);
        }
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
