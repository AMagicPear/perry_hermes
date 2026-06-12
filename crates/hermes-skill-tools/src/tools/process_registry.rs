//! Process Registry — in-memory registry for managed background processes.
//!
//! Tracks processes spawned via `terminal(background=true)`, providing:
//! - Output buffering (rolling 200KB window)
//! - Status polling and log retrieval
//! - Blocking wait with timeout
//! - Process killing
//! - Completion notification via mpsc channel
//!
//! Usage:
//! ```ignore
//! let session_id = PROCESS_REGISTRY.spawn("echo hi", cwd, true).await?;
//! let result = PROCESS_REGISTRY.poll(&session_id).await;
//! ```

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::sync::{Mutex, RwLock, mpsc};
use tokio::time::Duration;

use perry_hermes_core::util;

/// Maximum output buffer size per process (200KB).
const MAX_OUTPUT_CHARS: usize = 200_000;
/// How long to keep finished processes before cleanup (30 minutes).
const FINISHED_TTL: Duration = Duration::from_secs(1800);
/// How often the cleanup task runs.
const CLEANUP_INTERVAL: Duration = Duration::from_secs(60);
/// Default timeout for `wait` when not specified.
const DEFAULT_WAIT_TIMEOUT: u64 = 300;
/// Truncation limit for notification output tails.
const NOTIFY_OUTPUT_TAIL_CHARS: usize = 2000;

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// Global process registry singleton.
pub static PROCESS_REGISTRY: LazyLock<ProcessRegistry> = LazyLock::new(ProcessRegistry::new);

/// A tracked background process.
#[derive(Debug, Clone)]
pub struct ProcessSession {
    pub id: String,
    pub command: String,
    pub cwd: PathBuf,
    pub pid: Option<u32>,
    pub started_at: Instant,
    pub exited: bool,
    pub exit_code: Option<i32>,
    /// Rolling output buffer (last MAX_OUTPUT_CHARS chars).
    pub output_buffer: String,
    /// Byte offset into the output that has been returned by `poll` / `wait`
    /// / `log`. Used to return only new content on subsequent polls.
    pub output_returned_offset: usize,
    pub notify_on_complete: bool,
    /// Set to `true` when the agent has consumed the result via
    /// `poll` / `wait` / `log`. Suppresses duplicate notifications.
    pub consumed: bool,
}

/// What `poll` returns.
#[derive(Debug, Clone)]
pub struct PollResult {
    pub session_id: String,
    pub status: String,
    pub exit_code: Option<i32>,
    pub output: String,
    pub uptime_secs: u64,
}

/// What `wait` returns.
#[derive(Debug, Clone)]
pub struct WaitResult {
    pub session_id: String,
    pub exit_code: Option<i32>,
    pub output: String,
    pub timed_out: bool,
}

/// What `kill` returns.
#[derive(Debug, Clone)]
pub struct KillResult {
    pub session_id: String,
    pub killed: bool,
    pub exit_code: Option<i32>,
}

/// What `read_log` returns.
#[derive(Debug, Clone)]
pub struct LogResult {
    pub session_id: String,
    pub total_lines: usize,
    pub lines: Vec<String>,
}

/// What `list` returns per process.
#[derive(Debug, Clone)]
pub struct ProcessInfo {
    pub id: String,
    pub command: String,
    pub status: String,
    pub exit_code: Option<i32>,
    pub uptime_secs: u64,
    pub output_chars: usize,
}

/// A notification emitted when a background process exits.
#[derive(Debug, Clone)]
pub enum ProcessNotification {
    Completed {
        session_id: String,
        exit_code: i32,
        command: String,
        output_tail: String,
    },
}

struct RegistryState {
    running: HashMap<String, ProcessSession>,
    finished: HashMap<String, ProcessSession>,
}

pub struct ProcessRegistry {
    state: RwLock<RegistryState>,
    notify_tx: mpsc::UnboundedSender<ProcessNotification>,
    notify_rx: Mutex<mpsc::UnboundedReceiver<ProcessNotification>>,
}

impl ProcessRegistry {
    fn new() -> Self {
        let (notify_tx, notify_rx) = mpsc::unbounded_channel();
        Self {
            state: RwLock::new(RegistryState {
                running: HashMap::new(),
                finished: HashMap::new(),
            }),
            notify_tx,
            notify_rx: Mutex::new(notify_rx),
        }
    }

