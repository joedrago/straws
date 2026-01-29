// Copyright (c) 2025, Joe Drago <joedrago@gmail.com>
// SPDX-License-Identifier: BSD-2-Clause

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

use parking_lot::Mutex;

use super::speed::SpeedTracker;

/// A log event for display
#[derive(Clone)]
pub struct LogEvent {
    pub message: String,
    pub is_error: bool,
}

/// A completed file with optional verification info
#[derive(Clone)]
pub struct CompletedFile {
    pub name: String,
    pub local_md5: Option<String>,
    pub remote_md5: Option<String>,
    pub mode: u32,
    pub mtime: u64,
}

/// Thread-safe progress tracking
pub struct ProgressTracker {
    total_bytes: AtomicU64,
    bytes_transferred: AtomicU64,
    total_files: AtomicU64,
    files_completed: AtomicU64,
    files_failed: AtomicU64,
    files_skipped: AtomicU64,
    speed_tracker: Mutex<SpeedTracker>,
    start_time: Instant,
    recent_files: Mutex<Vec<CompletedFile>>,
    agent_jobs: Mutex<Vec<Option<String>>>, // Current job description per agent
    log_events: Mutex<Vec<LogEvent>>,
    verify_enabled: AtomicBool,
    source_desc: Mutex<String>,
    dest_desc: Mutex<String>,
}

impl ProgressTracker {
    pub fn new(tunnel_count: usize) -> Self {
        ProgressTracker {
            total_bytes: AtomicU64::new(0),
            bytes_transferred: AtomicU64::new(0),
            total_files: AtomicU64::new(0),
            files_completed: AtomicU64::new(0),
            files_failed: AtomicU64::new(0),
            files_skipped: AtomicU64::new(0),
            speed_tracker: Mutex::new(SpeedTracker::new()),
            start_time: Instant::now(),
            recent_files: Mutex::new(Vec::new()),
            agent_jobs: Mutex::new(vec![None; tunnel_count]),
            log_events: Mutex::new(Vec::new()),
            verify_enabled: AtomicBool::new(false),
            source_desc: Mutex::new(String::new()),
            dest_desc: Mutex::new(String::new()),
        }
    }

    pub fn set_totals(&self, bytes: u64, files: u64) {
        self.total_bytes.store(bytes, Ordering::Relaxed);
        self.total_files.store(files, Ordering::Relaxed);
    }

    pub fn set_descriptions(&self, source: &str, dest: &str) {
        *self.source_desc.lock() = source.to_string();
        *self.dest_desc.lock() = dest.to_string();
    }

    pub fn set_verify_enabled(&self, enabled: bool) {
        self.verify_enabled.store(enabled, Ordering::Relaxed);
    }

    pub fn verify_enabled(&self) -> bool {
        self.verify_enabled.load(Ordering::Relaxed)
    }

    pub fn source_desc(&self) -> String {
        self.source_desc.lock().clone()
    }

    pub fn dest_desc(&self) -> String {
        self.dest_desc.lock().clone()
    }

    pub fn log_event(&self, message: &str, is_error: bool) {
        let mut events = self.log_events.lock();
        events.push(LogEvent {
            message: message.to_string(),
            is_error,
        });
        // Keep last 5 events
        while events.len() > 5 {
            events.remove(0);
        }
    }

    pub fn log_events(&self) -> Vec<LogEvent> {
        self.log_events.lock().clone()
    }

    pub fn file_skipped(&self) {
        self.files_skipped.fetch_add(1, Ordering::Relaxed);
    }

    pub fn set_files_skipped(&self, count: u64) {
        self.files_skipped.store(count, Ordering::Relaxed);
    }

    pub fn files_skipped(&self) -> u64 {
        self.files_skipped.load(Ordering::Relaxed)
    }

    pub fn add_bytes(&self, bytes: u64) {
        self.bytes_transferred.fetch_add(bytes, Ordering::Relaxed);
        self.speed_tracker.lock().add_bytes(bytes);
    }

    pub fn file_completed(&self, name: &str, local_md5: Option<String>, remote_md5: Option<String>, mode: u32, mtime: u64) {
        self.files_completed.fetch_add(1, Ordering::Relaxed);

        let mut recent = self.recent_files.lock();
        recent.push(CompletedFile {
            name: name.to_string(),
            local_md5,
            remote_md5,
            mode,
            mtime,
        });
        if recent.len() > 5 {
            recent.remove(0);
        }
    }

    pub fn file_failed(&self) {
        self.files_failed.fetch_add(1, Ordering::Relaxed);
    }

    pub fn set_agent_job(&self, agent_id: usize, job_desc: Option<String>) {
        let mut jobs = self.agent_jobs.lock();
        if agent_id < jobs.len() {
            jobs[agent_id] = job_desc;
        }
    }

    pub fn total_bytes(&self) -> u64 {
        self.total_bytes.load(Ordering::Relaxed)
    }

    pub fn bytes_transferred(&self) -> u64 {
        self.bytes_transferred.load(Ordering::Relaxed)
    }

    pub fn total_files(&self) -> u64 {
        self.total_files.load(Ordering::Relaxed)
    }

    pub fn files_completed(&self) -> u64 {
        self.files_completed.load(Ordering::Relaxed)
    }

    pub fn files_failed(&self) -> u64 {
        self.files_failed.load(Ordering::Relaxed)
    }

    pub fn speed_bps(&self) -> f64 {
        self.speed_tracker.lock().speed_bps()
    }

    pub fn elapsed_secs(&self) -> u64 {
        self.start_time.elapsed().as_secs()
    }

    pub fn eta_secs(&self) -> Option<u64> {
        let speed = self.speed_bps();
        if speed <= 0.0 {
            return None;
        }

        let remaining = self.total_bytes().saturating_sub(self.bytes_transferred());
        if remaining == 0 {
            return Some(0);
        }

        Some((remaining as f64 / speed) as u64)
    }

    pub fn progress_percent(&self) -> f64 {
        let total = self.total_bytes();
        if total == 0 {
            return 100.0;
        }
        (self.bytes_transferred() as f64 / total as f64) * 100.0
    }

    pub fn recent_files(&self) -> Vec<CompletedFile> {
        self.recent_files.lock().clone()
    }

    pub fn agent_jobs(&self) -> Vec<Option<String>> {
        self.agent_jobs.lock().clone()
    }
}
