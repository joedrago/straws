use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex as SyncMutex;
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::{Mutex as AsyncMutex, Semaphore};
use tokio::time::timeout;

use super::protocol::{Request, Response, ResponseHeader, ResponseStatus};
use super::python::agent_command;
use crate::config::Config;
use crate::debug_log;
use crate::error::{Result, StrawsError};

const IO_BUFFER_SIZE: usize = 65536; // 64KB
const STALL_TIMEOUT_SECS: u64 = 30;
const INITIAL_PING_TIMEOUT_SECS: u64 = 10;
const BATCH_SIZE: usize = 6;
const BATCH_DELAY_MS: u64 = 300;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentState {
    Starting,
    Ready,
    Busy,
    Unhealthy,
}

pub struct Agent {
    pub id: usize,
    state: SyncMutex<AgentState>,
    process: SyncMutex<Option<Child>>,
    stdin: AsyncMutex<Option<BufWriter<ChildStdin>>>,
    stdout: AsyncMutex<Option<BufReader<ChildStdout>>>,
    bytes_transferred: AtomicU64,
    last_activity: SyncMutex<Instant>,
    stderr_buffer: Arc<SyncMutex<String>>,
}

impl Agent {
    fn new(id: usize) -> Self {
        Agent {
            id,
            state: SyncMutex::new(AgentState::Starting),
            process: SyncMutex::new(None),
            stdin: AsyncMutex::new(None),
            stdout: AsyncMutex::new(None),
            bytes_transferred: AtomicU64::new(0),
            last_activity: SyncMutex::new(Instant::now()),
            stderr_buffer: Arc::new(SyncMutex::new(String::new())),
        }
    }

    pub fn state(&self) -> AgentState {
        *self.state.lock()
    }

    fn set_state(&self, state: AgentState) {
        *self.state.lock() = state;
    }

    pub fn bytes_transferred(&self) -> u64 {
        self.bytes_transferred.load(Ordering::Relaxed)
    }

    fn add_bytes(&self, bytes: u64) {
        self.bytes_transferred.fetch_add(bytes, Ordering::Relaxed);
        *self.last_activity.lock() = Instant::now();
    }

    pub fn is_available(&self) -> bool {
        self.state() == AgentState::Ready
    }

    pub fn mark_unhealthy(&self, reason: &str) {
        debug_log!("Agent {} marked unhealthy: {}", self.id, reason);
        self.set_state(AgentState::Unhealthy);
    }

    /// Send a request and read the full response (for small responses)
    pub async fn request(&self, req: &Request) -> Result<Response> {
        let encoded = req.encode();

        // Write request
        {
            let mut stdin_guard = self.stdin.lock().await;
            let stdin = stdin_guard
                .as_mut()
                .ok_or_else(|| StrawsError::Connection("Agent stdin not available".to_string()))?;

            timeout(Duration::from_secs(STALL_TIMEOUT_SECS), stdin.write_all(&encoded))
                .await
                .map_err(|_| StrawsError::Stall(STALL_TIMEOUT_SECS))?
                .map_err(|e| StrawsError::Io(e))?;

            timeout(Duration::from_secs(STALL_TIMEOUT_SECS), stdin.flush())
                .await
                .map_err(|_| StrawsError::Stall(STALL_TIMEOUT_SECS))?
                .map_err(|e| StrawsError::Io(e))?;
        }

        // Read response header
        let mut header_buf = [0u8; ResponseHeader::SIZE];
        {
            let mut stdout_guard = self.stdout.lock().await;
            let stdout = stdout_guard
                .as_mut()
                .ok_or_else(|| StrawsError::Connection("Agent stdout not available".to_string()))?;

            timeout(
                Duration::from_secs(STALL_TIMEOUT_SECS),
                stdout.read_exact(&mut header_buf),
            )
            .await
            .map_err(|_| StrawsError::Stall(STALL_TIMEOUT_SECS))?
            .map_err(|e| StrawsError::Io(e))?;
        }

        let header = ResponseHeader::decode(&header_buf)?;

        // Read response data
        let mut data = vec![0u8; header.data_len as usize];
        if header.data_len > 0 {
            let mut stdout_guard = self.stdout.lock().await;
            let stdout = stdout_guard.as_mut().ok_or_else(|| {
                StrawsError::Connection("Agent stdout not available".to_string())
            })?;

            timeout(
                Duration::from_secs(STALL_TIMEOUT_SECS),
                stdout.read_exact(&mut data),
            )
            .await
            .map_err(|_| StrawsError::Stall(STALL_TIMEOUT_SECS))?
            .map_err(|e| StrawsError::Io(e))?;

            self.add_bytes(header.data_len);
        }

        Ok(Response {
            status: header.status,
            data,
        })
    }

