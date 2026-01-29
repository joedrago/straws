// Copyright (c) 2026, Joe Drago
// SPDX-License-Identifier: BSD-2-Clause

use std::path::Path;

use tokio::fs::{File, OpenOptions};
use tokio::io::{AsyncSeekExt, AsyncWriteExt};

use crate::error::{Result, StrawsError};

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

/// Preallocate file space (create sparse file)
pub async fn preallocate(path: &Path, size: u64) -> Result<()> {
    // Ensure parent directory exists
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| StrawsError::Io(e))?;
    }

    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .open(path)
        .await
        .map_err(|e| StrawsError::Io(e))?;

    file.set_len(size).await.map_err(|e| StrawsError::Io(e))?;

    Ok(())
}

/// Sync file to disk
pub async fn sync_file(file: &mut File) -> Result<()> {
    file.flush().await.map_err(|e| StrawsError::Io(e))?;
    file.sync_all().await.map_err(|e| StrawsError::Io(e))?;
    Ok(())
}
