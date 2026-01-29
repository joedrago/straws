// Copyright (c) 2025, Joe Drago <joedrago@gmail.com>
// SPDX-License-Identifier: BSD-2-Clause

use std::path::Path;

use crate::error::{Result, StrawsError};

/// Validate that an SSH key file exists and is readable
pub fn validate_key_file(path: &Path) -> Result<()> {
    if !path.exists() {
        return Err(StrawsError::Config(format!(
            "SSH key file not found: {}",
            path.display()
        )));
    }

    if !path.is_file() {
        return Err(StrawsError::Config(format!(
            "SSH key path is not a file: {}",
            path.display()
        )));
    }

    // Check permissions (should be 600 or 400)
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        let metadata = std::fs::metadata(path).map_err(|e| {
            StrawsError::Config(format!(
                "Cannot read SSH key file {}: {}",
                path.display(),
                e
            ))
        })?;

        let mode = metadata.mode() & 0o777;
        if mode & 0o077 != 0 {
            return Err(StrawsError::Config(format!(
                "SSH key file {} has insecure permissions {:o}. Should be 600 or 400.",
                path.display(),
                mode
            )));
        }
    }

    Ok(())
}