    /// Send a read request and stream data to a writer
    pub async fn stream_read<W: tokio::io::AsyncWrite + Unpin>(
        &self,
        path: &str,
        offset: u64,
        length: u64,
        writer: &mut W,
    ) -> Result<u64> {
        let req = Request::read(path, offset, length);
        let encoded = req.encode();

        // Acquire both locks for the duration of the operation
        let mut stdin_guard = self.stdin.lock().await;
        let mut stdout_guard = self.stdout.lock().await;

        let stdin = stdin_guard
            .as_mut()
            .ok_or_else(|| StrawsError::Connection("Agent stdin not available".to_string()))?;

        // Write request
        timeout(Duration::from_secs(STALL_TIMEOUT_SECS), stdin.write_all(&encoded))
            .await
            .map_err(|_| StrawsError::Stall(STALL_TIMEOUT_SECS))?
            .map_err(|e| StrawsError::Io(e))?;

        timeout(Duration::from_secs(STALL_TIMEOUT_SECS), stdin.flush())
            .await
            .map_err(|_| StrawsError::Stall(STALL_TIMEOUT_SECS))?
            .map_err(|e| StrawsError::Io(e))?;

        let stdout = stdout_guard
            .as_mut()
            .ok_or_else(|| StrawsError::Connection("Agent stdout not available".to_string()))?;

        // Read response header
        let mut header_buf = [0u8; ResponseHeader::SIZE];
        timeout(
            Duration::from_secs(STALL_TIMEOUT_SECS),
            stdout.read_exact(&mut header_buf),
        )
        .await
        .map_err(|_| StrawsError::Stall(STALL_TIMEOUT_SECS))?
        .map_err(|e| StrawsError::Io(e))?;

        let header = ResponseHeader::decode(&header_buf)?;

        if header.status == ResponseStatus::Error {
            // Read error message
            let mut error_data = vec![0u8; header.data_len as usize];
            if header.data_len > 0 {
                timeout(
                    Duration::from_secs(STALL_TIMEOUT_SECS),
                    stdout.read_exact(&mut error_data),
                )
                .await
                .map_err(|_| StrawsError::Stall(STALL_TIMEOUT_SECS))?
                .map_err(|e| StrawsError::Io(e))?;
            }
            let msg = String::from_utf8_lossy(&error_data).to_string();
            return Err(StrawsError::Remote(msg));
        }

        // Stream data to writer in chunks
        let mut remaining = header.data_len;
        let mut buf = vec![0u8; IO_BUFFER_SIZE];
        let mut total_written = 0u64;

        while remaining > 0 {
            let to_read = std::cmp::min(remaining as usize, buf.len());

            timeout(
                Duration::from_secs(STALL_TIMEOUT_SECS),
                stdout.read_exact(&mut buf[..to_read]),
            )
            .await
            .map_err(|_| StrawsError::Stall(STALL_TIMEOUT_SECS))?
            .map_err(|e| StrawsError::Io(e))?;

            writer
                .write_all(&buf[..to_read])
                .await
                .map_err(|e| StrawsError::Io(e))?;

            remaining -= to_read as u64;
            total_written += to_read as u64;
            self.add_bytes(to_read as u64);
        }

        writer.flush().await.map_err(|e| StrawsError::Io(e))?;

        Ok(total_written)
    }

