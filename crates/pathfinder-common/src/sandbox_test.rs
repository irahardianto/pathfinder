
use super::*;

/// Build a sandbox with no disk I/O and no user-defined ignore rules.
///
/// Uses `with_user_rules` so tests are completely in-memory and avoid
/// touching the real file system at the hardcoded `/tmp/test` path.
fn default_sandbox() -> Sandbox {
    Sandbox::with_user_rules(
        std::env::temp_dir().as_path(),
        &SandboxConfig::default(),
        None,
    )
}

#[test]
fn test_hardcoded_deny_git_objects() {
    let sandbox = default_sandbox();
    let result = sandbox.check(Path::new(".git/objects/abc123"));
    assert!(result.is_err());
    if let Err(PathfinderError::AccessDenied { tier, .. }) = result {
        assert!(matches!(tier, SandboxTier::HardcodedDeny));
    }
}

#[test]
fn test_hardcoded_deny_pem_file() {
    let sandbox = default_sandbox();
    assert!(sandbox.check(Path::new("certs/server.pem")).is_err());
    assert!(sandbox.check(Path::new("keys/private.key")).is_err());
    assert!(sandbox.check(Path::new("cert.pfx")).is_err());
}

#[test]
fn test_git_allowlist() {
    let sandbox = default_sandbox();
    assert!(sandbox.check(Path::new(".gitignore")).is_ok());
    assert!(sandbox.check(Path::new(".github/workflows/ci.yml")).is_ok());
    assert!(sandbox
        .check(Path::new(".github/actions/custom/action.yml"))
        .is_ok());
}

#[test]
fn test_default_deny_env() {
    let sandbox = default_sandbox();
    assert!(sandbox.check(Path::new(".env")).is_err());
}

#[test]
fn test_default_deny_node_modules() {
    let sandbox = default_sandbox();
    assert!(sandbox
        .check(Path::new("node_modules/express/index.js"))
        .is_err());
}

#[test]
fn test_default_deny_vendor() {
    let sandbox = default_sandbox();
    assert!(sandbox.check(Path::new("vendor/github.com/pkg")).is_err());
}

#[test]
fn test_allow_override() {
    let config = SandboxConfig {
        additional_deny: vec![],
        allow_override: vec![".env".to_owned()],
    };
    let sandbox = Sandbox::with_user_rules(std::env::temp_dir().as_path(), &config, None);
    // .env should now be allowed because it's in allow_override
    assert!(sandbox.check(Path::new(".env")).is_ok());
}

#[test]
fn test_additional_deny() {
    let config = SandboxConfig {
        additional_deny: vec!["*.generated.ts".to_owned()],
        allow_override: vec![],
    };
    let sandbox = Sandbox::with_user_rules(std::env::temp_dir().as_path(), &config, None);
    assert!(sandbox.check(Path::new("src/schema.generated.ts")).is_err());
    // Normal TS files should be fine
    assert!(sandbox.check(Path::new("src/auth.ts")).is_ok());
}

/// Regression test for F1 (audit 2026-03-09-1007):
/// A bare-word `additional_deny` pattern must NOT use substring matching,
/// which would cause `"secret"` to deny `src/secretariat/utils.rs`.
#[test]
fn test_additional_deny_bare_word_does_not_substring_match() {
    let config = SandboxConfig {
        additional_deny: vec!["secret".to_owned()],
        allow_override: vec![],
    };
    let sandbox = Sandbox::with_user_rules(std::env::temp_dir().as_path(), &config, None);

    // A file whose path contains "secret" as a substring but not as a whole
    // filename component must NOT be denied — this was the pre-fix behaviour.
    assert!(
        sandbox.check(Path::new("src/secretariat/utils.rs")).is_ok(),
        "bare-word pattern must not substring-match across path segments"
    );
    // But an exact filename match must still be denied.
    assert!(
        sandbox.check(Path::new("src/secret")).is_err(),
        "bare-word pattern must deny an exact filename match"
    );
}

#[test]
fn test_additional_deny_directory_pattern_no_prefix_leak() {
    // "temp/" should deny "temp/file.txt" but NOT "src/template/file.txt"
    let config = SandboxConfig {
        additional_deny: vec!["temp/".to_owned()],
        allow_override: vec![],
    };
    let sandbox = Sandbox::with_user_rules(std::env::temp_dir().as_path(), &config, None);

    assert!(
        sandbox.check(Path::new("temp/scratch.txt")).is_err(),
        "temp/ pattern must deny paths starting with temp/"
    );
    assert!(
        sandbox.check(Path::new("src/template/index.ts")).is_ok(),
        "temp/ pattern must not deny src/template/ (prefix leak)"
    );
}

#[test]
fn test_normal_source_files_allowed() {
    let sandbox = default_sandbox();
    assert!(sandbox.check(Path::new("src/main.rs")).is_ok());
    assert!(sandbox.check(Path::new("src/auth.ts")).is_ok());
    assert!(sandbox.check(Path::new("README.md")).is_ok());
    assert!(sandbox.check(Path::new("Cargo.toml")).is_ok());
}

