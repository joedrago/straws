// Copyright (c) 2026, Joe Drago
// SPDX-License-Identifier: BSD-2-Clause

use std::path::Path;

use md5::{Digest, Md5};
use tokio::fs::{File, OpenOptions};
use tokio::io::{AsyncReadExt, AsyncSeekExt};

use crate::error::{Result, StrawsError};

const IO_BUFFER_SIZE: usize = 524288; // 512KB

/// Open a file for writing at a specific offset
pub async fn open_write_at(path: &Path, offset: u64) -> Result<File> {
    // Ensure parent directory exists
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| StrawsError::Io(e))?;
    }

    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .open(path)
        .await
        .map_err(|e| StrawsError::Io(e))?;

    if offset > 0 {
        file.seek(std::io::SeekFrom::Start(offset))
            .await
            .map_err(|e| StrawsError::Io(e))?;
    }

    Ok(file)
}

/// Compute MD5 of a byte range within a file
pub async fn compute_local_md5(path: &Path, offset: u64, length: u64) -> Result<String> {
    let mut file = File::open(path).await.map_err(|e| StrawsError::Io(e))?;

    if offset > 0 {
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
pub async fn compute_file_md5(path: &Path) -> Result<String> {
    let file_size = tokio::fs::metadata(path)
        .await
        .map_err(|e| StrawsError::Io(e))?
        .len();

    compute_local_md5(path, 0, file_size).await
}
