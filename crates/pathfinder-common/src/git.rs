use std::collections::HashSet;
use std::path::{Path, PathBuf};
use tokio::process::Command;

/// Gets a set of files changed since a given Git target (e.g., ref, duration).
///
/// # Errors
/// Returns an error if the git command fails to execute or fails with an error status.
pub async fn get_changed_files_since(
    workspace_root: &Path,
    target: &str,
) -> Result<HashSet<PathBuf>, std::io::Error> {
    let output = Command::new("git")
        .current_dir(workspace_root)
        .args(["diff", "--name-only", target])
        .output()
        .await?;

    if !output.status.success() {
        return Err(std::io::Error::other(
            format!("git diff failed: {}", String::from_utf8_lossy(&output.stderr)),
        ));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut files = HashSet::new();
    for line in stdout.lines() {
        if !line.is_empty() {
            files.insert(PathBuf::from(line));
        }
    }

    Ok(files)
}
