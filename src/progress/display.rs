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
            .unwrap_or_else(|| "--".to_string());

        let progress_line = format!(
            " {:5.1}% [{}] {}/{} {} ETA: {}",
            percent,
            bar,
            format_bytes(transferred),
            format_bytes(total),
            format_speed(speed),
            eta_str
        );

        // Print progress line
        let _ = execute!(
            stdout,
            SetForegroundColor(Color::Cyan),
            Print(&progress_line[..std::cmp::min(progress_line.len(), width)]),
            ResetColor,
            Print("\n\n")
        );

        // File counts
        let files_completed = self.tracker.files_completed();
        let total_files = self.tracker.total_files();
        let files_failed = self.tracker.files_failed();

        let files_line = format!(
            " Files: {}/{} completed",
            files_completed, total_files
        );
        let _ = execute!(stdout, Print(&files_line));

        if files_failed > 0 {
            let _ = execute!(
                stdout,
                Print(", "),
                SetForegroundColor(Color::Red),
                Print(format!("{} failed", files_failed)),
                ResetColor
            );
        }
        let _ = execute!(stdout, Print("\n"));

        // Tunnel status (if verbose)
        if self.verbose {
            let _ = execute!(stdout, Print("\n Tunnels:\n"));

            let agents = self.pool.agents();
            let agent_jobs = self.tracker.agent_jobs();
            let cells_per_row = std::cmp::max(1, width / 25);

            for (i, agent) in agents.iter().enumerate() {
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
                    .unwrap_or_else(|| "idle".to_string());

                let cell = format!(" {}#{}: {}", symbol, i, truncate(&job_desc, 15));

                let _ = execute!(
                    stdout,
                    SetForegroundColor(color),
                    Print(&cell),
                    ResetColor
                );

                if (i + 1) % cells_per_row == 0 {
                    let _ = execute!(stdout, Print("\n"));
                }
            }
            let _ = execute!(stdout, Print("\n"));
        }

        // Recent files
        let recent = self.tracker.recent_files();
        if !recent.is_empty() {
            let _ = execute!(stdout, Print("\n Recent:\n"));
            for file in recent.iter().rev().take(5) {
                let _ = execute!(
                    stdout,
                    SetForegroundColor(Color::Green),
                    Print(" ✓ "),
                    ResetColor,
                    Print(truncate(file, width.saturating_sub(4))),
                    Print("\n")
                );
            }
        }

        let _ = stdout.flush();
    }

    /// Print final summary
    pub fn print_summary(&self) {
        let mut stdout = io::stdout();
        let _ = execute!(stdout, Clear(ClearType::All), MoveTo(0, 0), Show);

        let elapsed = self.tracker.elapsed_secs();
        let transferred = self.tracker.bytes_transferred();
        let files_completed = self.tracker.files_completed();
        let files_failed = self.tracker.files_failed();

        let avg_speed = if elapsed > 0 {
            transferred as f64 / elapsed as f64
        } else {
            0.0
        };

        println!("\n Transfer complete!");
        println!(
            " {} transferred in {}",
            format_bytes(transferred),
            format_duration(elapsed)
        );
        println!(" Average speed: {}", format_speed(avg_speed));
        println!(" Files: {} completed", files_completed);

        if files_failed > 0 {
            let _ = execute!(
                stdout,
                Print(" "),
                SetForegroundColor(Color::Red),
                Print(format!("{} failed\n", files_failed)),
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
