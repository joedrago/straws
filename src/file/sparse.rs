// Copyright (c) 2025, Joe Drago <joedrago@gmail.com>
// SPDX-License-Identifier: BSD-2-Clause

use std::path::Path;

use crate::error::{Result, StrawsError};

/// Preallocate a sparse file
pub fn preallocate_sync(path: &Path, size: u64) -> Result<()> {
    use std::fs::OpenOptions;

    // Ensure parent directory exists
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| StrawsError::Io(e))?;
    }

    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .open(path)
        .map_err(|e| StrawsError::Io(e))?;

    file.set_len(size).map_err(|e| StrawsError::Io(e))?;

    Ok(())
}
