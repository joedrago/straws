// Copyright (c) 2025, Joe Drago <joedrago@gmail.com>
// SPDX-License-Identifier: BSD-2-Clause

use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crossterm::{
    cursor::{Hide, MoveTo, Show},
    execute,
    style::{Attribute, Color, Print, ResetColor, SetAttribute, SetForegroundColor},
    terminal::{self, Clear, ClearType},
};

use super::speed::{format_bytes, format_count, format_duration, format_duration_f64, format_speed};
use super::tracker::ProgressTracker;
use crate::agent::{AgentPool, AgentState};

const RENDER_INTERVAL_MS: u64 = 250;
const PROGRESS_BAR_WIDTH: usize = 40;
const CELL_WIDTH: usize = 32;

pub struct ProgressDisplay {
    tracker: Arc<ProgressTracker>,
    pool: Arc<AgentPool>,
    verbose: bool,
    running: AtomicBool,
}

impl ProgressDisplay {
    pub fn new(tracker: Arc<ProgressTracker>, pool: Arc<AgentPool>, verbose: bool) -> Self {
        ProgressDisplay {
            tracker,
            pool,
            verbose,
            running: AtomicBool::new(false),
        }
    }

    /// Start the progress display loop
    pub async fn run(&self) {
        self.running.store(true, Ordering::SeqCst);

        // Hide cursor
        let mut stdout = io::stdout();
        let _ = execute!(stdout, Hide);

        while self.running.load(Ordering::SeqCst) {
            self.render();
            tokio::time::sleep(Duration::from_millis(RENDER_INTERVAL_MS)).await;
        }

        // Show cursor and clear
        let _ = execute!(stdout, Show);
    }

