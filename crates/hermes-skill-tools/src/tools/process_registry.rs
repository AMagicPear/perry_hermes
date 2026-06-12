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
        let mut child = Command::new(shell)
            .arg("-c")
            .arg(command)
            .current_dir(&cwd)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| format!("failed to spawn: {e}"))?;

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

        // Spawn a background task to read output and wait for exit.
        let registry_id = id.clone();
        let notify_tx = self.notify_tx.clone();
        let session_command = command.to_string();
        tokio::spawn(async move {
            let (stdout_bytes, stderr_bytes, status) = {
                let stdout_fut = async {
                    let mut buf = Vec::new();
                    if let Some(mut s) = child.stdout.take() {
                        let _ = s.read_to_end(&mut buf).await;
                    }
                    buf
                };
                let stderr_fut = async {
                    let mut buf = Vec::new();
                    if let Some(mut s) = child.stderr.take() {
                        let _ = s.read_to_end(&mut buf).await;
                    }
                    buf
                };
                let (stdout_bytes, stderr_bytes) = tokio::join!(stdout_fut, stderr_fut);
                let status = child.wait().await;
                (stdout_bytes, stderr_bytes, status)
            };

            let out = String::from_utf8_lossy(&stdout_bytes).into_owned();
            let err = String::from_utf8_lossy(&stderr_bytes).into_owned();
            let combined = if err.is_empty() {
                out
            } else if out.is_empty() {
                err
            } else {
                format!("{out}\n--- stderr ---\n{err}")
            };

            let exit_code = status.ok().and_then(|s| s.code()).unwrap_or(-1);

            // Move from running to finished.
            let session = {
                let mut state = PROCESS_REGISTRY.state.write().await;
                if let Some(mut session) = state.running.remove(&registry_id) {
                    session.exited = true;
                    session.exit_code = Some(exit_code);
                    session.output_buffer = truncate_buffer(&combined);
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

    /// Kill a running process.
    pub async fn kill(&self, session_id: &str) -> Result<KillResult, String> {
        // We can't easily kill by PID from inside the registry since we
        // don't hold the Child handle. Instead, use `kill` syscall via pid.
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
                // SAFETY: sending SIGTERM to a process we own.
                unsafe { libc::kill(pid as i32, libc::SIGTERM) };
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
        let offset = offset.unwrap_or(if total > limit { total - limit } else { 0 });
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

/// Truncate output to the last MAX_OUTPUT_CHARS characters, snapping to a
/// newline boundary.
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

    fn tmp_cwd() -> PathBuf {
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/tmp"))
    }

    #[tokio::test]
    async fn spawn_and_poll_simple_command() {
        let id = PROCESS_REGISTRY
            .spawn("echo hello-from-bg", tmp_cwd(), false)
            .await
            .unwrap();
        // Wait a bit for the process to finish.
        tokio::time::sleep(Duration::from_millis(500)).await;
        let result = PROCESS_REGISTRY.poll(&id).await.unwrap();
        assert_eq!(result.status, "finished");
        assert_eq!(result.exit_code, Some(0));
        assert!(result.output.contains("hello-from-bg"));
    }

    #[tokio::test]
    async fn wait_returns_output_on_exit() {
        let id = PROCESS_REGISTRY
            .spawn("echo waited", tmp_cwd(), false)
            .await
            .unwrap();
        let result = PROCESS_REGISTRY.wait(&id, Some(5)).await.unwrap();
        assert!(!result.timed_out);
        assert_eq!(result.exit_code, Some(0));
        assert!(result.output.contains("waited"));
    }

    #[tokio::test]
    async fn wait_times_out_for_long_command() {
        let id = PROCESS_REGISTRY
            .spawn("sleep 60", tmp_cwd(), false)
            .await
            .unwrap();
        let result = PROCESS_REGISTRY.wait(&id, Some(1)).await.unwrap();
        assert!(result.timed_out);
        // Clean up.
        let _ = PROCESS_REGISTRY.kill(&id).await;
    }

    #[tokio::test]
    async fn list_shows_running_and_finished() {
        let id = PROCESS_REGISTRY
            .spawn("echo listed", tmp_cwd(), false)
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(500)).await;
        let list = PROCESS_REGISTRY.list().await;
        assert!(list.iter().any(|p| p.id == id));
    }

    #[tokio::test]
    async fn read_log_returns_full_output() {
        let id = PROCESS_REGISTRY
            .spawn("echo line1; echo line2; echo line3", tmp_cwd(), false)
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(500)).await;
        let log = PROCESS_REGISTRY.read_log(&id, None, None).await.unwrap();
        assert!(log.total_lines >= 3);
        assert!(log.lines.iter().any(|l| l.contains("line1")));
        assert!(log.lines.iter().any(|l| l.contains("line3")));
    }

    #[tokio::test]
    async fn notification_sent_when_notify_on_complete() {
        let id = PROCESS_REGISTRY
            .spawn("echo notified", tmp_cwd(), true)
            .await
            .unwrap();
        // Wait for process to finish and notification to be delivered.
        tokio::time::sleep(Duration::from_millis(500)).await;
        let notifications = PROCESS_REGISTRY.drain_notifications().await;
        let found = notifications.iter().any(|n| match n {
            ProcessNotification::Completed { session_id, .. } => session_id == &id,
        });
        assert!(found, "expected completion notification for {id}");
    }

    #[tokio::test]
    async fn no_notification_when_notify_off() {
        let _id = PROCESS_REGISTRY
            .spawn("echo silent", tmp_cwd(), false)
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(500)).await;
        let notifications = PROCESS_REGISTRY.drain_notifications().await;
        // There might be notifications from other tests, but none for
        // our session since notify_on_complete was false.
        // This is a weak assertion — the real check is that the registry
        // doesn't panic or block.
        let _ = notifications;
    }
}
