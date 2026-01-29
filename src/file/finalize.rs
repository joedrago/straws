// Copyright (c) 2025, Joe Drago <joedrago@gmail.com>
// SPDX-License-Identifier: BSD-2-Clause

use std::sync::Arc;

use crate::debug_log;
use crate::error::{Result, StrawsError};
use crate::job::types::FileMeta;

use super::metadata::{set_mode, set_mtime};

/// Finalize a completed file: rename temp to final, set mode and mtime
/// Uses OnceLock pattern to ensure only one chunk finalizes even if multiple
/// chunks complete nearly simultaneously.
pub fn finalize_file(file_meta: &Arc<FileMeta>) -> Result<()> {
    // Use OnceLock to ensure only one caller does the actual finalization
    let result = file_meta.store_finalize_result(do_finalize(file_meta));
    result.map_err(|e| StrawsError::Io(std::io::Error::new(std::io::ErrorKind::Other, e)))
}

/// Internal finalization logic
fn do_finalize(file_meta: &FileMeta) -> std::result::Result<(), String> {
    let temp_path = file_meta.temp_path();
    let final_path = &file_meta.local_path;

    // Rename temp to final
    std::fs::rename(&temp_path, final_path).map_err(|e| {
        format!(
            "Failed to rename {} to {}: {}",
            temp_path.display(),
            final_path.display(),
            e
        )
    })?;

    // Set mode (only user bits, preserve umask behavior)
    if file_meta.mode != 0 {
        let mode = file_meta.mode & 0o777;
        if let Err(e) = set_mode(final_path, mode) {
            debug_log!("Warning: Failed to set mode for {}: {}", final_path.display(), e);
        }
    }

    // Set mtime
    if file_meta.mtime != 0 {
        if let Err(e) = set_mtime(final_path, file_meta.mtime) {
            debug_log!("Warning: Failed to set mtime for {}: {}", final_path.display(), e);
        }
    }

    debug_log!("Finalized: {}", final_path.display());

    Ok(())
}

/// Delete incomplete temp file
pub fn cleanup_temp(file_meta: &Arc<FileMeta>) {
    let temp_path = file_meta.temp_path();
    if temp_path.exists() {
        if let Err(e) = std::fs::remove_file(&temp_path) {
            debug_log!("Warning: Failed to remove temp file {}: {}", temp_path.display(), e);
        }
    }
}