    /// Stop the progress display
    pub fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
    }

    /// Render one final frame (call after stop, before print_summary)
    pub fn render_final(&self) {
        self.render();
    }

    fn render(&self) {
        let mut stdout = io::stdout();

        // Clear screen and move to top
        let _ = execute!(stdout, MoveTo(0, 0), Clear(ClearType::All));

        // Get terminal size
        let (term_width, _) = terminal::size().unwrap_or((80, 24));
        let width = term_width as usize;

        // Header: source → destination
        let source = self.tracker.source_desc();
        let dest = self.tracker.dest_desc();
        if !source.is_empty() {
            let max_len = width.saturating_sub(6); // "  · " / "  → " prefix + margin
            let src_display = truncate_middle(&source, max_len);
            let dest_display = truncate_middle(&dest, max_len);
            let _ = execute!(stdout, Print("\n"));
            let _ = execute!(
                stdout,
                Print("  "),
                SetForegroundColor(Color::DarkGrey),
                Print("· "),
                Print(&src_display),
                Print("\n"),
                Print("  → "),
                Print(&dest_display),
                ResetColor,
                Print("\n")
            );
        }

        // Progress bar
        let percent = self.tracker.progress_percent();
        let filled = ((percent / 100.0) * PROGRESS_BAR_WIDTH as f64) as usize;
        let empty = PROGRESS_BAR_WIDTH.saturating_sub(filled);

        let bar_filled: String = "█".repeat(filled);
        let bar_empty: String = "░".repeat(empty);

        // Stats
        let transferred = self.tracker.bytes_transferred();
        let total = self.tracker.total_bytes();
        let speed = self.tracker.speed_bps();
        let eta = self.tracker.eta_secs();

        let eta_str = eta
            .map(format_duration)
            .unwrap_or_else(|| "--:--".to_string());

        let _ = execute!(stdout, Print("\n"));
        let _ = execute!(
            stdout,
            Print("  "),
            SetAttribute(Attribute::Bold),
            SetForegroundColor(Color::Rgb { r: 180, g: 150, b: 255 }),
            Print(&bar_filled),
            SetAttribute(Attribute::Reset),
            SetForegroundColor(Color::DarkGrey),
            Print(&bar_empty),
            ResetColor,
            Print(format!(" {:5.1}%\n", percent))
        );

        let _ = execute!(stdout, Print("\n"));
        let _ = execute!(
            stdout,
            Print("  Progress: "),
            SetForegroundColor(Color::Yellow),
            Print(format_bytes(transferred)),
            ResetColor,
            Print(" / "),
            SetForegroundColor(Color::Yellow),
            Print(format_bytes(total)),
            ResetColor,
            Print("\n")
        );
        let _ = execute!(
            stdout,
            Print("  Speed   : "),
            SetForegroundColor(Color::Cyan),
            Print(format_speed(speed)),
            ResetColor,
            Print("\n")
        );
        let elapsed_transfer = self.tracker.current_phase_elapsed_secs();
        let _ = execute!(
            stdout,
            Print("  Elapsed : "),
            SetAttribute(Attribute::Bold),
            Print(format_duration_f64(elapsed_transfer)),
            SetAttribute(Attribute::Reset),
            Print("\n")
        );
        let _ = execute!(
            stdout,
            Print("  ETA     : "),
            SetAttribute(Attribute::Bold),
            Print(&eta_str),
            SetAttribute(Attribute::Reset),
            Print("\n")
        );

        // File counts
        let files_completed = self.tracker.files_completed();
        let total_files = self.tracker.total_files();
        let files_failed = self.tracker.files_failed();

        let _ = execute!(
            stdout,
            Print(format!("  Files   : {} / {}", format_count(files_completed), format_count(total_files)))
        );

        if files_failed > 0 {
            let _ = execute!(
                stdout,
                Print(" ("),
                SetForegroundColor(Color::Red),
                Print(format!("{} failed", format_count(files_failed))),
                ResetColor,
                Print(")")
            );
        }
        let _ = execute!(stdout, Print("\n"));

        // Tunnel grid (always show)
        let _ = execute!(stdout, Print("\n"));
        let _ = execute!(stdout, Print("  Agents:\n"));

        self.render_agent_grid(&mut stdout, width);

        // Log events (errors, retries, etc.)
        let log_events = self.tracker.log_events();
        if !log_events.is_empty() {
            let _ = execute!(stdout, Print("\n"));
            let _ = execute!(stdout, Print("  Log:\n"));
            for event in log_events.iter().rev().take(5) {
                let color = if event.is_error { Color::Red } else { Color::Yellow };
                let _ = execute!(
                    stdout,
                    Print("    "),
                    SetForegroundColor(color),
                    Print(truncate(&event.message, width.saturating_sub(6))),
                    ResetColor,
                    Print("\n")
                );
            }
        }

        // Recent completed files
        let recent = self.tracker.recent_files();
        if !recent.is_empty() {
            let _ = execute!(stdout, Print("\n"));
            let _ = execute!(stdout, Print("  Recent:\n"));
            for file in recent.iter().rev().take(5) {
                let extra_info = if self.verbose {
                    // Verbose: show MD5, mode, mtime
                    let md5_str = if let (Some(local), Some(remote)) = (&file.local_md5, &file.remote_md5) {
                        format!(" ({} = {})", local, remote)
                    } else {
                        String::new()
                    };
                    format!("{} mode={:04o} mtime={}", md5_str, file.mode & 0o7777, file.mtime)
                } else {
                    // Non-verbose: just show checkmark if verified
                    if file.local_md5.is_some() {
                        " ✓".to_string()
                    } else {
                        String::new()
                    }
                };
                let display_str = format!("{}{}", file.name, extra_info);
                let _ = execute!(
                    stdout,
                    Print("    "),
                    SetForegroundColor(Color::Green),
                    Print("✓ "),
                    ResetColor,
                    Print(truncate(&display_str, width.saturating_sub(6))),
                    Print("\n")
                );
            }
        }

        let _ = stdout.flush();
    }

    fn render_agent_grid(&self, stdout: &mut io::Stdout, term_width: usize) {
        let agents = self.pool.agents();
        let agent_jobs = self.tracker.agent_jobs();

        let indent = 4;
        // Calculate how many cells fit per row: indent + (CELL_WIDTH * cols) <= term_width
        let available_width = term_width.saturating_sub(indent);
        let cols = std::cmp::max(1, available_width / CELL_WIDTH);

        for (i, agent) in agents.iter().enumerate() {
            // Print newline and indent for new rows
            if i % cols == 0 {
                if i > 0 {
                    let _ = execute!(stdout, Print("\n"));
                }
                let _ = execute!(stdout, Print("    ")); // indent only at start of row
            }

            let state = agent.state();
            let (symbol, color) = match state {
                AgentState::Starting => ("○", Color::Yellow),
                AgentState::Ready => ("○", Color::DarkGrey),
                AgentState::Busy => ("●", Color::Rgb { r: 120, g: 100, b: 180 }),
                AgentState::Unhealthy => ("✕", Color::Red),
            };

            let job_desc = agent_jobs
                .get(i)
                .and_then(|j| j.clone())
                .unwrap_or_else(|| "-".to_string());

            // Format: "●00:filename.ext" with fixed width
            let id_str = format!("{:02}", i);
            let max_name_len = CELL_WIDTH.saturating_sub(5); // symbol + id(2) + colon + spacing
            let name = truncate(&job_desc, max_name_len);

            let cell = format!("{}{}:{}", symbol, id_str, name);
            let padding = CELL_WIDTH.saturating_sub(visible_len(&cell));

            let _ = execute!(
                stdout,
                SetForegroundColor(color),
                Print(&cell),
                ResetColor,
                Print(" ".repeat(padding))
            );
        }
        let _ = execute!(stdout, Print("\n"));
    }

    /// Print final summary
    pub fn print_summary(&self) {
        let mut stdout = io::stdout();
        let _ = execute!(stdout, Show, Print("\n\n"));

        let elapsed = self.tracker.elapsed_secs();
        let transferred = self.tracker.bytes_transferred();
        let files_completed = self.tracker.files_completed();
        let files_failed = self.tracker.files_failed();
        let files_skipped = self.tracker.files_skipped();
        let timing = self.tracker.phase_timing();

        let avg_speed = if elapsed > 0 {
            transferred as f64 / elapsed as f64
        } else {
            0.0
        };

        let _ = execute!(
            stdout,
            SetForegroundColor(Color::Green),
            Print("✓"),
            ResetColor,
            Print(" Transfer complete: "),
            SetForegroundColor(Color::Yellow),
            Print(format_bytes(transferred)),
            ResetColor,
            Print("\n")
        );

        let _ = execute!(
            stdout,
            Print("  Average speed: "),
            SetForegroundColor(Color::Cyan),
            Print(format_speed(avg_speed)),
            ResetColor,
            Print("\n")
        );

        // Timing breakdown
        let total_secs = timing.connect_secs + timing.index_secs + timing.transfer_secs;
        let _ = execute!(
            stdout,
            Print("  Timing: "),
            SetAttribute(Attribute::Bold),
            Print(format_duration_f64(total_secs)),
            SetAttribute(Attribute::Reset),
            Print(" (connect: "),
            SetAttribute(Attribute::Bold),
            Print(format_duration_f64(timing.connect_secs)),
            SetAttribute(Attribute::Reset),
            Print(", index: "),
            SetAttribute(Attribute::Bold),
            Print(format_duration_f64(timing.index_secs)),
            SetAttribute(Attribute::Reset),
            Print(", transfer: "),
            SetAttribute(Attribute::Bold),
            Print(format_duration_f64(timing.transfer_secs)),
            SetAttribute(Attribute::Reset),
            Print(")\n")
        );

        if files_completed > 1 {
            let _ = execute!(
                stdout,
                Print("  Files: "),
                SetForegroundColor(Color::Green),
                Print(format_count(files_completed)),
                ResetColor,
                Print("\n")
            );
        }

        if files_skipped > 0 {
            let _ = execute!(
                stdout,
                Print("  Skipped (already complete): "),
                SetForegroundColor(Color::Green),
                Print(format_count(files_skipped)),
                ResetColor,
                Print("\n")
            );
        }

        if files_failed > 0 {
            let _ = execute!(
                stdout,
                Print("  "),
                SetForegroundColor(Color::Red),
                Print(format!("Failed: {}\n", format_count(files_failed))),
                ResetColor
            );

            // Show details for each failed file
            let failed_files = self.tracker.failed_files();
            let _ = execute!(stdout, Print("\n"));
            let _ = execute!(
                stdout,
                Print("  "),
                SetForegroundColor(Color::Red),
                SetAttribute(Attribute::Bold),
                Print("Failed files:\n"),
                SetAttribute(Attribute::Reset),
                ResetColor
            );

            let max_to_show = 20;
            for (i, failed) in failed_files.iter().enumerate() {
                if i >= max_to_show {
                    let remaining = failed_files.len() - max_to_show;
                    let _ = execute!(
                        stdout,
                        Print("    "),
                        SetForegroundColor(Color::DarkGrey),
                        Print(format!("... and {} more\n", remaining)),
                        ResetColor
                    );
                    break;
                }

                // Show remote path (what we were trying to transfer)
                let _ = execute!(
                    stdout,
                    Print("    "),
                    SetForegroundColor(Color::Red),
                    Print("✗ "),
                    ResetColor,
                    Print(&failed.remote_path),
                    Print("\n")
                );

                // Show the error reason indented
                let _ = execute!(
                    stdout,
                    Print("      "),
                    SetForegroundColor(Color::Yellow),
                    Print(&failed.error),
                    ResetColor,
                    Print("\n")
                );
            }
        }

        println!();
        let _ = stdout.flush();
    }
}