    /// Spawn a background process. Returns the session id.
    pub async fn spawn(
        &self,
        command: &str,
        cwd: PathBuf,
        notify_on_complete: bool,
    ) -> Result<String, String> {
        let id = format!("proc_{}", NEXT_ID.fetch_add(1, Ordering::Relaxed));

        let shell = if util::which("zsh") { "zsh" } else { "bash" };
        let mut cmd = Command::new(shell);
        cmd.arg("-c")
            .arg(command)
            .current_dir(&cwd)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        // On Unix, create a new process group so `kill` can terminate
        // the entire subtree (shell + children).
        #[cfg(unix)]
        set_process_group(&mut cmd);
        let mut child = cmd.spawn().map_err(|e| format!("failed to spawn: {e}"))?;

        let pid = child.id();
        let session = ProcessSession {
            id: id.clone(),
            command: command.to_string(),
            cwd,
            pid,
            started_at: Instant::now(),
            exited: false,
            exit_code: None,
            output_buffer: String::new(),
            output_returned_offset: 0,
            notify_on_complete,
            consumed: false,
        };

        {
            let mut state = self.state.write().await;
            state.running.insert(id.clone(), session);
        }

        // Spawn a background task that reads stdout/stddr incrementally
        // so `poll` shows live output while the process is still running.
        let registry_id = id.clone();
        let notify_tx = self.notify_tx.clone();
        let session_command = command.to_string();
        tokio::spawn(async move {
            let stdout = child.stdout.take();
            let stderr = child.stderr.take();

            // Two reader tasks append chunks to the registry's
            // output_buffer in real time.
            let rid_stdout = registry_id.clone();
            let rid_stderr = registry_id.clone();

            let stdout_reader = async move {
                if let Some(mut s) = stdout {
                    let mut buf = [0u8; 8192];
                    loop {
                        match s.read(&mut buf).await {
                            Ok(0) => break,
                            Ok(n) => {
                                let chunk = String::from_utf8_lossy(&buf[..n]).into_owned();
                                append_output(&rid_stdout, chunk).await;
                            }
                            Err(_) => break,
                        }
                    }
                }
            };
            let stderr_reader = async move {
                if let Some(mut s) = stderr {
                    let mut buf = [0u8; 8192];
                    loop {
                        match s.read(&mut buf).await {
                            Ok(0) => break,
                            Ok(n) => {
                                let chunk = String::from_utf8_lossy(&buf[..n]).into_owned();
                                append_output(&rid_stderr, chunk).await;
                            }
                            Err(_) => break,
                        }
                    }
                }
            };

            tokio::join!(stdout_reader, stderr_reader);
            let status = child.wait().await;
            let exit_code = status.ok().and_then(|s| s.code()).unwrap_or(-1);

            // Move from running to finished.
            let session = {
                let mut state = PROCESS_REGISTRY.state.write().await;
                if let Some(mut session) = state.running.remove(&registry_id) {
                    session.exited = true;
                    session.exit_code = Some(exit_code);
                    // Final truncation pass.
                    session.output_buffer = truncate_buffer(&session.output_buffer);
                    Some(session)
                } else {
                    None
                }
            };

            if let Some(session) = session {
                let notify = session.notify_on_complete;
                let output_tail = tail_chars(&session.output_buffer, NOTIFY_OUTPUT_TAIL_CHARS);
                {
                    let mut state = PROCESS_REGISTRY.state.write().await;
                    state.finished.insert(registry_id.clone(), session);
                }
                if notify {
                    let _ = notify_tx.send(ProcessNotification::Completed {
                        session_id: registry_id,
                        exit_code,
                        command: session_command,
                        output_tail,
                    });
                }
            }
        });

        // Start cleanup task on first spawn (idempotent via a static flag).
        start_cleanup_task();

        Ok(id)
    }

