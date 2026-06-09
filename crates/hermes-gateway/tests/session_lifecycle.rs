// Cross-crate integration tests for the gateway session lifecycle:
//
// When a `SessionRegistry` writes an `AgentSession` to disk, a fresh
// registry built against the same sessions directory must reload the
// conversation history on restart (the load-on-`get_or_create` fix).
//
// The tests also exercise the archive lifecycle:
// - `archive_active` moves the active file aside, so a restart
// after shutdown begins with an empty session - matching the
// manual smoke test described in the plan ("Messages:0 (the new
// run) and Archived:1 (the previous run)").
// - A corrupt active JSON file is quarantined under
// `.archive/<key>/` instead of crashing the registry.
// - `reset` archives the active snapshot, then leaves an empty
// active file behind for the next session to load.

use std::path::PathBuf;

use perry_hermes_agent::{SessionRegistry, format_session_id};

#[tokio::test]
async fn registry_restart_loads_existing_history() {
    let tmp = tempfile::tempdir().unwrap();
    let sessions_dir = tmp.path().join("sessions");
    std::fs::create_dir_all(&sessions_dir).unwrap();

    let key = "telegram:dm:42";

    // First "process": create the session and append some messages,
    // then drop the registry without archiving. The active JSON file
    // is the source of truth for a restart.
    {
        let registry =
            SessionRegistry::new(sessions_dir.clone(), PathBuf::from("/tmp/project"), None);
        let entry = registry.get_or_create(key).await;
        entry
            .session
            .append_message(perry_hermes_core::Message::user("remember this"))
            .await;
    }

    let session_id = format_session_id(key);
    let active = sessions_dir.join(format!("{session_id}.json"));
    assert!(
        active.exists(),
        "active session file should remain after drop"
    );

    // Second "process": rebuild the registry, ask for the same key,
    // and confirm the history is loaded from disk.
    {
        let registry =
            SessionRegistry::new(sessions_dir.clone(), PathBuf::from("/tmp/project"), None);
        let entry = registry.get_or_create(key).await;
        let messages = entry.session.messages().await;
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].content.as_text(), "remember this");
    }
}

#[tokio::test]
async fn registry_archive_active_moves_file_and_restart_starts_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let sessions_dir = tmp.path().join("sessions");
    std::fs::create_dir_all(&sessions_dir).unwrap();

    let key = "telegram:dm:42";
    let session_id = format_session_id(key);

    // First "process": create the session, append, then archive on
    // shutdown (mirrors the CLI shutdown hook).
    {
        let registry =
            SessionRegistry::new(sessions_dir.clone(), PathBuf::from("/tmp/project"), None);
        let entry = registry.get_or_create(key).await;
        entry
            .session
            .append_message(perry_hermes_core::Message::user("remember this"))
            .await;
        assert!(registry.archive_active(key).await.is_some());
    }

    let active = sessions_dir.join(format!("{session_id}.json"));
    assert!(!active.exists(), "archive must move the active file aside");

    let archive_dir = sessions_dir.join(".archive").join(&session_id);
    assert_eq!(std::fs::read_dir(&archive_dir).unwrap().count(), 1);

    // Second "process": no active file on disk, so the registry
    // produces a fresh empty session (matches the plan's smoke test:
    // "Messages:0 (the new run) and Archived:1 (the previous run)").
    {
        let registry =
            SessionRegistry::new(sessions_dir.clone(), PathBuf::from("/tmp/project"), None);
        let entry = registry.get_or_create(key).await;
        assert!(entry.session.messages().await.is_empty());
    }
}

#[tokio::test]
async fn registry_corrupt_file_is_quarantined_and_session_starts_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let sessions_dir = tmp.path().join("sessions");
    std::fs::create_dir_all(&sessions_dir).unwrap();
    let key = "telegram:dm:42";
    let session_id = format_session_id(key);
    let active = sessions_dir.join(format!("{session_id}.json"));
    std::fs::write(&active, b"this is not valid json").unwrap();

    let registry = SessionRegistry::new(sessions_dir.clone(), PathBuf::from("/tmp/project"), None);
    let entry = registry.get_or_create(key).await;
    assert!(entry.session.messages().await.is_empty());

    // The bad file is now under .archive/<key>/<ts>.corrupt.json.
    let quarantine = sessions_dir.join(".archive").join(&session_id);
    let quarantined: Vec<_> = std::fs::read_dir(&quarantine)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".corrupt.json"))
        .collect();
    assert_eq!(quarantined.len(), 1);
    assert!(!active.exists(), "corrupt active file should be gone");
}

#[tokio::test]
async fn reset_archives_then_starts_fresh_on_next_get_or_create() {
    let tmp = tempfile::tempdir().unwrap();
    let sessions_dir = tmp.path().join("sessions");
    std::fs::create_dir_all(&sessions_dir).unwrap();

    let key = "telegram:dm:99";
    let session_id = format_session_id(key);

    // Seed a session, reset, then verify a new get_or_create is empty.
    {
        let registry =
            SessionRegistry::new(sessions_dir.clone(), PathBuf::from("/tmp/project"), None);
        let entry = registry.get_or_create(key).await;
        entry
            .session
            .append_message(perry_hermes_core::Message::user("first turn"))
            .await;
        assert!(registry.reset(key).await);
        assert!(entry.session.messages().await.is_empty());
    }

    // The archive holds one entry.
    let archive_dir = sessions_dir.join(".archive").join(&session_id);
    assert_eq!(std::fs::read_dir(&archive_dir).unwrap().count(), 1);

    // A fresh registry sees the active file (empty after reset) and
    // loads an empty message log.
    {
        let registry =
            SessionRegistry::new(sessions_dir.clone(), PathBuf::from("/tmp/project"), None);
        let entry = registry.get_or_create(key).await;
        assert!(entry.session.messages().await.is_empty());
    }
}