fn truncate_middle(s: &str, max_len: usize) -> String {
    let len = s.chars().count();
    if len <= max_len {
        return s.to_string();
    }
    if max_len <= 3 {
        return s.chars().take(max_len).collect();
    }
    let keep = max_len - 3; // room for "..."
    let front = (keep + 1) / 2;
    let back = keep / 2;
    let head: String = s.chars().take(front).collect();
    let tail: String = s.chars().skip(len - back).collect();
    format!("{}...{}", head, tail)
}

fn truncate(s: &str, max_len: usize) -> String {
    if s.chars().count() <= max_len {
        s.to_string()
    } else if max_len <= 3 {
        s.chars().take(max_len).collect()
    } else {
        let truncated: String = s.chars().take(max_len - 3).collect();
        format!("{}...", truncated)
    }
}

/// Get visible length of string (ignoring ANSI codes)
fn visible_len(s: &str) -> usize {
    // Simple implementation - just count non-ANSI chars
    // This works because our strings don't have ANSI codes at this point
    s.chars().count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_truncate_ascii() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello world", 8), "hello...");
        assert_eq!(truncate("ab", 2), "ab");
        assert_eq!(truncate("abcd", 3), "abc");
    }

    #[test]
    fn test_truncate_unicode() {
        // Multi-byte characters should not panic
        assert_eq!(truncate("日本語テスト", 10), "日本語テスト");
        assert_eq!(truncate("日本語テスト", 5), "日本...");
        assert_eq!(truncate("日本語テスト.txt", 6), "日本語...");

        // Emoji (4-byte chars)
        assert_eq!(truncate("🎉🎊🎈test", 5), "🎉🎊...");

        // Mixed ASCII and multi-byte
        assert_eq!(truncate("café_naïve_test", 10), "café_na...");
    }

    #[test]
    fn test_truncate_edge_cases() {
        assert_eq!(truncate("", 5), "");
        assert_eq!(truncate("a", 1), "a");
        assert_eq!(truncate("ab", 1), "a");
        assert_eq!(truncate("abcdef", 0), "");
    }
}
