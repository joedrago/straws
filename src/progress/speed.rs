// Copyright (c) 2025, Joe Drago <joedrago@gmail.com>
// SPDX-License-Identifier: BSD-2-Clause

use std::time::Instant;

/// Rolling window speed calculator
/// Uses 20 buckets x 50ms = 1 second window for responsive speed updates
pub struct SpeedTracker {
    buckets: Vec<u64>,
    bucket_count: usize,
    bucket_duration_ms: u64,
    current_bucket: usize,
    last_update: Instant,
    total_bytes: u64,
}

impl SpeedTracker {
    pub fn new() -> Self {
        let bucket_count = 20;
        SpeedTracker {
            buckets: vec![0; bucket_count],
            bucket_count,
            bucket_duration_ms: 50,
            current_bucket: 0,
            last_update: Instant::now(),
            total_bytes: 0,
        }
    }

    /// Add bytes transferred
    pub fn add_bytes(&mut self, bytes: u64) {
        self.advance_buckets();
        self.buckets[self.current_bucket] += bytes;
        self.total_bytes += bytes;
    }

    /// Get current speed in bytes per second
    pub fn speed_bps(&mut self) -> f64 {
        self.advance_buckets();

        let total: u64 = self.buckets.iter().sum();
        let window_seconds = (self.bucket_count as f64 * self.bucket_duration_ms as f64) / 1000.0;

        total as f64 / window_seconds
    }

    /// Get total bytes transferred
    pub fn total_bytes(&self) -> u64 {
        self.total_bytes
    }

    fn advance_buckets(&mut self) {
        let now = Instant::now();
        let elapsed_ms = now.duration_since(self.last_update).as_millis() as u64;

        if elapsed_ms >= self.bucket_duration_ms {
            let buckets_to_advance = (elapsed_ms / self.bucket_duration_ms) as usize;

            for _ in 0..std::cmp::min(buckets_to_advance, self.bucket_count) {
                self.current_bucket = (self.current_bucket + 1) % self.bucket_count;
                self.buckets[self.current_bucket] = 0;
            }

            self.last_update = now;
        }
    }
}

impl Default for SpeedTracker {
    fn default() -> Self {
        Self::new()
    }
}

/// Format bytes per second as human-readable string
pub fn format_speed(bps: f64) -> String {
    if bps >= 1_000_000_000.0 {
        format!("{:.1} GB/s", bps / 1_000_000_000.0)
    } else if bps >= 1_000_000.0 {
        format!("{:.1} MB/s", bps / 1_000_000.0)
    } else if bps >= 1_000.0 {
        format!("{:.1} KB/s", bps / 1_000.0)
    } else {
        format!("{:.0} B/s", bps)
    }
}

/// Format bytes as human-readable string
pub fn format_bytes(bytes: u64) -> String {
    if bytes >= 1_000_000_000 {
        format!("{:.1} GB", bytes as f64 / 1_000_000_000.0)
    } else if bytes >= 1_000_000 {
        format!("{:.1} MB", bytes as f64 / 1_000_000.0)
    } else if bytes >= 1_000 {
        format!("{:.1} KB", bytes as f64 / 1_000.0)
    } else {
        format!("{} B", bytes)
    }
}

/// Format duration as human-readable string
pub fn format_duration(seconds: u64) -> String {
    if seconds >= 3600 {
        let hours = seconds / 3600;
        let minutes = (seconds % 3600) / 60;
        format!("{}h{}m", hours, minutes)
    } else if seconds >= 60 {
        let minutes = seconds / 60;
        let secs = seconds % 60;
        format!("{}m{}s", minutes, secs)
    } else {
        format!("{}s", seconds)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_speed() {
        assert_eq!(format_speed(500.0), "500 B/s");
        assert_eq!(format_speed(1500.0), "1.5 KB/s");
        assert_eq!(format_speed(1_500_000.0), "1.5 MB/s");
        assert_eq!(format_speed(1_500_000_000.0), "1.5 GB/s");
    }

    #[test]
    fn test_format_bytes() {
        assert_eq!(format_bytes(500), "500 B");
        assert_eq!(format_bytes(1500), "1.5 KB");
        assert_eq!(format_bytes(1_500_000), "1.5 MB");
        assert_eq!(format_bytes(1_500_000_000), "1.5 GB");
    }

    #[test]
    fn test_format_duration() {
        assert_eq!(format_duration(30), "30s");
        assert_eq!(format_duration(90), "1m30s");
        assert_eq!(format_duration(3700), "1h1m");
    }
}
