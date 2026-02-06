use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use super::queue::JobQueue;
use super::types::{Direction, FileMeta, Job};
use crate::agent::protocol::{Request, StatInfo};
use crate::agent::AgentPool;
use crate::config::Config;
use crate::debug_log;
use crate::error::{Result, StrawsError};
use crate::transfer::io::compute_file_md5;

/// Schedules jobs based on file enumeration
pub struct JobScheduler {
    config: Config,
    queue: JobQueue,
    job_id_counter: AtomicU64,
    total_bytes: AtomicU64,
    total_files: AtomicU64,
    files_skipped: AtomicU64,
    /// Files found during scanning (for progress updates)
    files_found: AtomicU64,
    /// Jobs scheduled (for progress updates during job creation)
    jobs_scheduled: AtomicU64,
}

impl JobScheduler {
    pub fn new(config: Config, queue: JobQueue) -> Self {
        JobScheduler {
            config,
            queue,
            job_id_counter: AtomicU64::new(0),
            total_bytes: AtomicU64::new(0),
            total_files: AtomicU64::new(0),
            files_skipped: AtomicU64::new(0),
            files_found: AtomicU64::new(0),
            jobs_scheduled: AtomicU64::new(0),
        }
    }

    fn next_job_id(&self) -> u64 {
        self.job_id_counter.fetch_add(1, Ordering::Relaxed)
    }

    pub fn total_bytes(&self) -> u64 {
        self.total_bytes.load(Ordering::Relaxed)
    }

    pub fn total_files(&self) -> u64 {
        self.total_files.load(Ordering::Relaxed)
    }

    pub fn files_skipped(&self) -> u64 {
        self.files_skipped.load(Ordering::Relaxed)
    }

    /// Get count of files found during scanning (for progress display)
    pub fn files_found(&self) -> u64 {
        self.files_found.load(Ordering::Relaxed)
    }

    /// Get count of jobs scheduled (for progress display during job creation)
    pub fn jobs_scheduled(&self) -> u64 {
        self.jobs_scheduled.load(Ordering::Relaxed)
    }

    /// Enumerate files and create jobs for download
    pub async fn schedule_downloads(&self, pool: &AgentPool) -> Result<()> {
        let remote_paths: Vec<&str> = self.config.remote.path.split('\0').collect();
        let local_dest = &self.config.local_paths[0];

        // Ensure local destination exists and is a directory if multiple sources
        if remote_paths.len() > 1 || self.config.remote.path.contains('*') {
            tokio::fs::create_dir_all(local_dest)
                .await
                .map_err(|e| StrawsError::Io(e))?;
        }

        for remote_path in remote_paths {
            if pool.is_aborted() {
                break;
            }
            self.schedule_download_path(pool, remote_path, local_dest)
                .await?;
        }

        Ok(())
    }

    async fn schedule_download_path(
        &self,
        pool: &AgentPool,
        remote_path: &str,
        local_dest: &PathBuf,
    ) -> Result<()> {
        // Check if remote path is a file or directory using stat
        let agent = pool.acquire().ok_or(StrawsError::AllAgentsUnhealthy)?;

        let stat_result = agent.request(&Request::stat(remote_path)).await;
        pool.release(&agent);

        match stat_result {
            Ok(response) if response.is_success() => {
                let stat = response.parse_stat()?;
                let is_dir = (stat.mode & 0o170000) == 0o040000;

                if is_dir {
                    // Trailing slash on remote path means "copy contents" (rsync convention)
                    // server:foo/ → copy contents to dest
                    // server:foo  → copy foo directory to dest (preserving name)
                    let copy_contents_only = remote_path.ends_with('/');

                    let local_base = if copy_contents_only {
                        // Remote has trailing slash: copy contents directly to destination
                        if local_dest.to_string_lossy().ends_with('/') || local_dest.is_dir() {
                            local_dest.clone()
                        } else {
                            // Destination doesn't exist - create it as target
                            local_dest.clone()
                        }
                    } else {
                        // No trailing slash: preserve directory name
                        let dir_name = PathBuf::from(remote_path)
                            .file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_else(|| "download".to_string());

                        if local_dest.to_string_lossy().ends_with('/') {
                            // Explicit directory destination: use as-is with dir name
                            local_dest.join(&dir_name)
                        } else if local_dest.is_dir() {
                            // Existing directory: add the remote dir name
                            local_dest.join(&dir_name)
                        } else {
                            // Destination doesn't exist or is a file path: use as the target dir name
                            local_dest.clone()
                        }
                    };

                    self.enumerate_remote_directory(pool, remote_path, &local_base)
                        .await?;
                } else {
                    // Single file
                    let file_name = PathBuf::from(remote_path)
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| "download".to_string());

                    let local_path = if local_dest.is_dir()
                        || local_dest.to_string_lossy().ends_with('/')
                    {
                        local_dest.join(&file_name)
                    } else {
                        local_dest.clone()
                    };

                    // For single files, get MD5 if verify is enabled
                    let remote_md5 = if self.config.verify {
                        let md5_response = pool.acquire()
                            .ok_or(StrawsError::AllAgentsUnhealthy)?;
                        let result = md5_response.request(&Request::md5(remote_path, 0, stat.size)).await;
                        pool.release(&md5_response);
                        result.ok()
                            .filter(|r| r.is_success())
                            .map(|r| String::from_utf8_lossy(&r.data).to_string())
                    } else {
                        None
                    };

                    self.create_download_jobs(pool, remote_path, &local_path, stat, remote_md5.as_deref(), pool.agents().len())
                        .await?;
                }
            }
            Ok(response) => {
                let msg = response.error_message().unwrap_or_else(|| "Unknown error".to_string());
                if msg.contains("not found") {
                    return Err(StrawsError::NotFound(remote_path.to_string()));
                }
                return Err(StrawsError::Remote(msg));
            }
            Err(e) => return Err(e),
        }