#[test]
fn test_hardcoded_deny_cannot_be_overridden() {
    let config = SandboxConfig {
        additional_deny: vec![],
        allow_override: vec![".git/objects/".to_owned()],
    };
    let sandbox = Sandbox::with_user_rules(std::env::temp_dir().as_path(), &config, None);
    // Hardcoded deny cannot be overridden by allow_override
    assert!(sandbox.check(Path::new(".git/objects/abc")).is_err());
}

// ── Pure in-memory testability ────────────────────────────────────────────
// These tests use `with_user_rules` to exercise the full sandbox logic
// without any disk I/O — no `.pathfinderignore` on disk needed.

#[test]
fn test_with_user_rules_none_skips_tier3() {
    // No user-defined rules: Tier 3 always passes.
    let sandbox = Sandbox::with_user_rules(
        std::env::temp_dir().as_path(),
        &SandboxConfig::default(),
        None,
    );
    // A path that would be caught only by .pathfinderignore — must pass.
    assert!(sandbox.check(Path::new("some/custom/path.txt")).is_ok());
}

#[test]
fn test_with_user_rules_injected_ignore() {
    // Build a Gitignore rule set in memory (workspace at temp_dir, no on-disk file needed).
    let workspace = std::env::temp_dir();
    let mut builder = GitignoreBuilder::new(&workspace);
    // Add a rule without a backing file — GitignoreBuilder::add_line is available.
    builder
        .add_line(None, "blocked_by_user.txt")
        .expect("valid pattern");
    let gitignore = builder.build().expect("valid gitignore");

    let sandbox = Sandbox::with_user_rules(
        workspace.as_path(),
        &SandboxConfig::default(),
        Some(gitignore),
    );
    // The injected rule blocks the path.
    assert!(sandbox.check(Path::new("blocked_by_user.txt")).is_err());
    // Other paths are unaffected.
    assert!(sandbox.check(Path::new("src/main.rs")).is_ok());
}

#[test]
fn test_same_workspace_absolute_path_allowed() {
    // Create a sandbox with a specific workspace root
    let workspace = std::env::temp_dir();
    let sandbox = Sandbox::with_user_rules(workspace.as_path(), &SandboxConfig::default(), None);

    // Same-workspace absolute path should be allowed (normalized to relative)
    let abs_path = workspace.join("src/main.rs");
    assert!(
        sandbox.check(&abs_path).is_ok(),
        "same-workspace absolute path should be allowed"
    );

    // Relative path should still work
    assert!(sandbox.check(Path::new("src/main.rs")).is_ok());
}

#[test]
fn test_cross_workspace_absolute_path_denied() {
    // Create a sandbox with one workspace root
    let workspace1 = std::env::temp_dir().join("workspace1");
    let sandbox = Sandbox::with_user_rules(&workspace1, &SandboxConfig::default(), None);

    // Cross-workspace absolute path should be denied
    let workspace2 = std::env::temp_dir().join("workspace2");
    let cross_workspace_path = workspace2.join("src/main.rs");
    assert!(
        sandbox.check(&cross_workspace_path).is_err(),
        "cross-workspace absolute path should be denied"
    );
}

// ── Sandbox::new disk I/O tests ─────────────────────────────────────────
//
// These tests exercise `Sandbox::new`, which reads `.pathfinderignore` from
// the workspace root on disk.  They use `tempfile::tempdir()` so they are
// completely isolated and clean up automatically on drop.
//
// Future agents: add more `.pathfinderignore` pattern tests here; each
// scenario should use a fresh `tempdir` to avoid cross-test contamination.

#[test]
fn test_new_loads_pathfinderignore_from_disk() {
    // Create a real temporary workspace directory.
    let workspace = tempfile::tempdir().expect("failed to create tempdir");
    let root = workspace.path();

    // Write a .pathfinderignore that blocks "secrets.txt".
    std::fs::write(root.join(".pathfinderignore"), "secrets.txt\n")
        .expect("failed to write .pathfinderignore");

    let sandbox = Sandbox::new(root, &SandboxConfig::default());

    // The file listed in .pathfinderignore must be blocked (Tier 3).
    assert!(
        sandbox.check(Path::new("secrets.txt")).is_err(),
        "secrets.txt should be denied by .pathfinderignore"
    );

    // An unlisted file must still be accessible.
    assert!(
        sandbox.check(Path::new("src/main.rs")).is_ok(),
        "src/main.rs should be allowed"
    );
}