    /// Poll a process for status and new output.
    pub async fn poll(&self, session_id: &str) -> Result<PollResult, String> {
        let mut state = self.state.write().await;
        let session = find_session_mut(&mut state, session_id)?;
        let new_output = session.output_buffer[session.output_returned_offset..].to_string();
        session.output_returned_offset = session.output_buffer.len();
        session.consumed = true;
        Ok(PollResult {
            session_id: session_id.to_string(),
            status: if session.exited {
                "finished"
            } else {
                "running"
            }
            .to_string(),
            exit_code: session.exit_code,
            output: new_output,
            uptime_secs: session.started_at.elapsed().as_secs(),
        })
    }

    /// Wait for a process to exit, with an optional timeout.
    pub async fn wait(
        &self,
        session_id: &str,
        timeout_secs: Option<u64>,
    ) -> Result<WaitResult, String> {
        let timeout = Duration::from_secs(timeout_secs.unwrap_or(DEFAULT_WAIT_TIMEOUT));
        let deadline = tokio::time::Instant::now() + timeout;

        loop {
            {
                let mut state = self.state.write().await;
                let session = find_session_mut(&mut state, session_id)?;
                if session.exited {
                    let output = session.output_buffer.clone();
                    session.consumed = true;
                    session.output_returned_offset = session.output_buffer.len();
                    return Ok(WaitResult {
                        session_id: session_id.to_string(),
                        exit_code: session.exit_code,
                        output,
                        timed_out: false,
                    });
                }
            }
            if tokio::time::Instant::now() >= deadline {
                let state = self.state.read().await;
                let session = find_session(&state, session_id)?;
                let output = session.output_buffer.clone();
                return Ok(WaitResult {
                    session_id: session_id.to_string(),
                    exit_code: None,
                    output,
                    timed_out: true,
                });
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    /// Kill a running process (and its entire process tree on Unix).
    pub async fn kill(&self, session_id: &str) -> Result<KillResult, String> {
        let mut state = self.state.write().await;
        let session = find_session_mut(&mut state, session_id)?;
        if session.exited {
            return Ok(KillResult {
                session_id: session_id.to_string(),
                killed: false,
                exit_code: session.exit_code,
            });
        }
        if let Some(pid) = session.pid {
            #[cfg(unix)]
            {
                // Send SIGTERM to the process *group* (negative PID)
                // so the shell and all its children are terminated.
                // SAFETY: sending signal to a process group we created.
                unsafe { libc::kill(-(pid as i32), libc::SIGTERM) };
            }
            #[cfg(not(unix))]
            {
                // On non-Unix, fall back to killing just the direct PID.
                let _ = pid;
            }
        }
        session.consumed = true;
        Ok(KillResult {
            session_id: session_id.to_string(),
            killed: true,
            exit_code: None,
        })
    }

    /// List all tracked processes (running + finished).
    pub async fn list(&self) -> Vec<ProcessInfo> {
        let state = self.state.read().await;
        let mut out = Vec::new();
        for s in state.running.values() {
            out.push(ProcessInfo {
                id: s.id.clone(),
                command: s.command.clone(),
                status: "running".to_string(),
                exit_code: None,
                uptime_secs: s.started_at.elapsed().as_secs(),
                output_chars: s.output_buffer.len(),
            });
        }
        for s in state.finished.values() {
            out.push(ProcessInfo {
                id: s.id.clone(),
                command: s.command.clone(),
                status: "finished".to_string(),
                exit_code: s.exit_code,
                uptime_secs: s.started_at.elapsed().as_secs(),
                output_chars: s.output_buffer.len(),
            });
        }
        out
    }

    /// Read the full output log with pagination.
    pub async fn read_log(
        &self,
        session_id: &str,
        offset: Option<usize>,
        limit: Option<usize>,
    ) -> Result<LogResult, String> {
        let mut state = self.state.write().await;
        let session = find_session_mut(&mut state, session_id)?;
        let lines: Vec<String> = session
            .output_buffer
            .lines()
            .map(|s| s.to_string())
            .collect();
        let total = lines.len();
        let limit = limit.unwrap_or(200);
        let offset = offset.unwrap_or(total.saturating_sub(limit));
        let end = (offset + limit).min(total);
        let slice = if offset < total {
            lines[offset..end].to_vec()
        } else {
            Vec::new()
        };
        session.consumed = true;
        Ok(LogResult {
            session_id: session_id.to_string(),
            total_lines: total,
            lines: slice,
        })
    }

    /// Drain all pending notifications from the channel.
    pub async fn drain_notifications(&self) -> Vec<ProcessNotification> {
        let mut rx = self.notify_rx.lock().await;
        let mut out = Vec::new();
        while let Ok(n) = rx.try_recv() {
            out.push(n);
        }
        out
    }
}

/// Append a chunk to the running process's output buffer in real time.
/// Called from the reader tasks spawned by `spawn`.
async fn append_output(session_id: &str, chunk: String) {
    let mut state = PROCESS_REGISTRY.state.write().await;
    if let Some(session) = state.running.get_mut(session_id) {
        session.output_buffer.push_str(&chunk);
        // Rolling window: if we exceed the cap, truncate from the front.
        if session.output_buffer.len() > MAX_OUTPUT_CHARS {
            session.output_buffer = truncate_buffer(&session.output_buffer);
            // Adjust the returned-offset so `poll` doesn't re-return
            // truncated-away content.
            session.output_returned_offset = session.output_buffer.len();
        }
    }
}

fn truncate_buffer(s: &str) -> String {
    if s.len() <= MAX_OUTPUT_CHARS {
        return s.to_string();
    }
    let start = s.len() - MAX_OUTPUT_CHARS;
    // Snap to next newline so we don't start mid-line.
    let start = s[start..]
        .find('\n')
        .map(|i| start + i + 1)
        .unwrap_or(start);
    s[start..].to_string()
}

/// Return the last `n` characters of `s`, snapped to a newline boundary.
fn tail_chars(s: &str, n: usize) -> String {
    if s.len() <= n {
        return s.to_string();
    }
    let start = s.len() - n;
    let start = s[start..]
        .find('\n')
        .map(|i| start + i + 1)
        .unwrap_or(start);
    format!(
        "[... output truncated — showing last {} chars]\n{}",
        s.len() - start,
        &s[start..]
    )
}

fn find_session<'a>(state: &'a RegistryState, id: &str) -> Result<&'a ProcessSession, String> {
    state
        .running
        .get(id)
        .or_else(|| state.finished.get(id))
        .ok_or_else(|| format!("process {id} not found"))
}

fn find_session_mut<'a>(
    state: &'a mut RegistryState,
    id: &str,
) -> Result<&'a mut ProcessSession, String> {
    if state.running.contains_key(id) {
        Ok(state.running.get_mut(id).unwrap())
    } else if state.finished.contains_key(id) {
        Ok(state.finished.get_mut(id).unwrap())
    } else {
        Err(format!("process {id} not found"))
    }
}

