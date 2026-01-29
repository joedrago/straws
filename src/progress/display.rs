// Copyright (c) 2025, Joe Drago <joedrago@gmail.com>
// SPDX-License-Identifier: BSD-2-Clause

use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crossterm::{
    cursor::{Hide, MoveTo, Show},
    execute,
    style::{Color, Print, ResetColor, SetForegroundColor},
    terminal::{self, Clear, ClearType},
};

use super::speed::{format_bytes, format_duration, format_speed};
use super::tracker::ProgressTracker;
use crate::agent::{AgentPool, AgentState};

const RENDER_INTERVAL_MS: u64 = 250;
const PROGRESS_BAR_WIDTH: usize = 40;
const CELL_WIDTH: usize = 22;

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
            let _ = execute!(stdout, Print("\n"));
            let _ = execute!(
                stdout,
                Print("  "),
                SetForegroundColor(Color::Cyan),
                Print(&source),
                ResetColor,
                SetForegroundColor(Color::DarkGrey),
                Print(" → "),
                ResetColor,
                SetForegroundColor(Color::Green),
                Print(&dest),
                ResetColor,
                Print("\n")
            );
        }

        // Progress bar
        let percent = self.tracker.progress_percent();
        let filled = ((percent / 100.0) * PROGRESS_BAR_WIDTH as f64) as usize;
        let empty = PROGRESS_BAR_WIDTH.saturating_sub(filled);

        let bar: String = "█".repeat(filled) + &"░".repeat(empty);

        // Stats
        let transferred = self.tracker.bytes_transferred();
        let total = self.tracker.total_bytes();
        let speed = self.tracker.speed_bps();
        let eta = self.tracker.eta_secs();

        let eta_str = eta
            .map(|s| format_duration(s))
            .unwrap_or_else(|| "--:--".to_string());

        let _ = execute!(stdout, Print("\n"));
        let _ = execute!(
            stdout,
            Print("  "),
            SetForegroundColor(Color::Green),
            Print(&bar),
            ResetColor,
            Print(format!(" {:5.1}%\n", percent))
        );

        let _ = execute!(stdout, Print("\n"));
        let _ = execute!(
            stdout,
            Print(format!("  Progress: {}/{}\n", format_bytes(transferred), format_bytes(total)))
        );
        let _ = execute!(
            stdout,
            Print("  Speed   : "),
            SetForegroundColor(Color::Cyan),
            Print(format_speed(speed)),
            ResetColor,
            Print("\n")
        );
        let _ = execute!(
            stdout,
            Print("  ETA     : "),
            SetForegroundColor(Color::Yellow),
            Print(&eta_str),
            ResetColor,
            Print("\n")
        );

        // File counts
        let files_completed = self.tracker.files_completed();
        let total_files = self.tracker.total_files();
        let files_failed = self.tracker.files_failed();

        let _ = execute!(
            stdout,
            Print(format!("  Files   : {}/{}", files_completed, total_files))
        );

        if files_failed > 0 {
            let _ = execute!(
                stdout,
                Print(" ("),
                SetForegroundColor(Color::Red),
                Print(format!("{} failed", files_failed)),
                ResetColor,
                Print(")")
            );
        }
        let _ = execute!(stdout, Print("\n"));

        // Tunnel grid (always show)
        let _ = execute!(stdout, Print("\n"));
        let _ = execute!(
            stdout,
            SetForegroundColor(Color::DarkGrey),
            Print("  Agents:\n"),
            ResetColor
        );

        self.render_agent_grid(&mut stdout, width);

        // Log events (errors, retries, etc.)
        let log_events = self.tracker.log_events();
        if !log_events.is_empty() {
            let _ = execute!(stdout, Print("\n"));
            let _ = execute!(
                stdout,
                SetForegroundColor(Color::DarkGrey),
                Print("  Log:\n"),
                ResetColor
            );
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

        // Recent files (if verbose)
        if self.verbose {
            let recent = self.tracker.recent_files();
            if !recent.is_empty() {
                let _ = execute!(stdout, Print("\n"));
                let _ = execute!(
                    stdout,
                    SetForegroundColor(Color::DarkGrey),
                    Print("  Recent:\n"),
                    ResetColor
                );
                for file in recent.iter().rev().take(5) {
                    let _ = execute!(
                        stdout,
                        Print("    "),
                        SetForegroundColor(Color::Green),
                        Print("✓ "),
                        ResetColor,
                        Print(truncate(file, width.saturating_sub(6))),
                        Print("\n")
                    );
                }
            }
        }

        let _ = stdout.flush();
    }

    fn render_agent_grid(&self, stdout: &mut io::Stdout, term_width: usize) {
        let agents = self.pool.agents();
        let agent_jobs = self.tracker.agent_jobs();

        let indent = 4;
        let available_width = term_width.saturating_sub(indent);
        let cols = std::cmp::max(1, available_width / CELL_WIDTH);

        for (i, agent) in agents.iter().enumerate() {
            if i % cols == 0 && i > 0 {
                let _ = execute!(stdout, Print("\n"));
            }

            let state = agent.state();
            let (symbol, color) = match state {
                AgentState::Starting => ("○", Color::Yellow),
                AgentState::Ready => ("○", Color::DarkGrey),
                AgentState::Busy => ("●", Color::Green),
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
                Print("    "),
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
        let _ = execute!(stdout, Clear(ClearType::All), MoveTo(0, 0), Show);

        let elapsed = self.tracker.elapsed_secs();
        let transferred = self.tracker.bytes_transferred();
        let files_completed = self.tracker.files_completed();
        let files_failed = self.tracker.files_failed();
        let files_verified = self.tracker.files_verified();

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
            Print(format!(
                " Transfer complete: {} in {}\n",
                format_bytes(transferred),
                format_duration(elapsed)
            ))
        );

        let _ = execute!(
            stdout,
            SetForegroundColor(Color::DarkGrey),
            Print(format!("  Average speed: {}\n", format_speed(avg_speed))),
            ResetColor
        );

        if files_completed > 1 {
            let _ = execute!(
                stdout,
                SetForegroundColor(Color::DarkGrey),
                Print(format!("  Files: {}\n", files_completed)),
                ResetColor
            );
        }

        if self.tracker.verify_enabled() {
            let _ = execute!(
                stdout,
                SetForegroundColor(Color::DarkGrey),
                Print(format!("  Verified: {} files\n", files_verified)),
                ResetColor
            );
        }

        if files_failed > 0 {
            let _ = execute!(
                stdout,
                Print("  "),
                SetForegroundColor(Color::Red),
                Print(format!("Failed: {}\n", files_failed)),
                ResetColor
            );
        }

        println!();
        let _ = stdout.flush();
    }
}

fn truncate(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else if max_len <= 3 {
        "...".to_string()
    } else {
        format!("{}...", &s[..max_len - 3])
    }
}

/// Get visible length of string (ignoring ANSI codes)
fn visible_len(s: &str) -> usize {
    // Simple implementation - just count non-ANSI chars
    // This works because our strings don't have ANSI codes at this point
    s.chars().count()
}
