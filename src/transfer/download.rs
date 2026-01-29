// Copyright (c) 2026, Joe Drago
// SPDX-License-Identifier: BSD-2-Clause

use std::sync::Arc;

use md5::{Digest, Md5};
use tokio::fs::File;
use tokio::io::{AsyncWriteExt, BufWriter};

use super::io::open_write_at;
use crate::agent::Agent;
use crate::debug_log;
use crate::error::{Result, StrawsError};
use crate::job::types::{Job, JobResult};

const IO_BUFFER_SIZE: usize = 65536; // 64KB

/// Execute a download job
pub async fn download_job(job: &Arc<Job>, agent: &Arc<Agent>) -> Result<JobResult> {
    let temp_path = job.file_meta.temp_path();

    // Ensure file is preallocated (only first chunk does the work, others wait/get cached result)
    job.file_meta
        .ensure_preallocated()
        .map_err(|e| StrawsError::Io(std::io::Error::new(std::io::ErrorKind::Other, e)))?;

    // Open temp file at correct offset
    let file = open_write_at(&temp_path, job.offset).await?;
    let mut writer = BufWriter::with_capacity(IO_BUFFER_SIZE, file);

    // Download data from remote
    let bytes_written = agent
        .stream_read(
            &job.file_meta.remote_path,
            job.offset,
            job.length,
            &mut writer,
        )
        .await?;

    writer.flush().await.map_err(|e| StrawsError::Io(e))?;

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

async fn compute_local_md5(path: &std::path::Path, offset: u64, length: u64) -> Result<String> {
    use tokio::io::AsyncReadExt;

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

/// Compute MD5 of an entire file
pub async fn compute_file_md5(path: &std::path::Path) -> Result<String> {
    let file_size = tokio::fs::metadata(path)
        .await
        .map_err(|e| StrawsError::Io(e))?
        .len();

    compute_local_md5(path, 0, file_size).await
}