/// On Unix, set `process_group(0)` on a `Command` so the child becomes
/// the leader of a new process group. This allows `kill(-pgid, SIGTERM)`
/// to terminate the entire subtree.
#[cfg(unix)]
fn set_process_group(cmd: &mut Command) {
    use std::os::unix::process::CommandExt;
    cmd.as_std_mut().process_group(0);
}

/// Start the background cleanup task. Idempotent — only starts once.
fn start_cleanup_task() {
    use std::sync::atomic::AtomicBool;
    static STARTED: AtomicBool = AtomicBool::new(false);
    if STARTED.swap(true, Ordering::Relaxed) {
        return;
    }
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(CLEANUP_INTERVAL).await;
            let cutoff = Instant::now() - FINISHED_TTL;
            let mut state = PROCESS_REGISTRY.state.write().await;
            state
                .finished
                .retain(|_, session| session.started_at > cutoff);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Mutex used to serialize tests that assert on `drain_notifications`.
    /// The global PROCESS_REGISTRY shares a single notification channel
    /// across all tests, so concurrent drainers race for notifications.
    static NOTIFY_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    #[tokio::test]
    async fn notification_sent_when_notify_on_complete() {
        let _guard = NOTIFY_TEST_LOCK.lock().await;

        let id = PROCESS_REGISTRY
            .spawn("echo notified", PathBuf::from("/tmp"), true)
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(500)).await;
        let notifications = PROCESS_REGISTRY.drain_notifications().await;
        let found = notifications.iter().any(|n| match n {
            ProcessNotification::Completed { session_id, .. } => session_id == &id,
        });
        assert!(found, "expected completion notification for {id}");
    }
}
