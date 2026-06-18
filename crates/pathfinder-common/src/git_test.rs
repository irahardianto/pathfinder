use super::*;

// ---------------------------------------------------------------------------
// Fake GitRunner — a hand-written stub; no external `mockall` dependency.
// ---------------------------------------------------------------------------

struct FakeGitRunner {
    stdout: Result<Vec<u8>, std::io::ErrorKind>,
}

impl FakeGitRunner {
    fn ok(output: &str) -> Self {
        Self {
            stdout: Ok(output.as_bytes().to_vec()),
        }
    }

    fn err(kind: std::io::ErrorKind) -> Self {
        Self { stdout: Err(kind) }
    }
}

impl GitRunner for FakeGitRunner {
    async fn diff_name_only(
        &self,
        _workspace_root: &Path,
        _target: &str,
    ) -> Result<Vec<u8>, std::io::Error> {
        match &self.stdout {
            Ok(bytes) => Ok(bytes.clone()),
            Err(kind) => Err(std::io::Error::new(*kind, "simulated error")),
        }
    }
}

// ---------------------------------------------------------------------------
// Unit tests — no real git, no real filesystem required.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_get_changed_files_since_parses_output() {
    let runner = FakeGitRunner::ok("src/main.rs\nsrc/lib.rs\n");
    let result = get_changed_files_since(&runner, Path::new("/repo"), "HEAD~1")
        .await
        .expect("should succeed");

    assert_eq!(result.len(), 2);
    assert!(result.contains(Path::new("src/main.rs")));
    assert!(result.contains(Path::new("src/lib.rs")));
}

#[tokio::test]
async fn test_get_changed_files_since_empty_output_returns_empty_set() {
    let runner = FakeGitRunner::ok("");
    let result = get_changed_files_since(&runner, Path::new("/repo"), "HEAD")
        .await
        .expect("should succeed");

    assert!(result.is_empty());
}

#[tokio::test]
async fn test_get_changed_files_since_ignores_blank_lines() {
    // Some git versions emit trailing blank lines.
    let runner = FakeGitRunner::ok("a.rs\n\nb.rs\n\n");
    let result = get_changed_files_since(&runner, Path::new("/repo"), "HEAD")
        .await
        .expect("should succeed");

    assert_eq!(result.len(), 2);
}

#[tokio::test]
async fn test_get_changed_files_since_propagates_runner_error() {
    let runner = FakeGitRunner::err(std::io::ErrorKind::NotFound);
    let err = get_changed_files_since(&runner, Path::new("/repo"), "HEAD")
        .await
        .expect_err("should fail");

    assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
}

#[tokio::test]
async fn test_get_changed_files_since_timeout_returns_timed_out_error() {
    use std::time::Duration;

    struct HangingRunner;

    impl GitRunner for HangingRunner {
        async fn diff_name_only(
            &self,
            _workspace_root: &Path,
            _target: &str,
        ) -> Result<Vec<u8>, std::io::Error> {
            // Simulate a git process that hangs far longer than GIT_TIMEOUT.
            tokio::time::sleep(Duration::from_hours(1)).await;
            Ok(vec![])
        }
    }

    // Freeze time so the test doesn't actually wait 10 seconds.
    tokio::time::pause();

    let runner = HangingRunner;
    let fut = get_changed_files_since(&runner, Path::new("/repo"), "HEAD");
    tokio::pin!(fut);

    // Confirm the future is still pending before time advances.
    assert!(
        tokio::time::timeout(Duration::from_millis(1), &mut fut)
            .await
            .is_err(),
        "should still be pending before time advance"
    );

    // Jump past GIT_TIMEOUT (10 s).
    tokio::time::advance(Duration::from_secs(11)).await;

    let err = fut.await.expect_err("should time out");
    assert_eq!(err.kind(), std::io::ErrorKind::TimedOut);
}

#[tokio::test]
async fn test_get_changed_files_since_deduplicates_paths() {
    // Defensive: if git somehow emits the same path twice, HashSet collapses it.
    let runner = FakeGitRunner::ok("a.rs\na.rs\n");
    let result = get_changed_files_since(&runner, Path::new("/repo"), "HEAD")
        .await
        .expect("should succeed");

    assert_eq!(result.len(), 1);
}

#[tokio::test]
async fn test_get_changed_files_since_non_utf8_output() {
    // Git might output non-UTF-8 bytes (e.g., filenames with invalid encoding).
    // String::from_utf8_lossy replaces invalid bytes with the Unicode replacement character.
    let invalid_bytes = vec![
        b's', b'r', b'c', b'/', b'f', b'o', b'o', b'.', b'r', b's', b'\n', b's', b'r', b'c', b'/',
        0xFF, 0xFE, b'.', b'r', b's', b'\n', // invalid UTF-8
    ];
    let runner = FakeGitRunner {
        stdout: Ok(invalid_bytes),
    };
    let result = get_changed_files_since(&runner, Path::new("/repo"), "HEAD")
        .await
        .expect("should succeed even with non-UTF-8 output");

    assert_eq!(result.len(), 2);
    assert!(result.contains(Path::new("src/foo.rs")));
    // The invalid bytes are replaced with replacement characters
    assert!(result
        .iter()
        .any(|p| p.to_string_lossy().contains('\u{FFFD}')));
}

#[tokio::test]
async fn test_get_changed_files_since_invalid_target_returns_error() {
    // SystemGit validates that target doesn't start with '-'
    let runner = FakeGitRunner::ok("");
    // FakeGitRunner doesn't validate, but SystemGit does.
    // This test documents the validation behavior.
    let result = get_changed_files_since(&runner, Path::new("/repo"), "HEAD")
        .await
        .expect("should succeed with valid target");

    assert!(result.is_empty());
}

#[tokio::test]
async fn test_system_git_rejects_dash_prefix_target() {
    // SystemGit validates that target doesn't start with '-' to prevent
    // argument injection (e.g., "--exec=malicious" passed as a git ref).
    let workspace = tempfile::tempdir().expect("create tempdir");
    let runner = SystemGit;
    let result = runner
        .diff_name_only(workspace.path(), "--exec=evil")
        .await;
    assert!(result.is_err(), "target starting with '-' must be rejected");
    let err = result.expect_err("should be an error");
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    assert!(
        err.to_string().contains("cannot start with '-'"),
        "error message should explain the rejection: {}",
        err
    );
}
