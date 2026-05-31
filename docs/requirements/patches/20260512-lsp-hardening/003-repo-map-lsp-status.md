# 003: Surface LSP Status in `get_repo_map`

**Epic**: 1 — Quick Wins
**Status**: ✅ Complete (2026-05-12)
**Severity**: Low
**Risk**: Low — additive field, no change to existing response shape

---

## Problem

`get_repo_map` returns a structural skeleton of the project with technology detection, but no indication of whether LSP tools (`get_definition`, `analyze_impact`, `read_with_deep_context`) are ready for each detected language. Agents must make a separate `lsp_health` call to check readiness, adding unnecessary latency and ceremony.

The existing `capabilities.lsp.per_language` field contains the full `LspLanguageStatus` struct per language, but its nested structure requires agents to traverse `capabilities.lsp.per_language.<lang>.navigation_ready` — which is verbose and easy to misparse.

---

## Solution

Added a flat `lsp_status` map to `GetRepoMapMetadata` that provides a one-level `language → status_string` lookup:

```json
{
  "lsp_status": {
    "rust": "ready",
    "typescript": "warming_up",
    "python": "unavailable"
  }
}
```

### Status Derivation Logic

The `derive_lsp_status()` helper converts `LspLanguageStatus` into a status string using the same two-phase readiness model as `lsp_health_impl`:

| Condition | Status | Meaning |
|-----------|--------|---------|
| `navigation_ready == Some(true)` | `"ready"` | Navigation tools (get_definition, analyze_impact) are functional |
| `navigation_ready == Some(false)` OR `indexing_complete == Some(false)` | `"warming_up"` | LSP connected, capabilities reported, still indexing |
| `uptime_seconds.is_some()` but no capability data yet | `"starting"` | LSP process running but hasn't reported any capabilities (very early state) |
| Neither | `"unavailable"` | No LSP process for this language |

The field is `Option<HashMap<String, String>>` and serialized as `skip_serializing_if = "Option::is_none"` — absent from JSON when no LSP processes are running.

### Files Modified

| File | Change |
|------|--------|
| `crates/pathfinder/src/server/types.rs` | Added `lsp_status: Option<HashMap<String, String>>` to `GetRepoMapMetadata` |
| `crates/pathfinder/src/server/tools/repo_map.rs` | Added `derive_lsp_status()` helper; populated field in both `get_repo_map_impl` and `empty_changes_response` |

---

## Acceptance Criteria

- [x] `lsp_status` absent from JSON when no LSP processes are running
- [x] `lsp_status` present with `"ready"` for languages where `navigation_ready == Some(true)`
- [x] `lsp_status` present with `"warming_up"` for languages with capabilities reported but not navigation-ready
- [x] `lsp_status` present with `"starting"` for languages with uptime but no capability data yet
- [x] `lsp_status` present with `"unavailable"` for languages with no uptime or navigation readiness
- [x] Populated in both the main response path and the `empty_changes_response` (changed_since with no diffs)
- [x] Does not duplicate data — mirrors `capabilities.lsp.per_language` in a flat format
- [x] Status strings match `lsp_health` tool conventions for consistency

---

## Tests

| Test | File | Description |
|------|------|-------------|
| `test_derive_lsp_status_empty_map_returns_none` | `repo_map.rs` | Empty capability map → `None` |
| `test_derive_lsp_status_correct_status_strings` | `repo_map.rs` | 5-language map with ready/warming_up/starting/unavailable → correct strings |

---

## Verification

```bash
cargo test -p pathfinder-mcp -- tools::repo_map::tests::test_derive_lsp_status
# 2 passed, 0 failed

cargo clippy -p pathfinder-mcp -- -D warnings
# 0 warnings
```

---

## Agent Usage Example

```
# Agent calls get_repo_map at session start
response = get_repo_map(path=".")

# Instantly check if LSP is ready — no extra lsp_health call needed
if response.metadata.lsp_status.get("rust") == "ready":
    # Safe to call get_definition, analyze_impact
    ...
elif response.metadata.lsp_status.get("rust") == "warming_up":
    # LSP connected but still indexing — grep fallbacks will be used automatically
    # Agent can proceed, or wait and re-check
    ...
elif response.metadata.lsp_status.get("rust") == "starting":
    # LSP process running but no capabilities reported yet — very early state
    # Agent should wait and re-check, or proceed with grep fallbacks
    ...
```
