// Copyright (c) 2025, Joe Drago <joedrago@gmail.com>
// SPDX-License-Identifier: BSD-2-Clause

use std::io::{self, BufRead, Write};
use std::path::Path;

use crate::error::{Result, StrawsError};

/// Get password from various sources
pub fn get_password(
    prompt: bool,
    file: Option<&Path>,
    env_var: &str,
) -> Result<Option<String>> {
    // Try password file first
    if let Some(path) = file {
        return read_password_file(path).map(Some);
    }

    // Try environment variable
    if let Ok(password) = std::env::var(env_var) {
        if !password.is_empty() {
            return Ok(Some(password));
        }
    }

    // Interactive prompt
    if prompt {
        return prompt_password().map(Some);
    }

    Ok(None)
}

fn read_password_file(path: &Path) -> Result<String> {
    let content = std::fs::read_to_string(path).map_err(|e| {
        StrawsError::Config(format!(
            "Failed to read password file {}: {}",
            path.display(),
            e
        ))
    })?;

    // Take first line, trim whitespace
    let password = content.lines().next().unwrap_or("").trim().to_string();

    if password.is_empty() {
        return Err(StrawsError::Config(format!(
            "Password file {} is empty",
            path.display()
        )));
    }

    Ok(password)
}

fn prompt_password() -> Result<String> {
    // Write prompt to stderr (so it doesn't mix with output)
    eprint!("Password: ");
    io::stderr().flush().map_err(|e| StrawsError::Io(e))?;

    // Read password with terminal raw mode for hidden input
    let password = read_password_raw()?;

    // Print newline after password entry
    eprintln!();

    if password.is_empty() {
        return Err(StrawsError::Config("Empty password".to_string()));
    }

    Ok(password)
}

#[cfg(unix)]
fn read_password_raw() -> Result<String> {
    use std::os::unix::io::AsRawFd;

    let stdin = io::stdin();
    let fd = stdin.as_raw_fd();

    // Get current terminal settings
    let mut termios = unsafe {
        let mut t = std::mem::zeroed();
        if libc::tcgetattr(fd, &mut t) != 0 {
            return Err(StrawsError::Io(io::Error::last_os_error()));
        }
        t
    };

    // Save original settings
    let original = termios;

    // Disable echo
    termios.c_lflag &= !libc::ECHO;

    // Apply new settings
    unsafe {
        if libc::tcsetattr(fd, libc::TCSANOW, &termios) != 0 {
            return Err(StrawsError::Io(io::Error::last_os_error()));
        }
    }

    // Read password
    let mut password = String::new();
    let result = io::stdin().lock().read_line(&mut password);

    // Restore original settings
    unsafe {
        libc::tcsetattr(fd, libc::TCSANOW, &original);
    }

    result.map_err(|e| StrawsError::Io(e))?;

    // Trim newline
    Ok(password.trim_end_matches(&['\r', '\n'][..]).to_string())
}

#[cfg(not(unix))]
fn read_password_raw() -> Result<String> {
    let mut password = String::new();
    io::stdin()
        .lock()
        .read_line(&mut password)
        .map_err(|e| StrawsError::Io(e))?;
    Ok(password.trim_end_matches(&['\r', '\n'][..]).to_string())
}
