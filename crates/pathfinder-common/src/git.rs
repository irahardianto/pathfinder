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
        if target.starts_with('-') {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "invalid git revision: cannot start with '-'",
            ));
        }

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

#[cfg(test)]
#[path = "git_test.rs"]
mod tests;
