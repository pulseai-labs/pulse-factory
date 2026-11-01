//! AC-2: ADR-0008's claim that `portable-pty` can spawn, read, write and resize from Tokio. A
//! stub shell program runs in a PTY inside a Tokio test. The test reads its output, writes to
//! it and resizes it, and the stub reports the new size.

mod common;

use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use std::time::Duration;

use pf_adapter_omp::pty::{PtySession, SpawnSpec};

fn write_stub(dir: &std::path::Path) -> std::path::PathBuf {
    let path = dir.join("stub.sh");
    // It prints its size when the terminal resizes, and it echoes each line it reads.
    std::fs::write(
        &path,
        r#"#!/bin/bash
trap 'echo "SIZE $(stty size)"' WINCH
echo "READY $(stty size)"
while true; do
  if read -t 0.05 line; then echo "GOT $line"; fi
done
"#,
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

#[tokio::test]
async fn spawn_read_write_and_resize_from_tokio() {
    let dir = tempfile::tempdir().unwrap();
    let stub = write_stub(dir.path());
    let spec = SpawnSpec {
        program: stub,
        args: vec![],
        cwd: dir.path().to_path_buf(),
        env: vec![("PF_PTY_TEST".into(), "1".into())],
        rows: 24,
        cols: 80,
    };

    // Spawn runs on a blocking thread. The session is shared with later blocking calls.
    let session = tokio::task::spawn_blocking(move || PtySession::spawn(&spec))
        .await
        .unwrap()
        .expect("the PTY spawns");
    let session = Arc::new(session);

    // Read: the stub prints its first size.
    let s = Arc::clone(&session);
    let ready =
        tokio::task::spawn_blocking(move || s.wait_for("READY 24 80", Duration::from_secs(10)))
            .await
            .unwrap();
    assert!(ready, "screen was: {}", session.screen());

    // Write: the stub echoes a line.
    let s = Arc::clone(&session);
    let echoed = tokio::task::spawn_blocking(move || {
        s.write(b"hello pty\n").unwrap();
        s.wait_for("GOT hello pty", Duration::from_secs(10))
    })
    .await
    .unwrap();
    assert!(echoed, "screen was: {}", session.screen());

    // Resize: the stub reports the new size.
    let s = Arc::clone(&session);
    let resized = tokio::task::spawn_blocking(move || {
        s.resize(30, 100).unwrap();
        s.wait_for("SIZE 30 100", Duration::from_secs(10))
    })
    .await
    .unwrap();
    assert!(resized, "screen was: {}", session.screen());

    // The process ends when the driver kills it.
    let s = Arc::clone(&session);
    let code = tokio::task::spawn_blocking(move || {
        s.kill();
        s.exit_code()
    })
    .await
    .unwrap();
    assert!(code.is_some(), "the stub is gone after kill");
}

#[test]
fn a_program_that_does_not_exist_fails_to_spawn() {
    let dir = tempfile::tempdir().unwrap();
    let spec = SpawnSpec {
        program: dir.path().join("missing"),
        args: vec![],
        cwd: dir.path().to_path_buf(),
        env: vec![],
        rows: 24,
        cols: 80,
    };
    assert!(PtySession::spawn(&spec).is_err());
}
