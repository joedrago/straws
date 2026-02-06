use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use clap::Parser;
use tokio::signal;
use tokio::sync::Notify;

use straws::agent::AgentPool;
use straws::auth::get_password;
use straws::config::{Args, Config, Direction};
use straws::debug_log;
use straws::error::{Result, StrawsError};
use straws::file::finalize::{cleanup_temp, finalize_file};
use straws::job::types::FileMeta;
use straws::job::{Job, JobQueue, JobScheduler};
use straws::logger::init_logger;
use straws::progress::{format_count, ProgressDisplay, ProgressTracker};
use straws::transfer::{download_job, upload_job};

const MAX_RETRIES: u32 = 3;

fn check_sshpass_available() -> Result<()> {
    match std::process::Command::new("sshpass").arg("-V").output() {
        Ok(output) if output.status.success() => Ok(()),
        _ => Err(StrawsError::Config(
            "Password authentication requires 'sshpass' to be installed.\n\
             Install it with:\n  \
               macOS:  brew install sshpass\n  \
               Ubuntu: sudo apt install sshpass\n  \
               Fedora: sudo dnf install sshpass".to_string()
        )),
    }
}

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("Error: {}", e);
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    let args = Args::parse();

    // Initialize debug logger
    init_logger(args.debug_log.as_deref())?;
    debug_log!("Starting straws");

    // Get password if needed
    let password = get_password(
        args.password_prompt,
        args.password_file.as_deref(),
        &args.password_env,
    )?;

    // If password is provided, verify sshpass is available
    if password.is_some() {
        check_sshpass_available()?;
    }

    // Build config
    let config = Config::from_args(args.clone(), password)?;
    debug_log!("Config: direction={:?}, remote={}", config.direction, config.remote.user_host());

    // Setup shutdown handling
    let abort_flag = Arc::new(AtomicBool::new(false));
    let shutdown_notify = Arc::new(Notify::new());

    let abort_flag_signal = Arc::clone(&abort_flag);
    let shutdown_notify_signal = Arc::clone(&shutdown_notify);

    tokio::spawn(async move {
        let _ = signal::ctrl_c().await;
        debug_log!("Received SIGINT");
        abort_flag_signal.store(true, Ordering::SeqCst);
        shutdown_notify_signal.notify_waiters();
    });

    // Create progress tracker (early, to time all phases)
    let tracker = Arc::new(ProgressTracker::new(config.tunnels));

    // Create agent pool
    let pool = Arc::new(AgentPool::new(config.clone()));
    debug_log!("Starting {} agents", config.tunnels);

    // Start agents with progress updates
    let start_result = pool.start(|connected, total| {
        eprint!("\rConnecting... {} / {} tunnels", format_count(connected as u64), format_count(total as u64));
        let _ = std::io::Write::flush(&mut std::io::stderr());
    }).await;
    if let Err(e) = start_result {
        eprintln!();
        return Err(e);
    }

    let healthy = pool.healthy_count();
    if healthy == 0 {
        eprintln!("\rConnecting... failed                    ");
        return Err(StrawsError::AllAgentsUnhealthy);
    }
    eprintln!("\rConnecting... {} / {} tunnels ready       ", format_count(healthy as u64), format_count(config.tunnels as u64));
    debug_log!("{} agents healthy", healthy);
    tracker.mark_connected();

    // Create job queue and scheduler
    let queue = JobQueue::new();
    let scheduler = Arc::new(JobScheduler::new(config.clone(), queue.clone()));

    // Schedule jobs based on direction with progress display
    let verify_note = if config.verify { " (with verification)" } else { "" };
    eprint!("\rScanning{}: 0 files found", verify_note);
    let _ = std::io::Write::flush(&mut std::io::stderr());

    // Spawn a task to update scanning/scheduling progress
    let progress_task = tokio::spawn({
        let scheduler = Arc::clone(&scheduler);
        let verify_note = verify_note.to_string();
        async move {
            let mut last_files = 0u64;
            let mut last_jobs = 0u64;
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                let files = scheduler.files_found();
                let jobs = scheduler.jobs_scheduled();
                if files != last_files || jobs != last_jobs {
                    if jobs > 0 {
                        eprint!("\rScanning{}: {} files found, {} jobs scheduled", verify_note, format_count(files), format_count(jobs));
                    } else {
                        eprint!("\rScanning{}: {} files found", verify_note, format_count(files));
                    }
                    let _ = std::io::Write::flush(&mut std::io::stderr());
                    last_files = files;
                    last_jobs = jobs;
                }
            }
        }
    });

    let schedule_result = match config.direction {
        Direction::Download => scheduler.schedule_downloads(&pool).await,
        Direction::Upload => scheduler.schedule_uploads(&pool).await,
    };

    progress_task.abort();
    schedule_result?;

    let final_jobs = scheduler.jobs_scheduled();
    if final_jobs > scheduler.total_files() {
        // More jobs than files means chunking occurred
        eprintln!("\rScanning{}: {} files, {} jobs scheduled       ", verify_note, format_count(scheduler.total_files()), format_count(final_jobs));
    } else {
        eprintln!("\rScanning{}: {} files found       ", verify_note, format_count(scheduler.total_files()));
    }
    tracker.mark_indexed();

    // Update tracker with totals and descriptions
    tracker.set_totals(scheduler.total_bytes(), scheduler.total_files());
    tracker.set_files_skipped(scheduler.files_skipped());
    tracker.set_verify_enabled(config.verify);

    // Set source/destination descriptions for display
    let remote_desc = format!("{}:{}", config.remote.user_host(), config.remote.path);
    let local_desc = config.local_paths.first()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| ".".to_string());
    let (source_desc, dest_desc) = match config.direction {
        Direction::Download => (remote_desc, local_desc),
        Direction::Upload => (local_desc, remote_desc),
    };
    tracker.set_descriptions(&source_desc, &dest_desc);

    if scheduler.total_files() == 0 {
        let skipped = scheduler.files_skipped();
        if skipped > 0 {
            println!("No files to transfer ({} already complete)", format_count(skipped));
        } else {
            println!("No files to transfer");
        }
        pool.shutdown().await;
        return Ok(());
    }

    debug_log!(
        "Scheduled {} files, {} bytes",
        scheduler.total_files(),
        scheduler.total_bytes()
    );

    // Start progress display
    let display = if !config.no_progress {
        let display = Arc::new(ProgressDisplay::new(
            Arc::clone(&tracker),
            Arc::clone(&pool),
            config.verbose,
        ));
        let display_clone = Arc::clone(&display);
        tokio::spawn(async move {
            display_clone.run().await;
        });
        Some(display)
    } else {
        None
    };

    // Track active files for finalization
    let active_files: Arc<parking_lot::Mutex<HashMap<u64, Arc<FileMeta>>>> =
        Arc::new(parking_lot::Mutex::new(HashMap::new()));

    // Flag to signal scheduling is complete (workers can exit when queue empty)
    let done_flag = Arc::new(AtomicBool::new(false));

    // Spawn worker tasks
    let mut worker_handles = Vec::new();
    let receiver = queue.receiver();
    let sender = queue.sender();

    for worker_id in 0..config.tunnels {
        let pool = Arc::clone(&pool);
        let tracker = Arc::clone(&tracker);
        let abort_flag = Arc::clone(&abort_flag);
        let done_flag = Arc::clone(&done_flag);
        let receiver = receiver.clone();
        let sender = sender.clone();
        let active_files = Arc::clone(&active_files);

        let handle = tokio::spawn(async move {
            worker_loop(
                worker_id,
                pool,
                tracker,
                abort_flag,
                done_flag,
                receiver,
                sender,
                active_files,
            )
            .await
        });
        worker_handles.push(handle);
    }

    // Signal that scheduling is done - workers can exit when queue drains
    done_flag.store(true, Ordering::SeqCst);
    drop(queue);
    drop(sender);

    // Wait for all workers
    for handle in worker_handles {
        let _ = handle.await;
    }
    tracker.mark_transfer_complete();

    // Stop progress display
    if let Some(ref display) = display {
        display.stop();
        // Render one final frame at 100% before showing summary
        display.render_final();
        display.print_summary();
    }

    // Cleanup on abort
    if abort_flag.load(Ordering::SeqCst) {
        debug_log!("Cleaning up after abort");
        // Clean up active file transfers
        let files = active_files.lock();
        for (_, meta) in files.iter() {
            if !meta.is_complete() {
                cleanup_temp(meta);
            }
        }
    }

    // Shutdown agents
    pool.shutdown().await;

    debug_log!("Transfer complete");

    if tracker.files_failed() > 0 {
        let failed_count = tracker.files_failed();
        let failed_files = tracker.failed_files();

        // Build a detailed error message
        let mut msg = format!(
            "{} file{} failed to transfer",
            failed_count,
            if failed_count == 1 { "" } else { "s" }
        );

        // If there are few failures, include them in the error message itself
        if failed_files.len() <= 5 {
            msg.push_str(":\n");
            for failed in &failed_files {
                msg.push_str(&format!("  - {}: {}\n", failed.remote_path, failed.error));
            }
        } else {
            msg.push_str(" (see list above)");
        }

        msg.push_str("\nRun the same command again to retry failed files.");

        return Err(StrawsError::TransferFailed(msg));
    }

    Ok(())
}

