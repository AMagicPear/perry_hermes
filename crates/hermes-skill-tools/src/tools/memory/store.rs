//! `MemoryStore` — persistent file-backed memory.
//!
//! Two on-disk files, `MEMORY.md` and `USER.md`, in `memories_dir`.
//! Entries are joined by the `§` delimiter (hermes-agent convention).
//! Concurrency is handled via `fs2` flock and atomic tempfile+rename.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use fs2::FileExt;
use serde::Serialize;
use thiserror::Error;
use tokio::sync::RwLock;

pub const ENTRY_DELIMITER: &str = "\n§\n";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MemoryTarget {
    Memory,
    User,
}

impl MemoryTarget {
    fn file_name(self) -> &'static str {
        match self {
            MemoryTarget::Memory => "MEMORY.md",
            MemoryTarget::User => "USER.md",
        }
    }
}

#[derive(Debug, Error, Serialize)]
#[serde(tag = "kind", content = "message", rename_all = "snake_case")]
pub enum MemoryError {
    #[error("target must be 'memory' or 'user'")]
    InvalidTarget,
    #[error("content is required")]
    MissingContent,
    #[error("no entry matched '{0}'")]
    NoMatch(String),
    #[error("multiple entries matched '{0}'; be more specific")]
    AmbiguousMatch(String),
    #[error("io error: {0}")]
    Io(String),
}

#[derive(Debug, Clone, Serialize)]
pub struct MemoryOpResult {
    pub target: MemoryTarget,
    pub entries: Vec<String>,
    pub entry_count: usize,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MemoryReadResult {
    pub target: MemoryTarget,
    pub entries: Vec<String>,
    pub entry_count: usize,
}

#[derive(Debug, Clone)]
pub struct MemoryConfig {
    pub memories_dir: PathBuf,
}

struct LiveState {
    memory_entries: Vec<String>,
    user_entries: Vec<String>,
}

pub struct MemoryStore {
    cfg: MemoryConfig,
    state: Arc<RwLock<LiveState>>,
}

impl MemoryStore {
    /// Read both memory files from disk and return a populated store.
    /// Missing files are treated as empty stores (not errors).
    pub async fn load(cfg: MemoryConfig) -> std::io::Result<Self> {
        tokio::fs::create_dir_all(&cfg.memories_dir).await?;
        let memory_entries = read_file(&cfg.memories_dir.join("MEMORY.md")).await;
        let user_entries = read_file(&cfg.memories_dir.join("USER.md")).await;
        Ok(Self {
            cfg,
            state: Arc::new(RwLock::new(LiveState {
                memory_entries,
                user_entries,
            })),
        })
    }

    /// Read-only view of the live entries for a target. Used by
    /// `MemoryBlock::load` to render the system prompt snapshot.
    pub async fn entries(&self, target: MemoryTarget) -> Vec<String> {
        let state = self.state.read().await;
        match target {
            MemoryTarget::Memory => state.memory_entries.clone(),
            MemoryTarget::User => state.user_entries.clone(),
        }
    }

    pub async fn add(
        &self,
        target: MemoryTarget,
        content: String,
    ) -> Result<MemoryOpResult, MemoryError> {
        let content = content.trim().to_string();
        if content.is_empty() {
            return Err(MemoryError::MissingContent);
        }

        let path = self.path_for(target);
        let lock_path = lock_path_for(&path);
        let _guard = FileLock::new(&lock_path).await;

        // Re-read under lock to pick up sister-session writes.
        let mut fresh = read_file(&path).await;
        fresh = dedup_in_place(fresh);

        if fresh.iter().any(|e| e == &content) {
            return Ok(success_result(
                target,
                &self.entries_for(target).await,
                Some("Entry already exists (no duplicate added)."),
            ));
        }

        fresh.push(content);
        write_file(&path, &fresh).await.map_err(|e| MemoryError::Io(e.to_string()))?;

        // Update live state.
        {
            let mut state = self.state.write().await;
            match target {
                MemoryTarget::Memory => state.memory_entries = fresh.clone(),
                MemoryTarget::User => state.user_entries = fresh.clone(),
            }
        }
        Ok(success_result(target, &fresh, Some("Entry added.")))
    }

