#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::uninlined_format_args
)]

//! Integration tests for `SystemGit` using real `git` subprocess calls.
//!
//! These tests create real git repositories in temporary directories and
//! exercise `SystemGit::diff_name_only` against actual git history. This
//! verifies that the `git` subprocess is invoked correctly, its output is
//! parsed, and error paths are handled — things that the `FakeGitRunner`
//! mock cannot prove.
//!
//! # Prerequisites
//!
//! The `git` binary must be present in `$PATH`. On CI this is always true.
//! On developer machines without git, tests will fail at the repo setup step.
//!
//! # Future agents
//!
//! Add tests here for new `GitRunner` methods as they are added to the trait.
//! Follow the pattern: init repo → make commits → call the method → assert.
//! Use `git_repo()` to get a fully initialized temporary repository.

use pathfinder_common::git::{GitRunner, SystemGit};
use std::process::Command;
use tempfile::TempDir;

/// Create a minimal git repository in a temporary directory.
///
/// Returns the `TempDir` (keep it alive for the test duration) and runs
/// the initial commit so `HEAD` is valid.
fn git_repo_with_initial_commit() -> TempDir {
    let dir = tempfile::tempdir().expect("failed to create tempdir");
    let root = dir.path();

    // Initialize repo with a default branch name to avoid git config warnings
    Command::new("git")
        .args(["init", "--initial-branch=main"])
        .current_dir(root)
        .output()
        .expect("git init failed");

    // Set required git identity (CI runners may not have a global config)
    Command::new("git")
        .args(["config", "user.email", "test@test.com"])
        .current_dir(root)
        .output()
        .expect("git config user.email failed");
    Command::new("git")
        .args(["config", "user.name", "Test User"])
        .current_dir(root)
        .output()
        .expect("git config user.name failed");

    // Create initial commit so HEAD is valid
    std::fs::write(root.join("initial.rs"), "// initial file").expect("write failed");
    Command::new("git")
        .args(["add", "."])
        .current_dir(root)
        .output()
        .expect("git add failed");
    Command::new("git")
        .args(["commit", "-m", "initial commit"])
        .current_dir(root)
        .output()
        .expect("git commit failed");

    dir
}

/// Verify that `diff_name_only` correctly identifies modified and added files
/// between two commits.
#[tokio::test]
async fn test_system_git_diff_detects_changed_and_new_files() {
    let repo = git_repo_with_initial_commit();
    let root = repo.path();

    // Make changes: modify the initial file and add a new one
    std::fs::write(root.join("initial.rs"), "// modified").expect("write failed");
    std::fs::write(root.join("added.rs"), "// new file").expect("write failed");
    Command::new("git")
        .args(["add", "."])
        .current_dir(root)
        .output()
        .expect("git add failed");
    Command::new("git")
        .args(["commit", "-m", "second commit"])
        .current_dir(root)
        .output()
        .expect("git commit failed");

    let git = SystemGit;
    let raw = git
        .diff_name_only(root, "HEAD~1")
        .await
        .expect("diff_name_only failed");

    let output = std::str::from_utf8(&raw).expect("non-UTF8 git output");
    let files: Vec<&str> = output.lines().collect();

    assert!(
        files.contains(&"initial.rs"),
        "modified file must appear in diff, files: {:?}",
        files
    );
    assert!(
        files.contains(&"added.rs"),
        "new file must appear in diff, files: {:?}",
        files
    );
}

/// Verify that `diff_name_only` returns empty output when there are no
/// changes between two identical commits (edge case: no-op diff).
#[tokio::test]
async fn test_system_git_diff_empty_when_no_changes() {
    let repo = git_repo_with_initial_commit();
    let root = repo.path();

    // Create a second commit with no file changes (empty commit)
    Command::new("git")
        .args(["commit", "--allow-empty", "-m", "empty commit"])
        .current_dir(root)
        .output()
        .expect("git commit failed");

    let git = SystemGit;
    let raw = git
        .diff_name_only(root, "HEAD~1")
        .await
        .expect("diff_name_only failed");

    let output = std::str::from_utf8(&raw).expect("non-UTF8 git output");
    assert!(
        output.trim().is_empty(),
        "empty commit should produce no diff output, got: {output:?}"
    );
}

/// Verify that `diff_name_only` correctly identifies deleted files.
#[tokio::test]
async fn test_system_git_diff_detects_deleted_files() {
    let repo = git_repo_with_initial_commit();
    let root = repo.path();

    // Delete the initial file and commit
    std::fs::remove_file(root.join("initial.rs")).expect("remove failed");
    Command::new("git")
        .args(["add", "."])
        .current_dir(root)
        .output()
        .expect("git add failed");
    Command::new("git")
        .args(["commit", "-m", "delete file"])
        .current_dir(root)
        .output()
        .expect("git commit failed");

    let git = SystemGit;
    let raw = git
        .diff_name_only(root, "HEAD~1")
        .await
        .expect("diff_name_only failed");

    let output = std::str::from_utf8(&raw).expect("non-UTF8 git output");
    let files: Vec<&str> = output.lines().collect();

    assert!(
        files.contains(&"initial.rs"),
        "deleted file must appear in diff, files: {:?}",
        files
    );
}

/// Verify that `diff_name_only` returns an error when the directory is not
/// a git repository. This exercises the git subprocess error path.
#[tokio::test]
async fn test_system_git_diff_fails_outside_repo() {
    // A tempdir that is NOT a git repo
    let not_a_repo = tempfile::tempdir().expect("failed to create tempdir");

    let git = SystemGit;
    let result = git.diff_name_only(not_a_repo.path(), "HEAD~1").await;

    assert!(
        result.is_err(),
        "diff_name_only must fail outside a git repository"
    );
}