async fn worker_loop(
    worker_id: usize,
    pool: Arc<AgentPool>,
    tracker: Arc<ProgressTracker>,
    abort_flag: Arc<AtomicBool>,
    done_flag: Arc<AtomicBool>,
    receiver: crossbeam_channel::Receiver<Arc<Job>>,
    sender: crossbeam_channel::Sender<Arc<Job>>,
    active_files: Arc<parking_lot::Mutex<HashMap<u64, Arc<FileMeta>>>>,
) {
    debug_log!("Worker {} starting", worker_id);

    loop {
        // Check abort
        if abort_flag.load(Ordering::SeqCst) {
            debug_log!("Worker {} aborting", worker_id);
            break;
        }

        // Get next job with timeout to allow checking flags
        let job = match receiver.recv_timeout(std::time::Duration::from_millis(100)) {
            Ok(job) => job,
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                // If scheduling is done and queue is empty, we're finished
                if done_flag.load(Ordering::SeqCst) && receiver.is_empty() {
                    debug_log!("Worker {} done (queue empty)", worker_id);
                    break;
                }
                continue;
            }
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                debug_log!("Worker {} queue closed", worker_id);
                break;
            }
        };

        // Track file
        {
            let mut files = active_files.lock();
            let file_id = Arc::as_ptr(&job.file_meta) as u64;
            files.entry(file_id).or_insert_with(|| Arc::clone(&job.file_meta));
        }

        // Acquire agent
        let agent = loop {
            if abort_flag.load(Ordering::SeqCst) {
                break None;
            }

            if let Some(agent) = pool.acquire() {
                break Some(agent);
            }

            // No available agents, check if all unhealthy
            if pool.healthy_count() == 0 {
                debug_log!("Worker {} all agents unhealthy", worker_id);
                break None;
            }

            // Wait a bit and retry
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        };

        let agent = match agent {
            Some(a) => a,
            None => {
                debug_log!("Worker {} cannot acquire agent for job {}", worker_id, job.id);
                tracker.file_failed(
                    &job.file_meta.remote_path,
                    &job.file_meta.local_path.display().to_string(),
                    "All transfer agents became unavailable (connection failures)",
                    job.retries(),
                );
                continue;
            }
        };

        // Update tracker with job info
        tracker.set_agent_job(agent.id, Some(job.description()));

        // Execute job (tracker is updated incrementally during transfer)
        let result = match job.direction {
            Direction::Download => download_job(&job, &agent, &tracker).await,
            Direction::Upload => upload_job(&job, &agent, &tracker).await,
        };

        // Clear job info
        tracker.set_agent_job(agent.id, None);

        // Handle result (bytes already reported incrementally during transfer)
        match result {
            Ok(job_result) => {
                pool.release(&agent);

                // Check if file complete and not already finalized by another worker
                // The finalize_attempted() check ensures only one worker handles completion
                // even if multiple chunks complete nearly simultaneously
                if job.file_meta.is_complete() && !job.file_meta.finalize_attempted() {
                    // Finalize file (for downloads)
                    if job.direction == Direction::Download {
                        if let Err(e) = finalize_file(&job.file_meta) {
                            debug_log!("Failed to finalize {}: {}", job.file_meta.remote_path, e);
                            tracker.file_failed(
                                &job.file_meta.remote_path,
                                &job.file_meta.local_path.display().to_string(),
                                &format!("Failed to save file locally: {}", e),
                                job.retries(),
                            );
                        } else {
                            let name = job
                                .file_meta
                                .local_path
                                .file_name()
                                .map(|n| n.to_string_lossy().to_string())
                                .unwrap_or_default();
                            // For non-chunked files with verify, both MD5s match (already verified)
                            let md5 = job_result.md5.clone();
                            tracker.file_completed(&name, md5.clone(), md5, job.file_meta.mode, job.file_meta.mtime);
                        }
                    } else {
                        // For uploads, mark as finalized to prevent duplicate completion tracking
                        let _ = job.file_meta.ensure_finalized(|| Ok(()));
                        let name = job
                            .file_meta
                            .local_path
                            .file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_default();
                        // For non-chunked files with verify, both MD5s match (already verified)
                        let md5 = job_result.md5.clone();
                        tracker.file_completed(&name, md5.clone(), md5, job.file_meta.mode, job.file_meta.mtime);
                    }

                    // Remove from active files
                    let file_id = Arc::as_ptr(&job.file_meta) as u64;
                    active_files.lock().remove(&file_id);
                }
            }
            Err(e) => {
                debug_log!("Job {} failed: {}", job.id, e);
                let file_name = job.file_meta.local_path.file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| "unknown".to_string());

                // Check if agent failure
                if e.is_agent_failure() {
                    agent.mark_unhealthy(&e.to_string());
                    tracker.log_event(&format!("Agent error: {}", e), true);
                } else {
                    pool.release(&agent);
                }

                // Check retry
                if e.is_retryable() {
                    let retries = job.increment_retry();
                    if retries < MAX_RETRIES {
                        debug_log!("Requeuing job {} (retry {})", job.id, retries);
                        tracker.log_event(&format!("Retrying {}: {}", file_name, e), false);
                        // Requeue the job
                        if sender.try_send(job.clone()).is_err() {
                            debug_log!("Failed to requeue job, queue full or closed");
                            tracker.log_event(&format!("Failed {}: queue full", file_name), true);
                            tracker.file_failed(
                                &job.file_meta.remote_path,
                                &job.file_meta.local_path.display().to_string(),
                                &format!("Transfer queue full, could not retry after: {}", e),
                                retries,
                            );
                        }
                    } else {
                        debug_log!("Job {} max retries exceeded", job.id);
                        tracker.log_event(&format!("Failed {}: max retries", file_name), true);
                        tracker.file_failed(
                            &job.file_meta.remote_path,
                            &job.file_meta.local_path.display().to_string(),
                            &format!("Failed after {} retries: {}", MAX_RETRIES, e),
                            retries,
                        );
                    }
                } else {
                    tracker.log_event(&format!("Failed {}: {}", file_name, e), true);
                    tracker.file_failed(
                        &job.file_meta.remote_path,
                        &job.file_meta.local_path.display().to_string(),
                        &e.to_string(),
                        job.retries(),
                    );
                }
            }
        }
    }

    debug_log!("Worker {} finished", worker_id);
}