    pub async fn replace(
        &self,
        target: MemoryTarget,
        old: &str,
        new: String,
    ) -> Result<MemoryOpResult, MemoryError> {
        let old = old.trim();
        let new = new.trim();
        if old.is_empty() {
            return Err(MemoryError::NoMatch("(empty old_text)".to_string()));
        }
        if new.is_empty() {
            return Err(MemoryError::MissingContent);
        }

        let path = self.path_for(target);
        let lock_path = lock_path_for(&path);
        let _guard = FileLock::new(&lock_path).await;

        let mut entries = read_file(&path).await;
        entries = dedup_in_place(entries);
        let matches: Vec<(usize, String)> = entries
            .iter()
            .enumerate()
            .filter(|(_, e)| e.contains(old))
            .map(|(i, e)| (i, e.clone()))
            .collect();

        let match_count = matches.len();
        match match_count {
            0 => Err(MemoryError::NoMatch(old.to_string())),
            1 => {
                let (idx, _) = matches[0];
                entries[idx] = new.to_string();
                write_file(&path, &entries).await.map_err(|e| MemoryError::Io(e.to_string()))?;
                self.set_entries(target, entries.clone()).await;
                Ok(success_result(target, &entries, Some("Entry replaced.")))
            }
            _ => {
                let unique: std::collections::HashSet<&str> =
                    matches.iter().map(|(_, e)| e.as_str()).collect();
                if unique.len() == 1 {
                    // All matches identical — replace the first.
                    let (idx, _) = matches[0];
                    entries[idx] = new.to_string();
                    write_file(&path, &entries).await.map_err(|e| MemoryError::Io(e.to_string()))?;
                    self.set_entries(target, entries.clone()).await;
                    Ok(success_result(target, &entries, Some("Entry replaced.")))
                } else {
                    Err(MemoryError::AmbiguousMatch(old.to_string()))
                }
            }
        }
    }

    pub async fn remove(
        &self,
        target: MemoryTarget,
        old: &str,
    ) -> Result<MemoryOpResult, MemoryError> {
        let old = old.trim();
        if old.is_empty() {
            return Err(MemoryError::NoMatch("(empty old_text)".to_string()));
        }

        let path = self.path_for(target);
        let lock_path = lock_path_for(&path);
        let _guard = FileLock::new(&lock_path).await;

        let mut entries = read_file(&path).await;
        entries = dedup_in_place(entries);
        let matches: Vec<(usize, String)> = entries
            .iter()
            .enumerate()
            .filter(|(_, e)| e.contains(old))
            .map(|(i, e)| (i, e.clone()))
            .collect();

        match matches.len() {
            0 => Err(MemoryError::NoMatch(old.to_string())),
            1 => {
                let (idx, _) = matches[0];
                entries.remove(idx);
                write_file(&path, &entries).await.map_err(|e| MemoryError::Io(e.to_string()))?;
                self.set_entries(target, entries.clone()).await;
                Ok(success_result(target, &entries, Some("Entry removed.")))
            }
            _ => {
                let unique: std::collections::HashSet<&str> =
                    matches.iter().map(|(_, e)| e.as_str()).collect();
                if unique.len() == 1 {
                    let (idx, _) = matches[0];
                    entries.remove(idx);
                    write_file(&path, &entries).await.map_err(|e| MemoryError::Io(e.to_string()))?;
                    self.set_entries(target, entries.clone()).await;
                    Ok(success_result(target, &entries, Some("Entry removed.")))
                } else {
                    Err(MemoryError::AmbiguousMatch(old.to_string()))
                }
            }
        }
    }