    /// Send a find request and stream results via callback
    /// Callback receives each FindEntry; return false to stop enumeration
    pub async fn find<F>(&self, path: &str, mut callback: F) -> Result<()>
    where
        F: FnMut(super::protocol::FindEntry) -> bool,
    {
        use super::protocol::FindEntry;
        use byteorder::{BigEndian, ByteOrder};

        let req = Request::find(path);
        let encoded = req.encode();

        let mut stdin_guard = self.stdin.lock().await;
        let mut stdout_guard = self.stdout.lock().await;

        let stdin = stdin_guard
            .as_mut()
            .ok_or_else(|| StrawsError::Connection("Agent stdin not available".to_string()))?;

        // Send request
        timeout(Duration::from_secs(STALL_TIMEOUT_SECS), stdin.write_all(&encoded))
            .await
            .map_err(|_| StrawsError::Stall(STALL_TIMEOUT_SECS))?
            .map_err(|e| StrawsError::Io(e))?;

        timeout(Duration::from_secs(STALL_TIMEOUT_SECS), stdin.flush())
            .await
            .map_err(|_| StrawsError::Stall(STALL_TIMEOUT_SECS))?
            .map_err(|e| StrawsError::Io(e))?;

        let stdout = stdout_guard
            .as_mut()
            .ok_or_else(|| StrawsError::Connection("Agent stdout not available".to_string()))?;

        // Read status byte
        let mut status_buf = [0u8; 1];
        timeout(
            Duration::from_secs(STALL_TIMEOUT_SECS),
            stdout.read_exact(&mut status_buf),
        )
        .await
        .map_err(|_| StrawsError::Stall(STALL_TIMEOUT_SECS))?
        .map_err(|e| StrawsError::Io(e))?;

        if status_buf[0] != 0 {
            // Error - read data_len and error message
            let mut len_buf = [0u8; 8];
            timeout(
                Duration::from_secs(STALL_TIMEOUT_SECS),
                stdout.read_exact(&mut len_buf),
            )
            .await
            .map_err(|_| StrawsError::Stall(STALL_TIMEOUT_SECS))?
            .map_err(|e| StrawsError::Io(e))?;

            let error_len = BigEndian::read_u64(&len_buf) as usize;
            let mut error_data = vec![0u8; error_len];
            if error_len > 0 {
                timeout(
                    Duration::from_secs(STALL_TIMEOUT_SECS),
                    stdout.read_exact(&mut error_data),
                )
                .await
                .map_err(|_| StrawsError::Stall(STALL_TIMEOUT_SECS))?
                .map_err(|e| StrawsError::Io(e))?;
            }
            return Err(StrawsError::Remote(
                String::from_utf8_lossy(&error_data).to_string(),
            ));
        }

        // Stream entries until we get path_len=0
        loop {
            // Read path_len
            let mut path_len_buf = [0u8; 2];
            timeout(
                Duration::from_secs(STALL_TIMEOUT_SECS),
                stdout.read_exact(&mut path_len_buf),
            )
            .await
            .map_err(|_| StrawsError::Stall(STALL_TIMEOUT_SECS))?
            .map_err(|e| StrawsError::Io(e))?;

            let path_len = BigEndian::read_u16(&path_len_buf) as usize;

            // End marker
            if path_len == 0 {
                break;
            }

            // Read path + stats
            let entry_size = path_len + 8 + 4 + 8; // path + size + mode + mtime
            let mut entry_buf = vec![0u8; entry_size];
            timeout(
                Duration::from_secs(STALL_TIMEOUT_SECS),
                stdout.read_exact(&mut entry_buf),
            )
            .await
            .map_err(|_| StrawsError::Stall(STALL_TIMEOUT_SECS))?
            .map_err(|e| StrawsError::Io(e))?;

            // Parse entry
            let entry_path = String::from_utf8_lossy(&entry_buf[..path_len]).to_string();
            let stats = &entry_buf[path_len..];
            let size = BigEndian::read_u64(&stats[0..8]);
            let mode = BigEndian::read_u32(&stats[8..12]);
            let mtime = BigEndian::read_u64(&stats[12..20]);

            let entry = FindEntry {
                path: entry_path,
                size,
                mode,
                mtime,
            };

            if !callback(entry) {
                break;
            }
        }

        Ok(())
    }

    async fn kill(&self) {
        let mut process_guard = self.process.lock();
        if let Some(ref mut process) = *process_guard {
            let _ = process.kill().await;
        }
    }

    async fn set_io(&self, stdin: BufWriter<ChildStdin>, stdout: BufReader<ChildStdout>) {
        *self.stdin.lock().await = Some(stdin);
        *self.stdout.lock().await = Some(stdout);
    }
}

pub struct AgentPool {
    agents: Vec<Arc<Agent>>,
    config: Config,
    abort_flag: AtomicBool,
}

impl AgentPool {
    pub fn new(config: Config) -> Self {
        let agents = (0..config.tunnels)
            .map(|id| Arc::new(Agent::new(id)))
            .collect();

        AgentPool {
            agents,
            config,
            abort_flag: AtomicBool::new(false),
        }
    }

    /// Start all agents with batched spawning
    pub async fn start(&self) -> Result<()> {
        let semaphore = Arc::new(Semaphore::new(BATCH_SIZE));
        let mut handles = Vec::new();

        for agent in &self.agents {
            let permit = semaphore.clone().acquire_owned().await.unwrap();
            let agent = Arc::clone(agent);
            let config = self.config.clone();

            let handle = tokio::spawn(async move {
                let result = Self::start_agent(&agent, &config).await;
                drop(permit);
                if result.is_err() {
                    agent.mark_unhealthy("Failed to start");
                }
                result
            });

            handles.push(handle);

            // Small delay between batch starts
            if handles.len() % BATCH_SIZE == 0 {
                tokio::time::sleep(Duration::from_millis(BATCH_DELAY_MS)).await;
            }
        }

        // Wait for all agents to start
        let mut success_count = 0;
        for handle in handles {
            if let Ok(Ok(())) = handle.await {
                success_count += 1;
            }
        }

        if success_count == 0 {
            return Err(StrawsError::AllAgentsUnhealthy);
        }

        debug_log!("Started {}/{} agents", success_count, self.agents.len());
        Ok(())
    }

