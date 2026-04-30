# PATCH-014: Surface did_you_mean and current_version_hash in MCP Errors

## Group: D (Low) — Agent Self-Adaptation

## Objective

Fix the MCP error message strip-mining problem. `PathfinderError::SymbolNotFound` generates
`did_you_mean` suggestions internally, and `VersionMismatch` includes `current_version_hash`,
but these structured fields are lost when converted to MCP's `ErrorData` format. Agents see
generic "SYMBOL_NOT_FOUND" with no recovery path.

## Severity: MEDIUM — agents hit dead ends without recovery hints

## Background

The MCP `ErrorData` struct has a `message: String` field. Structured data from
`PathfinderError` variants must be serialized into this string. The current code likely
calls `error.to_string()` or a similar method that drops the structured fields.

The fix: serialize structured error data into the `message` field as JSON, or add it
to the `data` field of `ErrorData`.

## Scope

| # | File | Action |
|---|------|--------|
| 1 | `crates/pathfinder/src/server/helpers.rs` | Update `pathfinder_to_error_data` to serialize structured fields |
| 2 | `crates/pathfinder-common/src/error.rs` | Add `to_json_message` method to `PathfinderError` |

## Step 1: Add structured error serialization

**File:** `crates/pathfinder-common/src/error.rs`

Find the `PathfinderError` enum and add a method that serializes error data as JSON:

```rust
impl PathfinderError {
    /// Serialize the error's structured data into a JSON string suitable for
    /// MCP ErrorData.message. This ensures agents can parse recovery information
    /// like `did_you_mean` suggestions and `current_version_hash`.
    pub fn to_json_message(&self) -> String {
        match self {
            Self::SymbolNotFound { semantic_path, did_you_mean } => {
                let mut obj = serde_json::Map::new();
                obj.insert("code".to_owned(), serde_json::Value::String("SYMBOL_NOT_FOUND".to_owned()));
                obj.insert("semantic_path".to_owned(), serde_json::Value::String(semantic_path.clone()));
                if !did_you_mean.is_empty() {
                    obj.insert(
                        "did_you_mean".to_owned(),
                        serde_json::Value::Array(
                            did_you_mean.iter().map(|s| serde_json::Value::String(s.clone())).collect()
                        ),
                    );
                }
                obj.insert("hint".to_owned(), serde_json::Value::String(
                    "Use read_source_file to see available symbols.".to_owned()
                ));
                serde_json::Value::Object(obj).to_string()
            }
            Self::VersionMismatch { path, expected_version, current_version, .. } => {
                let mut obj = serde_json::Map::new();
                obj.insert("code".to_owned(), serde_json::Value::String("VERSION_MISMATCH".to_owned()));
                obj.insert("path".to_owned(), serde_json::Value::String(path.to_string_lossy().to_string()));
                obj.insert("expected_version".to_owned(), serde_json::Value::String(expected_version.clone()));
                obj.insert("current_version".to_owned(), serde_json::Value::String(current_version.clone()));
                obj.insert("hint".to_owned(), serde_json::Value::String(
                    "Re-read the file to get the current version hash, then retry.".to_owned()
                ));
                serde_json::Value::Object(obj).to_string()
            }
            _ => self.to_string(),
        }
    }
}
```

## Step 2: Use to_json_message in pathfinder_to_error_data

**File:** `crates/pathfinder/src/server/helpers.rs`

**Find:**
```rust
pub fn pathfinder_to_error_data(err: &PathfinderError) -> ErrorData {
```

Update the function to use `to_json_message` for the `message` field:

```rust
pub fn pathfinder_to_error_data(err: &PathfinderError) -> ErrorData {
    let code = err.error_code(); // e.g., -32001
    let message = err.to_json_message(); // structured JSON instead of plain string
    ErrorData::new(code.into(), message, None)
}
```

Note: The `error_code()` method must already exist (it was added in prior patches).
If it doesn't, use a match on `PathfinderError` variants to assign appropriate
MCP error codes.

## EXCLUSIONS — Do NOT Modify These

- The `PathfinderError` enum variants — no new variants needed
- Other error types (io errors, tree-sitter errors) — those already have adequate messages
- The `rmcp` framework's `ErrorData` struct — it supports arbitrary string messages

## Verification

```bash
# 1. Confirm to_json_message exists
grep -n 'to_json_message' crates/pathfinder-common/src/error.rs

# 2. Confirm pathfinder_to_error_data uses it
grep -n 'to_json_message' crates/pathfinder/src/server/helpers.rs

# 3. Run error handling tests
cargo test -p pathfinder error

# 4. Full verification
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all
```

## Expected Impact

Before:
```
Error: SYMBOL_NOT_FOUND
```

After:
```json
{"code":"SYMBOL_NOT_FOUND","semantic_path":"indent.rs::dedant","did_you_mean":["dedent","indent"],"hint":"Use read_source_file to see available symbols."}
```

Agents can parse this JSON and immediately try the suggested alternative, saving
a round-trip to `read_source_file`.
