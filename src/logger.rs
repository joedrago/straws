// Copyright (c) 2025, Joe Drago <joedrago@gmail.com>
// SPDX-License-Identifier: BSD-2-Clause

use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::Mutex;

use crate::error::{Result, StrawsError};

/// Line-buffered debug logger
pub struct Logger {
    writer: Option<Mutex<BufWriter<File>>>,
}

impl Logger {
    pub fn new(path: Option<&Path>) -> Result<Self> {
        let writer = match path {
            Some(p) => {
                let file = OpenOptions::new()
                    .create(true)
                    .write(true)
                    .truncate(true)
                    .open(p)
                    .map_err(|e| StrawsError::Io(e))?;
                Some(Mutex::new(BufWriter::new(file)))
            }
            None => None,
        };
        Ok(Logger { writer })
    }

    pub fn log(&self, msg: &str) {
        if let Some(ref writer) = self.writer {
            if let Ok(mut w) = writer.lock() {
                let timestamp = chrono_lite_timestamp();
                let _ = writeln!(w, "[{}] {}", timestamp, msg);
                let _ = w.flush();
            }
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.writer.is_some()
    }
}

fn chrono_lite_timestamp() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    let millis = now.subsec_millis();

    // Simple timestamp without chrono dependency
    format!("{}.{:03}", secs, millis)
}

/// Global logger instance
static LOGGER: std::sync::OnceLock<Logger> = std::sync::OnceLock::new();

pub fn init_logger(path: Option<&Path>) -> Result<()> {
    let logger = Logger::new(path)?;
    LOGGER.get_or_init(|| logger);
    Ok(())
}

pub fn log_debug(msg: &str) {
    if let Some(logger) = LOGGER.get() {
        logger.log(msg);
    }
}

#[macro_export]
macro_rules! debug_log {
    ($($arg:tt)*) => {
        $crate::logger::log_debug(&format!($($arg)*))
    };
}