    async fn start_agent(agent: &Agent, config: &Config) -> Result<()> {
        let mut cmd = Command::new("ssh");

        // Basic SSH options
        cmd.arg("-T") // No PTY
            .arg("-o").arg("BatchMode=yes")
            .arg("-o").arg("StrictHostKeyChecking=accept-new")
            .arg("-o").arg("ServerAliveInterval=60")
            .arg("-o").arg("ServerAliveCountMax=3")
            .arg("-p").arg(config.port.to_string());

        // Compression
        if config.compress {
            cmd.arg("-C");
        }

        // Identity file
        if let Some(ref identity) = config.identity {
            cmd.arg("-i").arg(identity);
        }

        // Host
        cmd.arg(config.remote.user_host());

        // Python agent command
        cmd.arg(agent_command());

        // Setup I/O
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        debug_log!("Starting agent {}: {:?}", agent.id, cmd);

        let mut process = cmd.spawn().map_err(|e| {
            StrawsError::Connection(format!("Failed to spawn SSH: {}", e))
        })?;

        let stdin = process.stdin.take().ok_or_else(|| {
            StrawsError::Connection("Failed to get stdin".to_string())
        })?;
        let stdout = process.stdout.take().ok_or_else(|| {
            StrawsError::Connection("Failed to get stdout".to_string())
        })?;

        // Setup stderr reader for diagnostics
        if let Some(stderr) = process.stderr.take() {
            let agent_id = agent.id;
            let stderr_buffer = agent.stderr_buffer.clone();
            tokio::spawn(async move {
                let mut reader = BufReader::new(stderr);
                let mut buf = [0u8; 1024];
                loop {
                    match reader.read(&mut buf).await {
                        Ok(0) => break,
                        Ok(n) => {
                            let text = String::from_utf8_lossy(&buf[..n]);
                            debug_log!("Agent {} stderr: {}", agent_id, text.trim());
                            let mut buffer = stderr_buffer.lock();
                            buffer.push_str(&text);
                            // Keep last 5000 chars
                            if buffer.len() > 5000 {
                                let start = buffer.len() - 5000;
                                *buffer = buffer[start..].to_string();
                            }
                        }
                        Err(_) => break,
                    }
                }
            });
        }

        *agent.process.lock() = Some(process);
        agent.set_io(
            BufWriter::with_capacity(IO_BUFFER_SIZE, stdin),
            BufReader::with_capacity(IO_BUFFER_SIZE, stdout),
        ).await;

        // Wait a bit for SSH to connect
        tokio::time::sleep(Duration::from_millis(300)).await;

        // Ping the agent to verify it's working
        let ping_result = timeout(
            Duration::from_secs(INITIAL_PING_TIMEOUT_SECS),
            agent.request(&Request::read("/dev/null", 0, 0)),
        )
        .await;

        match ping_result {
            Ok(Ok(_)) => {
                agent.set_state(AgentState::Ready);
                debug_log!("Agent {} ready", agent.id);
                Ok(())
            }
            Ok(Err(e)) => {
                agent.mark_unhealthy(&format!("Ping failed: {}", e));
                Err(e)
            }
            Err(_) => {
                agent.mark_unhealthy("Ping timeout");
                Err(StrawsError::Stall(INITIAL_PING_TIMEOUT_SECS))
            }
        }
    }

    /// Acquire an available agent
    pub fn acquire(&self) -> Option<Arc<Agent>> {
        for agent in &self.agents {
            let mut state = agent.state.lock();
            if *state == AgentState::Ready {
                *state = AgentState::Busy;
                return Some(Arc::clone(agent));
            }
        }
        None
    }

    /// Release an agent back to the pool
    pub fn release(&self, agent: &Agent) {
        if agent.state() != AgentState::Unhealthy {
            agent.set_state(AgentState::Ready);
        }
    }

    /// Get count of healthy agents
    pub fn healthy_count(&self) -> usize {
        self.agents
            .iter()
            .filter(|a| a.state() != AgentState::Unhealthy)
            .count()
    }

    /// Check if any healthy agents are available
    pub fn has_available(&self) -> bool {
        self.agents.iter().any(|a| a.is_available())
    }

    /// Get all agents
    pub fn agents(&self) -> &[Arc<Agent>] {
        &self.agents
    }

    /// Set abort flag
    pub fn abort(&self) {
        self.abort_flag.store(true, Ordering::SeqCst);
    }

    /// Check if aborted
    pub fn is_aborted(&self) -> bool {
        self.abort_flag.load(Ordering::SeqCst)
    }

    /// Shutdown all agents
    pub async fn shutdown(&self) {
        debug_log!("Shutting down agent pool");

        for agent in &self.agents {
            agent.set_state(AgentState::Unhealthy);
            agent.kill().await;
        }
    }
}
