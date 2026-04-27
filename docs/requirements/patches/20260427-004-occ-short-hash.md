# PATCH-004: OCC Short Hash Support (Git-Style 7-Char Prefix)

**Status:** Completed  
**Priority:** P2 — Medium (ergonomic improvement, reduces LLM token friction)  
**Estimated Effort:** 1–2 hours  
**Prerequisite:** None (fully independent)  
**PR Strategy:** Standalone PR — single function change with backward-compatible semantics

---

## Problem Statement

Every `version_hash` produced by Pathfinder is a full SHA-256 hex digest:

```
sha256:4ec5a8ada8dd9ebfd69d14aecc6955a51284775b3cc770e2d009fa55f9d4b5aa
```

That is `sha256:` (7 chars) + 64 hex chars = **71 characters** per hash.

In a typical agent editing session, 5–15 hashes appear in responses and must be copied into
subsequent tool calls as `base_version` arguments. At 71 chars each, this is a non-trivial
fraction of the LLM's context window usage for what is mechanically just a state-threading token.

Git has solved this ergonomically: the standard `git log --oneline` shows 7-char prefixes.
These are universally understood as uniquely identifying a commit within any reasonably-sized
repository. The same logic applies here.

### Collision safety analysis

| Prefix length | Unique values | P(collision in 1000-file workspace) |
|---|---|---|
| 7 hex chars (28 bits) | ~268 million | ~0.000004% |
| 8 hex chars (32 bits) | ~4.3 billion | <0.0000003% |
| 64 hex chars (full) | 2^256 | effectively zero |

For a local MCP server serving a single developer workspace (< 10,000 files), 7 hex chars
provides mathematically sufficient uniqueness. Git uses it at scale. This is fine.

---

## Proposed Change

### `crates/pathfinder/src/server/helpers.rs`

Update `check_occ` to accept both full hashes and 7-char (or longer) prefixes:

```rust
/// OCC guard: verify the agent's `base_version` matches the current file hash.
///
/// Accepts two formats:
/// - Full hash: `sha256:<64 hex chars>` — exact match (legacy, always valid)
/// - Short prefix: `sha256:<N hex chars>` where N ≥ 7 — prefix match
///
/// The minimum prefix length of 7 characters matches Git's convention and
/// provides sufficient collision resistance for workspace-scale file sets.
/// A prefix shorter than 7 hex chars after `sha256:` is rejected as too short.
pub(crate) fn check_occ(
    base_version: &str,
    current_hash: &VersionHash,
    path: PathBuf,
) -> Result<(), ErrorData> {
    const SHA256_PREFIX: &str = "sha256:";
    const MIN_HEX_CHARS: usize = 7;

    let current = current_hash.as_str();

    let matches = if base_version.len() == current.len() {
        // Fast path: same length → exact comparison
        base_version == current
    } else if base_version.len() > current.len() {
        // Claimed hash is longer than current (malformed or different algo)
        false
    } else {
        // base_version is shorter — attempt prefix match
        let hex_part_len = base_version
            .strip_prefix(SHA256_PREFIX)
            .map(|h| h.len())
            .unwrap_or(0);

        if hex_part_len < MIN_HEX_CHARS {
            // Too short to be meaningful — treat as mismatch to be safe
            return Err(pathfinder_to_error_data(
                &PathfinderError::VersionMismatch {
                    path,
                    current_version_hash: current.to_owned(),
                    lines_changed: None,
                },
            ));
        }

        current.starts_with(base_version)
    };

    if !matches {
        return Err(pathfinder_to_error_data(
            &PathfinderError::VersionMismatch {
                path,
                current_version_hash: current.to_owned(),
                lines_changed: None,
            },
        ));
    }
    Ok(())
}
```

---

## Tool Description Update

Add to the description of **all tools that accept `base_version`**:

