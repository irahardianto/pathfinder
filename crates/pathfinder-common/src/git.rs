use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Abstracts the execution of a git subprocess, enabling unit testing without a
/// real git installation or repository.
///
/// Callers use monomorphisation (`<R: GitRunner>`) rather than `&dyn GitRunner`
/// to avoid `async-trait`. The trait is desugared from `async fn` to
/// `-> impl Future + Send` so that the `Send` bound is explicit and enforced
/// for all implementations — a requirement for use inside `tokio::spawn`.
pub trait GitRunner: Send + Sync {
    /// Runs `git diff --name-only <target>` rooted at `workspace_root` and
    /// returns the raw stdout bytes on success, or an I/O error on failure.
    // ALLOW: the manual desugaring is intentional — it pins the `Send` bound
    // that `async fn in trait` cannot guarantee without it.
    #[allow(clippy::manual_async_fn)]
    fn diff_name_only<'a>(
        &'a self,
        workspace_root: &'a Path,
        target: &'a str,
    ) -> impl std::future::Future<Output = Result<Vec<u8>, std::io::Error>> + Send + 'a;
}

/// Production implementation that shells out to the real `git` binary.
pub struct SystemGit;

impl GitRunner for SystemGit {
    async fn diff_name_only(
        &self,
        workspace_root: &Path,
        target: &str,
    ) -> Result<Vec<u8>, std::io::Error> {
        let output = tokio::process::Command::new("git")
            .current_dir(workspace_root)
            .args(["diff", "--name-only", target])
            .output()
            .await?;

        if !output.status.success() {
            return Err(std::io::Error::other(format!(
                "git diff failed: {}",
                String::from_utf8_lossy(&output.stderr)
            )));
        }

        Ok(output.stdout)
    }
}

/// Maximum time we allow a `git diff` call to run before treating it as hung.
const GIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Gets a set of files changed since a given Git target (e.g., ref, duration).
///
/// Uses the provided [`GitRunner`] so callers can inject a fake in tests.
/// A hard timeout of [`GIT_TIMEOUT`] (10 s) is applied; if `git` blocks longer,
/// the future is cancelled and a [`std::io::ErrorKind::TimedOut`] error is
/// returned.
///
/// # Errors
/// Returns an error if:
/// - `git` fails to execute or exits non-zero.
/// - The operation exceeds [`GIT_TIMEOUT`].
pub async fn get_changed_files_since<R: GitRunner>(
    runner: &R,
    workspace_root: &Path,
    target: &str,
) -> Result<HashSet<PathBuf>, std::io::Error> {
    tracing::debug!(
        operation = "get_changed_files_since",
        target = target,
        workspace = %workspace_root.display(),
        "git diff operation starting"
    );

    let raw = tokio::time::timeout(GIT_TIMEOUT, runner.diff_name_only(workspace_root, target))
        .await
        .map_err(|_elapsed| {
            tracing::error!(
                operation = "get_changed_files_since",
                target = target,
                timeout_secs = GIT_TIMEOUT.as_secs(),
                "git diff timed out"
            );
            std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!("git diff timed out after {}s", GIT_TIMEOUT.as_secs()),
            )
        })??;

    let stdout = String::from_utf8_lossy(&raw);
    let mut files = HashSet::new();
    for line in stdout.lines() {
        if !line.is_empty() {
            files.insert(PathBuf::from(line));
        }
    }

    tracing::debug!(
        operation = "get_changed_files_since",
        target = target,
        file_count = files.len(),
        "git diff operation completed"
    );

    Ok(files)
}

// Allow `expect_used` within tests: panics on assertion failure are the intended
// behaviour in a test context and are acceptable per the Rust idioms guide.
#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
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
                tokio::time::sleep(Duration::from_secs(3600)).await;
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
}