    pub async fn read(&self, target: MemoryTarget) -> Result<MemoryReadResult, MemoryError> {
        let entries = self.entries(target).await;
        Ok(MemoryReadResult {
            target,
            entry_count: entries.len(),
            entries,
        })
    }

    fn path_for(&self, target: MemoryTarget) -> PathBuf {
        self.cfg.memories_dir.join(target.file_name())
    }

    async fn entries_for(&self, target: MemoryTarget) -> Vec<String> {
        self.entries(target).await
    }

    async fn set_entries(&self, target: MemoryTarget, entries: Vec<String>) {
        let mut state = self.state.write().await;
        match target {
            MemoryTarget::Memory => state.memory_entries = entries,
            MemoryTarget::User => state.user_entries = entries,
        }
    }
}

fn success_result(
    target: MemoryTarget,
    entries: &[String],
    message: Option<&str>,
) -> MemoryOpResult {
    MemoryOpResult {
        target,
        entry_count: entries.len(),
        entries: entries.to_vec(),
        message: message.map(|s| s.to_string()),
    }
}

fn lock_path_for(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".lock");
    PathBuf::from(s)
}

fn dedup_in_place(mut entries: Vec<String>) -> Vec<String> {
    // Preserve order, keep first occurrence.
    let mut seen = std::collections::HashSet::new();
    entries.retain(|e| seen.insert(e.clone()));
    entries
}

async fn read_file(path: &Path) -> Vec<String> {
    let raw = match tokio::fs::read_to_string(path).await {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(e) => {
            tracing::warn!("failed to read {}: {e}", path.display());
            return Vec::new();
        }
    };
    if raw.trim().is_empty() {
        return Vec::new();
    }
    raw.split(ENTRY_DELIMITER)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

async fn write_file(path: &Path, entries: &[String]) -> std::io::Result<()> {
    let content = if entries.is_empty() {
        String::new()
    } else {
        entries.join(ENTRY_DELIMITER)
    };
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    // Write to a temp file in the same directory, then atomically rename.
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("memory file has no parent directory"))?;
    let mut tmp = tempfile::Builder::new()
        .prefix(".mem_")
        .suffix(".tmp")
        .tempfile_in(parent)?;
    use std::io::Write;
    tmp.write_all(content.as_bytes())?;
    tmp.flush()?;
    let tmp_path = tmp.path().to_path_buf();
    tmp.into_temp_path().persist(path).map_err(|e| {
        std::io::Error::other(format!(
            "failed to rename temp file to {}: {e}",
            path.display()
        ))
    })?;
    // Ensure the persisted file exists at `path`; `persist` does that.
    let _ = tmp_path;
    Ok(())
}

/// Blocking flock wrapper. `fs2::FileExt::lock_exclusive` is sync; we
/// run it on a blocking task to keep `MemoryStore` async.
struct FileLock {
    _file: std::fs::File,
    path: PathBuf,
}

