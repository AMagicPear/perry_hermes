//! End-to-end smoke for the `hermes` binary's config resolution and
//! basic REPL round-trip with the `echo` provider.

use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// Path to a fresh, empty scratch dir under the system temp dir.
fn scratch(label: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!(
        "hermes-cli-itest-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&p).unwrap();
    p
}

fn hermes_bin() -> &'static str {
    env!("CARGO_BIN_EXE_hermes")
}

#[test]
fn hermes_errors_when_no_config_is_found() {
    let home = scratch("nohome");
    let cwd = scratch("nocwd");
    fs::create_dir_all(home.join(".perry_hermes")).unwrap();

    let output = Command::new(hermes_bin())
        .env("HOME", &home)
        .current_dir(&cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to spawn hermes");

    assert!(!output.status.success(), "expected non-zero exit, got {:?}", output.status);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("no hermes config found"),
        "stderr should explain the lookup failure, got: {stderr}"
    );
    assert!(stderr.contains(".perry_hermes"), "stderr should name the home path: {stderr}");
    assert!(stderr.contains("hermes.toml"), "stderr should name the cwd path: {stderr}");
}

#[test]
fn hermes_picks_up_cwd_hermes_toml() {
    let home = scratch("cwdhome"); // empty HOME so ~/.perry_hermes/config.toml is absent
    let cwd = scratch("cwdtoml");
    let config_path = cwd.join("hermes.toml");
    fs::write(&config_path, "[provider]\nkind=\"echo\"\n").unwrap();

    let mut child = Command::new(hermes_bin())
        .env("HOME", &home)
        .current_dir(&cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn hermes");

    // Write one line, then close stdin so the REPL exits cleanly.
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"hello\n")
        .expect("write to stdin");
    drop(child.stdin.take());

    let output = child.wait_with_output().expect("failed to wait on hermes");
    assert!(
        output.status.success(),
        "expected zero exit on EOF, got {:?}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    // Echo provider echoes back "echo: hello"; the REPL streams it via
    // ContentDelta. The on_event closure in main.rs uses eprint! (stderr)
    // for status/output so stdout stays clean for piping.
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("echo: hello"),
        "stderr should contain the echoed response, got: {stderr}"
    );
}

#[test]
fn hermes_respects_explicit_config_flag() {
    let home = scratch("flaghome"); // empty HOME
    let cwd = scratch("flagcwd");   // empty cwd
    let config_dir = scratch("flagcfg");
    let config_path = config_dir.join("my-config.toml");
    fs::write(
        &config_path,
        "[provider]\nkind=\"echo\"\n[agent]\nmax_iterations=2\n",
    )
    .unwrap();

    let mut child = Command::new(hermes_bin())
        .env("HOME", &home)
        .current_dir(&cwd)
        .arg("--config")
        .arg(&config_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn hermes");

    child.stdin.as_mut().unwrap().write_all(b"hi\n").unwrap();
    drop(child.stdin.take());

    let output = child.wait_with_output().expect("failed to wait on hermes");
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
}