#[test]
fn test_new_without_pathfinderignore_allows_normal_files() {
    // Workspace with NO .pathfinderignore — Tier 3 must be absent.
    let workspace = tempfile::tempdir().expect("failed to create tempdir");
    let root = workspace.path();

    // Sanity: no .pathfinderignore file exists.
    assert!(!root.join(".pathfinderignore").exists());

    let sandbox = Sandbox::new(root, &SandboxConfig::default());

    // Normal source files should pass without a .pathfinderignore.
    assert!(sandbox.check(Path::new("src/lib.rs")).is_ok());
    assert!(sandbox.check(Path::new("README.md")).is_ok());
}

#[test]
fn test_new_with_directory_glob_in_pathfinderignore() {
    // Verify that glob patterns in .pathfinderignore work correctly via
    // the `ignore` crate's gitignore-style parser.
    //
    // NOTE on gitignore semantics: a trailing-slash pattern like `private/`
    // matches the *directory entry* itself, not files inside it (is_dir check).
    // To deny files *within* a directory, use `private/**` instead.
    // This test uses `private/**` to match what users actually want.
    let workspace = tempfile::tempdir().expect("failed to create tempdir");
    let root = workspace.path();

    // Block all files inside `private/` using a glob wildcard.
    std::fs::write(root.join(".pathfinderignore"), "private/**\n")
        .expect("failed to write .pathfinderignore");

    let sandbox = Sandbox::new(root, &SandboxConfig::default());

    assert!(
        sandbox.check(Path::new("private/config.toml")).is_err(),
        "file inside private/ must be denied by private/** pattern"
    );
    assert!(
        sandbox.check(Path::new("public/api.rs")).is_ok(),
        "file outside private/ must be allowed"
    );
}

// ── Path traversal guard (L137-L146) ────────────────────────────────────

#[test]
fn test_path_traversal_parent_dir_denied() {
    let sandbox = default_sandbox();
    // `../etc/passwd` contains a `ParentDir` component — must be denied
    // at the hardcoded-deny tier (before Tier-1 pattern checks).
    let result = sandbox.check(Path::new("../etc/passwd"));
    assert!(
        result.is_err(),
        "path traversal with '..' must be denied by the hardcoded guard"
    );
    let Err(PathfinderError::AccessDenied { tier, .. }) = result else {
        panic!("expected AccessDenied");
    };
    assert!(
        matches!(tier, SandboxTier::HardcodedDeny),
        "traversal must be SandboxTier::HardcodedDeny"
    );
}

#[test]
fn test_nested_path_traversal_denied() {
    let sandbox = default_sandbox();
    // A traversal buried deeper in the path is equally dangerous.
    let result = sandbox.check(Path::new("src/../../etc/shadow"));
    assert!(
        result.is_err(),
        "nested '..' traversal must be denied by the hardcoded guard"
    );
}

// ── matches_wildcard_pattern no-star arm (L109-L110) ────────────────────

#[test]
fn test_wildcard_pattern_without_trailing_star_returns_false() {
    // Pattern has no trailing `*` — `strip_suffix('*')` returns None.
    // The `else { return false; }` arm must be exercised.
    //
    // We use `is_additional_denied` indirectly via `check()` by injecting an
    // `additional_deny` config with no `*.` prefix, no trailing `/`,
    // and no trailing `*` — making it hit the exact-pattern branch, NOT the
    // wildcard branch. To directly call the private function we use it from
    // within the same module.
    assert!(
        !Sandbox::matches_wildcard_pattern("no-star-pattern", "anything.txt"),
        "a pattern without a trailing '*' must return false"
    );
}

#[test]
fn test_wildcard_pattern_with_star_matches_prefix() {
    // Confirm the positive arm: `.env.*` should match `.env.local`.
    assert!(
        Sandbox::matches_wildcard_pattern(".env.*", ".env.local"),
        ".env.* pattern must match .env.local"
    );
}

#[test]
fn test_wildcard_pattern_with_star_no_match() {
    // `.env.*` must NOT match `secrets.txt` (different prefix).
    assert!(
        !Sandbox::matches_wildcard_pattern(".env.*", "secrets.txt"),
        ".env.* must not match secrets.txt"
    );
}

/// Validate that all hardcoded deny patterns start with ".git/".
/// If this assumption changes, `is_hardcoded_denied` must be updated.
#[test]
fn test_hardcoded_deny_patterns_all_start_with_git() {
    for pattern in HARDCODED_DENY_PATTERNS {
        assert!(
            pattern.starts_with(".git/"),
            "HARDCODED_DENY_PATTERNS contains non-git pattern: {pattern}. \
                 Update is_hardcoded_denied() to handle it."
        );
    }
}

#[test]
fn test_sandbox_check_performance_and_prefix_matching() {
    let sandbox = default_sandbox();
    assert!(sandbox.check(Path::new(".gitignore")).is_ok());
    assert!(sandbox.check(Path::new(".gitattributes")).is_ok());
    assert!(sandbox.check(Path::new(".gitignorex")).is_ok());
    assert!(sandbox.check(Path::new(".git/config")).is_err());

    let path = Path::new(".gitignore");
    for _ in 0..10000 {
        let _ = sandbox.check(path);
    }
}