```
// Add to description:
"base_version accepts either the full SHA-256 hash (e.g., \"sha256:4ec5a8a...\") 
or a short 7-character prefix (e.g., \"sha256:4ec5a8a\"), matching Git convention."
```

Affected tools: `replace_body`, `replace_full`, `insert_before`, `insert_after`,
`insert_into`, `delete_symbol`, `validate_only`, `create_file`, `delete_file`,
`write_file`, `replace_batch`.

---

## Implementation Steps

1. **Update `check_occ`** in `helpers.rs` — add prefix match logic
2. **Update tool descriptions** — add short hash note to all `base_version` parameters
3. **Add tests** (see below)
4. **Verify:** `cargo test --workspace`, `cargo clippy`, `cargo fmt --check`

---

## Test Plan

New tests in `crates/pathfinder/src/server/helpers.rs` (inline `#[cfg(test)]`):

```rust
/// PATCH-004-T1: Full hash — exact match passes
#[test]
fn test_check_occ_full_hash_match() {
    let hash = VersionHash::compute(b"hello world");
    let result = check_occ(hash.as_str(), &hash, PathBuf::from("test.rs"));
    assert!(result.is_ok());
}

/// PATCH-004-T2: Short 7-char prefix — passes when correct
#[test]
fn test_check_occ_short_7_char_prefix_matches() {
    let hash = VersionHash::compute(b"hello world");
    // Take first 7 hex chars after "sha256:"
    let short = &hash.as_str()[..14]; // "sha256:" (7) + 7 hex chars = 14
    let result = check_occ(short, &hash, PathBuf::from("test.rs"));
    assert!(result.is_ok(), "7-char prefix should be accepted");
}

/// PATCH-004-T3: Short prefix — wrong prefix fails
#[test]
fn test_check_occ_wrong_prefix_fails() {
    let hash = VersionHash::compute(b"hello world");
    let result = check_occ("sha256:0000000", &hash, PathBuf::from("test.rs"));
    assert!(result.is_err(), "wrong prefix must fail");
}

/// PATCH-004-T4: Prefix shorter than 7 hex chars is rejected
#[test]
fn test_check_occ_prefix_too_short_is_rejected() {
    let hash = VersionHash::compute(b"hello world");
    let result = check_occ("sha256:4ec", &hash, PathBuf::from("test.rs")); // only 3 hex chars
    assert!(result.is_err(), "prefix < 7 hex chars must be rejected");
}

/// PATCH-004-T5: Full hash — mismatch fails (regression)
#[test]
fn test_check_occ_full_hash_mismatch_fails() {
    let hash_a = VersionHash::compute(b"hello world");
    let hash_b = VersionHash::compute(b"different content");
    let result = check_occ(hash_a.as_str(), &hash_b, PathBuf::from("test.rs"));
    assert!(result.is_err());
}

/// PATCH-004-T6: 8-char prefix also works (longer than minimum)
#[test]
fn test_check_occ_8_char_prefix_matches() {
    let hash = VersionHash::compute(b"hello world");
    let prefix_8 = &hash.as_str()[..15]; // "sha256:" + 8 hex chars
    let result = check_occ(prefix_8, &hash, PathBuf::from("test.rs"));
    assert!(result.is_ok(), "8-char prefix should also be accepted");
}
```

---

## Acceptance Criteria

- [ ] `base_version: "sha256:4ec5a8a"` (7 hex chars) accepted when it matches the full hash prefix
- [ ] `base_version: "sha256:000"` (3 hex chars — too short) returns `VERSION_MISMATCH`
- [ ] `base_version: "<full 71-char hash>"` continues to work (backward compatibility)
- [ ] Wrong prefix still returns `VERSION_MISMATCH`
- [ ] All 6 new tests pass
- [ ] `cargo test --workspace` passes with zero regressions
- [ ] Tool descriptions updated in at least `replace_body`, `insert_before`, `insert_after`,
  `insert_into`, `replace_batch`
