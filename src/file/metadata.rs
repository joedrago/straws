// Copyright (c) 2025, Joe Drago <joedrago@gmail.com>
// SPDX-License-Identifier: BSD-2-Clause

use std::path::Path;
use std::time::{Duration, UNIX_EPOCH};

use crate::error::{Result, StrawsError};

/// Set file mode (permissions)
#[cfg(unix)]
pub fn set_mode(path: &Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let permissions = std::fs::Permissions::from_mode(mode);
    std::fs::set_permissions(path, permissions).map_err(|e| StrawsError::Io(e))?;

    Ok(())
}

/// Set file mode (permissions) - no-op on Windows (Unix permissions don't apply)
#[cfg(not(unix))]
pub fn set_mode(_path: &Path, _mode: u32) -> Result<()> {
    Ok(())
}

/// Set file modification time
pub fn set_mtime(path: &Path, mtime: u64) -> Result<()> {
    use std::fs::File;

    let time = UNIX_EPOCH + Duration::from_secs(mtime);

    // Use filetime crate functionality via std
    let file = File::open(path).map_err(|e| StrawsError::Io(e))?;
    file.set_modified(time).map_err(|e| StrawsError::Io(e))?;

    Ok(())
}