        Ok(())
    }

    async fn enumerate_remote_directory(
        &self,
        pool: &AgentPool,
        remote_dir: &str,
        local_base: &PathBuf,
    ) -> Result<()> {
        // Use an agent to enumerate files via the FIND protocol command
        let agent = pool
            .acquire()
            .ok_or_else(|| StrawsError::Connection("No agents available for enumeration".into()))?;

        debug_log!("Enumerating remote directory via agent: {}", remote_dir);

        let tunnel_count = pool.agents().len();
        let remote_dir_path = PathBuf::from(remote_dir);
        let local_base = local_base.clone();

        // Collect entries to process (we need to release agent before creating jobs)
        // Request MD5 hashes if verify is enabled
        let mut entries = Vec::new();

        agent
            .find(remote_dir, self.config.verify, |entry| {
                if pool.is_aborted() {
                    return false;
                }
                self.files_found.fetch_add(1, Ordering::Relaxed);
                entries.push(entry);
                true
            })
            .await?;

        // Release agent back to pool
        pool.release(&agent);

        // Now create jobs for each entry
        for entry in entries {
            if pool.is_aborted() {
                break;
            }

            let remote_file_path = PathBuf::from(&entry.path);
            let relative = remote_file_path
                .strip_prefix(&remote_dir_path)
                .unwrap_or(&remote_file_path);
            let local_path = local_base.join(relative);

            let stat = StatInfo {
                size: entry.size,
                mode: entry.mode,
                mtime: entry.mtime,
            };
            self.create_download_jobs(pool, &entry.path, &local_path, stat, entry.md5.as_deref(), tunnel_count)
                .await?;
        }

        Ok(())
    }

    async fn create_download_jobs(
        &self,
        _pool: &AgentPool,
        remote_path: &str,
        local_path: &PathBuf,
        stat: StatInfo,
        remote_md5: Option<&str>,
        tunnel_count: usize,
    ) -> Result<()> {
        // Check if file already exists and is complete (skip if --force)
        if !self.config.force {
            if let Ok(local_meta) = tokio::fs::metadata(local_path).await {
                if local_meta.len() == stat.size {
                    // If verify is enabled and we have remote MD5, check it
                    if self.config.verify {
                        if let Some(expected_md5) = remote_md5 {
                            debug_log!("Verifying existing file: {}", local_path.display());

                            // Compute local MD5
                            let local_md5 = match compute_file_md5(local_path).await {
                                Ok(md5) => md5,
                                Err(e) => {
                                    debug_log!("Failed to compute local MD5: {}, will redownload", e);
                                    String::new()
                                }
                            };

                            if !local_md5.is_empty() && local_md5 == expected_md5 {
                                debug_log!(
                                    "Skipping verified file: {} (MD5 matches)",
                                    local_path.display()
                                );
                                self.files_skipped.fetch_add(1, Ordering::Relaxed);
                                return Ok(());
                            } else if !local_md5.is_empty() {
                                debug_log!(
                                    "MD5 mismatch for {}: local={} remote={}, will redownload",
                                    local_path.display(),
                                    local_md5,
                                    expected_md5
                                );
                            }
                            // Fall through to download the file
                        } else {
                            // No remote MD5 available, skip by size only
                            debug_log!(
                                "Skipping existing file: {} (size matches, no MD5 available)",
                                local_path.display()
                            );
                            self.files_skipped.fetch_add(1, Ordering::Relaxed);
                            return Ok(());
                        }
                    } else {
                        debug_log!(
                            "Skipping existing file: {} (size matches)",
                            local_path.display()
                        );
                        self.files_skipped.fetch_add(1, Ordering::Relaxed);
                        return Ok(());
                    }
                }
            }
        }

        self.total_bytes.fetch_add(stat.size, Ordering::Relaxed);
        self.total_files.fetch_add(1, Ordering::Relaxed);

        // Determine chunking - split large files into chunk_size pieces
        let chunk_size = self.config.chunk_size;
        let should_chunk = stat.size > chunk_size && tunnel_count > 1;

        if should_chunk {
            // Calculate chunk count based on configured chunk size
            let chunk_count = ((stat.size + chunk_size - 1) / chunk_size) as u32;

            let file_meta = Arc::new(FileMeta::new(
                remote_path.to_string(),
                local_path.clone(),
                stat.size,
                stat.mode,
                stat.mtime,
                chunk_count,
            ));

            // Create chunk jobs - each chunk is at most chunk_size bytes
            for i in 0..chunk_count {
                let offset = i as u64 * chunk_size;
                let length = std::cmp::min(chunk_size, stat.size.saturating_sub(offset));

                if length > 0 {
                    let job = Arc::new(Job::chunk(
                        self.next_job_id(),
                        Arc::clone(&file_meta),
                        Direction::Download,
                        i,
                        offset,
                        length,
                        self.config.verify,
                    ));
                    self.queue.push(job);
                    self.jobs_scheduled.fetch_add(1, Ordering::Relaxed);
                }
            }

            debug_log!(
                "Scheduled chunked download: {} ({} chunks)",
                remote_path,
                chunk_count
            );
        } else {
            // Single job for whole file
            let file_meta = Arc::new(FileMeta::new(
                remote_path.to_string(),
                local_path.clone(),
                stat.size,
                stat.mode,
                stat.mtime,
                1,
            ));

            let job = Arc::new(Job::whole_file(
                self.next_job_id(),
                file_meta,
                Direction::Download,
                self.config.verify,
            ));
            self.queue.push(job);
            self.jobs_scheduled.fetch_add(1, Ordering::Relaxed);

            debug_log!("Scheduled download: {}", remote_path);
        }

        Ok(())
    }

    /// Schedule upload jobs
    pub async fn schedule_uploads(&self, pool: &AgentPool) -> Result<()> {
        let remote_dest = &self.config.remote.path;

        for local_path in &self.config.local_paths {
            if pool.is_aborted() {
                break;
            }

            let metadata = tokio::fs::metadata(local_path)
                .await
                .map_err(|e| StrawsError::Io(e))?;

            if metadata.is_dir() {
                // Trailing slash on local path means "copy contents" (rsync convention)
                // foo/  → copy contents to dest
                // foo   → copy foo directory to dest (preserving name)
                let copy_contents_only = local_path.to_string_lossy().ends_with('/');

                let remote_base = if copy_contents_only {
                    // Local has trailing slash: copy contents directly to destination
                    remote_dest.clone()
                } else {
                    // No trailing slash: preserve directory name
                    let dir_name = local_path
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| "upload".to_string());

                    if remote_dest.ends_with('/') {
                        // Explicit directory destination: append dir name
                        format!("{}{}", remote_dest, dir_name)
                    } else {
                        // Use remote_dest as the target name
                        remote_dest.clone()
                    }
                };

                self.enumerate_local_directory(pool, local_path, &remote_base)
                    .await?;
            } else {
                let file_name = local_path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| "upload".to_string());

                let remote_path = if remote_dest.ends_with('/') {
                    format!("{}{}", remote_dest, file_name)
                } else {
                    remote_dest.clone()
                };

                self.create_upload_jobs(pool, local_path, &remote_path, &metadata)
                    .await?;
            }
        }

        Ok(())
    }

    async fn enumerate_local_directory(
        &self,
        pool: &AgentPool,
        local_dir: &PathBuf,
        remote_base: &str,
    ) -> Result<()> {
        let mut entries = tokio::fs::read_dir(local_dir)
            .await
            .map_err(|e| StrawsError::Io(e))?;

        while let Some(entry) = entries.next_entry().await.map_err(|e| StrawsError::Io(e))? {
            if pool.is_aborted() {
                break;
            }

            let path = entry.path();
            let metadata = entry.metadata().await.map_err(|e| StrawsError::Io(e))?;

            let relative = path
                .strip_prefix(local_dir)
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|_| {
                    path.file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default()
                });

            let remote_path = format!("{}/{}", remote_base.trim_end_matches('/'), relative);

            if metadata.is_dir() {
                // Recursively enumerate
                Box::pin(self.enumerate_local_directory(pool, &path, &remote_path)).await?;
            } else {
                self.create_upload_jobs(pool, &path, &remote_path, &metadata)
                    .await?;
            }
        }

        Ok(())
    }

    async fn create_upload_jobs(
        &self,
        pool: &AgentPool,
        local_path: &PathBuf,
        remote_path: &str,
        metadata: &std::fs::Metadata,
    ) -> Result<()> {
        let size = metadata.len();
        let tunnel_count = pool.agents().len();

        // Check if remote file already exists and is complete (skip if --force)
        if !self.config.force {
            let agent = pool.acquire().ok_or(StrawsError::AllAgentsUnhealthy)?;
            let stat_result = agent.request(&Request::stat(remote_path)).await;
            pool.release(&agent);

            if let Ok(response) = stat_result {
                if response.is_success() {
                    if let Ok(remote_stat) = response.parse_stat() {
                        if remote_stat.size == size {
                            // Sizes match - check MD5 if verify is enabled
                            if self.config.verify {
                                // Compute local MD5
                                let local_md5 = match compute_file_md5(local_path).await {
                                    Ok(md5) => md5,
                                    Err(e) => {
                                        debug_log!("Failed to compute local MD5: {}, will upload", e);
                                        String::new()
                                    }
                                };

                                if !local_md5.is_empty() {
                                    // Get remote MD5
                                    let agent = pool.acquire().ok_or(StrawsError::AllAgentsUnhealthy)?;
                                    let md5_result = agent.request(&Request::md5(remote_path, 0, size)).await;
                                    pool.release(&agent);

                                    if let Ok(md5_response) = md5_result {
                                        if md5_response.is_success() {
                                            let remote_md5 = String::from_utf8_lossy(&md5_response.data).to_string();
                                            if local_md5 == remote_md5 {
                                                debug_log!(
                                                    "Skipping verified upload: {} (MD5 matches)",
                                                    remote_path
                                                );
                                                self.files_skipped.fetch_add(1, Ordering::Relaxed);
                                                return Ok(());
                                            } else {
                                                debug_log!(
                                                    "MD5 mismatch for {}: local={} remote={}, will upload",
                                                    remote_path,
                                                    local_md5,
                                                    remote_md5
                                                );
                                            }
                                        }
                                    }
                                }
                            } else {
                                // No verify - skip by size only
                                debug_log!(
                                    "Skipping existing remote file: {} (size matches)",
                                    remote_path
                                );
                                self.files_skipped.fetch_add(1, Ordering::Relaxed);
                                return Ok(());
                            }
                        }
                    }
                }
            }
        }

        #[cfg(unix)]
        let mode = {
            use std::os::unix::fs::MetadataExt;
            metadata.mode()
        };
        #[cfg(not(unix))]
        let mode = 0o644u32; // Default permissions for files uploaded from Windows
        let mtime = metadata
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);

        self.total_bytes.fetch_add(size, Ordering::Relaxed);
        self.total_files.fetch_add(1, Ordering::Relaxed);

        // Determine chunking - split large files into chunk_size pieces
        let chunk_size = self.config.chunk_size;
        let should_chunk = size > chunk_size && tunnel_count > 1;

        if should_chunk {
            // Calculate chunk count based on configured chunk size
            let chunk_count = ((size + chunk_size - 1) / chunk_size) as u32;

            let file_meta = Arc::new(FileMeta::new(
                remote_path.to_string(),
                local_path.clone(),
                size,
                mode,
                mtime,
                chunk_count,
            ));

            // Create chunk jobs - each chunk is at most chunk_size bytes
            for i in 0..chunk_count {
                let offset = i as u64 * chunk_size;
                let length = std::cmp::min(chunk_size, size.saturating_sub(offset));

                if length > 0 {
                    let job = Arc::new(Job::chunk(
                        self.next_job_id(),
                        Arc::clone(&file_meta),
                        Direction::Upload,
                        i,
                        offset,
                        length,
                        self.config.verify,
                    ));
                    self.queue.push(job);
                    self.jobs_scheduled.fetch_add(1, Ordering::Relaxed);
                }
            }

            debug_log!(
                "Scheduled chunked upload: {} ({} chunks)",
                remote_path,
                chunk_count
            );
        } else {
            let file_meta = Arc::new(FileMeta::new(
                remote_path.to_string(),
                local_path.clone(),
                size,
                mode,
                mtime,
                1,
            ));

            let job = Arc::new(Job::whole_file(
                self.next_job_id(),
                file_meta,
                Direction::Upload,
                self.config.verify,
            ));
            self.queue.push(job);
            self.jobs_scheduled.fetch_add(1, Ordering::Relaxed);

            debug_log!("Scheduled upload: {}", remote_path);
        }

        Ok(())
    }
}
