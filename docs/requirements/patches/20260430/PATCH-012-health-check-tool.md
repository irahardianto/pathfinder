# PATCH-012: Add lsp_health Tool for Upfront LSP Status

## Group: D (Low) — Agent Self-Adaptation

## Objective

Add a lightweight `lsp_health` tool that reports LSP status so agents can adapt their
strategy upfront rather than discovering degradation tool-by-tool. Currently agents must
call navigation tools and check the `degraded` field in each response to determine LSP
status. A single upfront check lets them choose the right tool strategy for the entire session.

## Severity: LOW — convenience tool that improves agent efficiency

## Scope

| # | File | Action |
|---|------|--------|
| 1 | `crates/pathfinder/src/server/types.rs` | Add `LspHealthParams` and `LspHealthResponse` |
| 2 | `crates/pathfinder/src/server/tools/navigation.rs` | Add `lsp_health_impl` |
| 3 | `crates/pathfinder/src/server/mod.rs` | Register the tool in the dispatcher |

## Step 1: Add parameter and response types

**File:** `crates/pathfinder/src/server/types.rs`

Add near the navigation response types:

```rust
/// Parameters for `lsp_health`.
#[derive(Debug, Default, serde::Deserialize, schemars::JsonSchema)]
pub struct LspHealthParams {
    /// Optional language to check (e.g., "rust", "typescript").
    /// If omitted, checks all available languages.
    #[serde(default)]
    pub language: Option<String>,
}

/// The response for `lsp_health`.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct LspHealthResponse {
    /// Overall LSP readiness: `"ready"`, `"warming_up"`, or `"unavailable"`.
    pub status: String,
    /// Per-language status details.
    pub languages: Vec<LspLanguageHealth>,
}

/// Per-language LSP health.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct LspLanguageHealth {
    /// Language ID (e.g., "rust", "typescript").
    pub language: String,
    /// Status: `"ready"`, `"warming_up"`, `"starting"`, or `"unavailable"`.
    pub status: String,
    /// Time since LSP process started (e.g., "45s").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uptime: Option<String>,
}
```

## Step 2: Implement lsp_health_impl

**File:** `crates/pathfinder/src/server/tools/navigation.rs`

Add the implementation:

```rust
    /// Check LSP health status.
    ///
    /// Tests whether LSP navigation tools (`get_definition`, `analyze_impact`,
    /// `read_with_deep_context`) will return real data or degraded results.
    /// Agents should call this once at session start to choose their strategy.
    pub(crate) async fn lsp_health_impl(
        &self,
        params: LspHealthParams,
    ) -> Result<Json<LspHealthResponse>, ErrorData> {
        let capability_status = self.lawyer.capability_status().await;

        let mut languages = Vec::new();
        let mut overall_status = "unavailable";

        for (lang, status) in &capability_status {
            if let Some(ref filter) = params.language {
                if lang != filter {
                    continue;
                }
            }
            let status_str = match status {
                pathfinder_lsp::types::LspLanguageStatus::Ready => {
                    overall_status = "ready";
                    "ready"
                }
                pathfinder_lsp::types::LspLanguageStatus::Starting => {
                    if overall_status != "ready" {
                        overall_status = "warming_up";
                    }
                    "starting"
                }
                pathfinder_lsp::types::LspLanguageStatus::Unavailable => "unavailable",
            };
            languages.push(LspLanguageHealth {
                language: lang.clone(),
                status: status_str.to_owned(),
                uptime: None,
            });
        }

        if languages.is_empty() && params.language.is_none() {
            overall_status = "unavailable";
        }

        Ok(Json(LspHealthResponse {
            status: overall_status.to_owned(),
            languages,
        }))
    }
```

## Step 3: Register the tool

**File:** `crates/pathfinder/src/server/mod.rs`

Add `lsp_health` to the tool dispatcher (the `request_dispatcher` or tool registration
array). Follow the pattern of existing tools like `get_definition`.

## EXCLUSIONS — Do NOT Modify These

- The `Lawyer` trait — `capability_status()` already exists and is used by `get_repo_map`
- Existing tool implementations — no changes needed
- The `LspLanguageStatus` type — already exists in `pathfinder-lsp`

## Verification

```bash
# 1. Confirm lsp_health exists
grep -rn 'lsp_health' crates/pathfinder/src/server/

# 2. Build
cargo build --all

# 3. Test via MCP (manual)
# mcp({ tool: "pathfinder_lsp_health", args: '{}' })

# 4. Full verification
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all
```