impl FileLock {
    async fn new(path: &Path) -> Self {
        if let Some(parent) = path.parent() {
            let _ = tokio::fs::create_dir_all(parent).await;
        }
        let path = path.to_path_buf();
        let file = tokio::task::spawn_blocking({
            let path = path.clone();
            move || -> std::io::Result<std::fs::File> {
                let f = std::fs::OpenOptions::new()
                    .create(true)
                    .truncate(false)
                    .read(true)
                    .write(true)
                    .open(&path)?;
                f.lock_exclusive()?;
                Ok(f)
            }
        })
        .await
        .expect("blocking task panicked")
        .expect("failed to acquire memory file lock");
        Self { _file: file, path }
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        // Drop the file fd first (drops `_file`); this releases the
        // flock. Then remove the lock file itself. Removal is
        // best-effort: if it fails (e.g. another process raced us to
        // open it for the next critical section, or a sister process
        // is mid-rotation), leave it — flock is advisory, so a stale
        // file does not corrupt correctness.
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_cfg() -> (tempfile::TempDir, MemoryConfig) {
        let dir = tempfile::tempdir().unwrap();
        let cfg = MemoryConfig {
            memories_dir: dir.path().to_path_buf(),
        };
        (dir, cfg)
    }

    #[tokio::test]
    async fn load_with_no_files_yields_empty_stores() {
        let (_dir, cfg) = temp_cfg();
        let store = MemoryStore::load(cfg).await.unwrap();
        assert!(store.entries(MemoryTarget::Memory).await.is_empty());
        assert!(store.entries(MemoryTarget::User).await.is_empty());
    }

    #[tokio::test]
    async fn load_reads_existing_files() {
        let (dir, cfg) = temp_cfg();
        std::fs::write(
            dir.path().join("MEMORY.md"),
            "alpha\n§\nbeta\n§\ngamma",
        )
        .unwrap();
        let store = MemoryStore::load(cfg).await.unwrap();
        let entries = store.entries(MemoryTarget::Memory).await;
        assert_eq!(entries, vec!["alpha", "beta", "gamma"]);
    }

    #[tokio::test]
    async fn add_appends_to_disk_and_state() {
        let (dir, cfg) = temp_cfg();
        let store = MemoryStore::load(cfg).await.unwrap();
        let result = store.add(MemoryTarget::Memory, "hello".into()).await.unwrap();
        assert_eq!(result.entry_count, 1);
        assert_eq!(result.entries, vec!["hello"]);
        let on_disk = std::fs::read_to_string(dir.path().join("MEMORY.md")).unwrap();
        assert_eq!(on_disk, "hello");
    }

    #[tokio::test]
    async fn add_rejects_empty_content() {
        let (_dir, cfg) = temp_cfg();
        let store = MemoryStore::load(cfg).await.unwrap();
        let err = store
            .add(MemoryTarget::Memory, "   \n  ".into())
            .await
            .unwrap_err();
        assert!(matches!(err, MemoryError::MissingContent));
    }

    #[tokio::test]
    async fn add_skips_exact_duplicate() {
        let (_dir, cfg) = temp_cfg();
        let store = MemoryStore::load(cfg).await.unwrap();
        store.add(MemoryTarget::Memory, "x".into()).await.unwrap();
        let result = store.add(MemoryTarget::Memory, "x".into()).await.unwrap();
        assert_eq!(result.entry_count, 1);
        assert!(result.message.unwrap().contains("no duplicate"));
    }

    #[tokio::test]
    async fn replace_with_one_match_substitutes() {
        let (dir, cfg) = temp_cfg();
        let store = MemoryStore::load(cfg).await.unwrap();
        store.add(MemoryTarget::Memory, "old text".into()).await.unwrap();
        store
            .replace(MemoryTarget::Memory, "old text", "new text".to_string())
            .await
            .unwrap();
        let on_disk = std::fs::read_to_string(dir.path().join("MEMORY.md")).unwrap();
        assert_eq!(on_disk, "new text");
    }

    #[tokio::test]
    async fn replace_with_no_match_returns_no_match_error() {
        let (_dir, cfg) = temp_cfg();
        let store = MemoryStore::load(cfg).await.unwrap();
        store.add(MemoryTarget::Memory, "x".into()).await.unwrap();
        let err = store
            .replace(MemoryTarget::Memory, "zzz", "y".to_string())
            .await
            .unwrap_err();
        assert!(matches!(err, MemoryError::NoMatch(ref s) if s == "zzz"));
    }

    #[tokio::test]
    async fn replace_with_ambiguous_distinct_matches_errors() {
        let (_dir, cfg) = temp_cfg();
        let store = MemoryStore::load(cfg).await.unwrap();
        store.add(MemoryTarget::Memory, "abc one".into()).await.unwrap();
        store.add(MemoryTarget::Memory, "abc two".into()).await.unwrap();
        let err = store
            .replace(MemoryTarget::Memory, "abc", "x".to_string())
            .await
            .unwrap_err();
        assert!(matches!(err, MemoryError::AmbiguousMatch(ref s) if s == "abc"));
    }

    #[tokio::test]
    async fn replace_with_identical_matches_replaces_first() {
        let (_dir, cfg) = temp_cfg();
        let store = MemoryStore::load(cfg).await.unwrap();
        store.add(MemoryTarget::Memory, "same".into()).await.unwrap();
        store.add(MemoryTarget::Memory, "same".into()).await.unwrap();
        store
            .replace(MemoryTarget::Memory, "same", "different".to_string())
            .await
            .unwrap();
        let entries = store.entries(MemoryTarget::Memory).await;
        // After dedup-then-replace, only one "different" remains.
        assert_eq!(entries, vec!["different"]);
    }

    #[tokio::test]
    async fn remove_deletes_matching_entry() {
        let (_dir, cfg) = temp_cfg();
        let store = MemoryStore::load(cfg).await.unwrap();
        store.add(MemoryTarget::Memory, "alpha".into()).await.unwrap();
        store.add(MemoryTarget::Memory, "beta".into()).await.unwrap();
        store.remove(MemoryTarget::Memory, "alpha").await.unwrap();
        let entries = store.entries(MemoryTarget::Memory).await;
        assert_eq!(entries, vec!["beta"]);
    }

    #[tokio::test]
    async fn remove_with_no_match_returns_error() {
        let (_dir, cfg) = temp_cfg();
        let store = MemoryStore::load(cfg).await.unwrap();
        let err = store
            .remove(MemoryTarget::Memory, "nope")
            .await
            .unwrap_err();
        assert!(matches!(err, MemoryError::NoMatch(_)));
    }

    #[tokio::test]
    async fn read_returns_current_entries() {
        let (_dir, cfg) = temp_cfg();
        let store = MemoryStore::load(cfg).await.unwrap();
        store.add(MemoryTarget::Memory, "x".into()).await.unwrap();
        store.add(MemoryTarget::User, "y".into()).await.unwrap();
        let m = store.read(MemoryTarget::Memory).await.unwrap();
        assert_eq!(m.entries, vec!["x"]);
        let u = store.read(MemoryTarget::User).await.unwrap();
        assert_eq!(u.entries, vec!["y"]);
    }

    #[tokio::test]
    async fn on_disk_round_trips_through_load() {
        let (dir, cfg) = temp_cfg();
        let store = MemoryStore::load(cfg.clone()).await.unwrap();
        store.add(MemoryTarget::Memory, "one".into()).await.unwrap();
        store.add(MemoryTarget::Memory, "two".into()).await.unwrap();
        drop(store);
        // Re-load from the same dir.
        let store2 = MemoryStore::load(cfg).await.unwrap();
        assert_eq!(
            store2.entries(MemoryTarget::Memory).await,
            vec!["one", "two"]
        );
        let on_disk = std::fs::read_to_string(dir.path().join("MEMORY.md")).unwrap();
        assert_eq!(on_disk, "one\n§\ntwo");
    }

    #[tokio::test]
    async fn lock_file_is_removed_after_mutator_releases() {
        // After an add() call completes, the .lock file used for
        // flock should be cleaned up — not left behind as a 0-byte
        // artifact on disk. Sister sessions and the next mutator
        // re-create it on demand.
        let (dir, cfg) = temp_cfg();
        let store = MemoryStore::load(cfg).await.unwrap();
        store.add(MemoryTarget::Memory, "entry".into()).await.unwrap();

        let lock_path = dir.path().join("MEMORY.md.lock");
        assert!(
            !lock_path.exists(),
            "lock file should be cleaned up after add completes, but {lock_path:?} still exists"
        );
    }
}