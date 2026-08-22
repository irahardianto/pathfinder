//! Position-aware canned index for `test-mock-lsp` (`LSP-Z2`).
//!
//! Enables the mock LSP server to return real, non-empty, position-accurate
//! responses for fixture repositories:
//! - `textDocument/definition`
//! - `textDocument/references`
//! - `textDocument/prepareCallHierarchy`
//! - `callHierarchy/incomingCalls`
//! - `callHierarchy/outgoingCalls`
//! - `textDocument/implementation`
//!
//! # Schema
//!
//! `{ uri -> { "(line, char)" -> PositionEntry } }`

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

/// An entry in the canned index keyed by line and character position.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PositionEntry {
    /// `textDocument/definition` response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub definition: Option<Value>,

    /// `textDocument/references` response (array of Locations).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub references: Option<Value>,

    /// `textDocument/prepareCallHierarchy` response (`CallHierarchyItem` or array of them).
    #[serde(
        default,
        alias = "prepare_call_hierarchy",
        skip_serializing_if = "Option::is_none"
    )]
    pub prepare_call_hierarchy: Option<Value>,

    /// `callHierarchy/incomingCalls` response (array of `CallHierarchyIncomingCall`).
    #[serde(
        default,
        alias = "incoming_calls",
        skip_serializing_if = "Option::is_none"
    )]
    pub incoming_calls: Option<Value>,

    /// `callHierarchy/outgoingCalls` response (array of `CallHierarchyOutgoingCall`).
    #[serde(
        default,
        alias = "outgoing_calls",
        skip_serializing_if = "Option::is_none"
    )]
    pub outgoing_calls: Option<Value>,

    /// `textDocument/implementation` response (Location, LocationLink, or array).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub implementation: Option<Value>,
}

/// A position key represented as (0-indexed line, 0-indexed character).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PositionKey {
    /// 0-indexed line number.
    pub line: u32,
    /// 0-indexed character offset.
    pub character: u32,
}

impl PositionKey {
    /// Create a new `PositionKey`.
    #[must_use]
    pub const fn new(line: u32, character: u32) -> Self {
        Self { line, character }
    }

    /// Parse a position string or value.
    ///
    /// Accepts:
    /// - `"(10, 5)"` or `"(10,5)"`
    /// - `"10:5"` or `"10,5"`
    /// - `[10, 5]`
    /// - `{"line": 10, "character": 5}`
    #[must_use]
    pub fn parse(val: &Value) -> Option<Self> {
        match val {
            Value::String(s) => Self::from_str_spec(s),
            Value::Array(arr) if arr.len() >= 2 => {
                let line = arr[0].as_u64()? as u32;
                let char_col = arr[1].as_u64()? as u32;
                Some(Self::new(line, char_col))
            }
            Value::Object(map) => {
                let line = map.get("line")?.as_u64()? as u32;
                let char_col = map.get("character").or_else(|| map.get("char"))?.as_u64()? as u32;
                Some(Self::new(line, char_col))
            }
            _ => None,
        }
    }

    fn from_str_spec(s: &str) -> Option<Self> {
        let cleaned = s.trim().trim_start_matches('(').trim_end_matches(')');
        let delimiter = if cleaned.contains(':') {
            ':'
        } else if cleaned.contains(',') {
            ','
        } else {
            ' '
        };

        let parts: Vec<&str> = cleaned.split(delimiter).collect();
        if parts.len() >= 2 {
            let line = parts[0].trim().parse::<u32>().ok()?;
            let char_col = parts[1].trim().parse::<u32>().ok()?;
            Some(Self::new(line, char_col))
        } else {
            None
        }
    }
}

/// A position-aware canned index of LSP responses.
#[derive(Debug, Clone, Default)]
pub struct CannedIndex {
    /// Maps URI string to a map of position -> `PositionEntry`.
    entries: HashMap<String, HashMap<PositionKey, PositionEntry>>,
}

impl CannedIndex {
    /// Create an empty `CannedIndex`.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Parse a JSON string or file contents into a `CannedIndex`.
    ///
    /// # Errors
    /// Returns an error message if JSON parsing fails.
    pub fn from_json_str(json_str: &str) -> Result<Self, String> {
        let val: Value = serde_json::from_str(json_str)
            .map_err(|e| format!("invalid canned index JSON: {e}"))?;
        Self::from_value(&val)
    }

    /// Construct a `CannedIndex` from a `serde_json::Value`.
    ///
    /// # Errors
    /// Returns an error message if the JSON structure is unexpected.
    pub fn from_value(val: &Value) -> Result<Self, String> {
        let root = val.as_object().ok_or_else(|| {
            "canned index root must be a JSON object of uri -> positions".to_string()
        })?;

        let mut index = Self::new();

        for (uri, pos_map_val) in root {
            let pos_obj = match pos_map_val.as_object() {
                Some(obj) => obj,
                None => continue,
            };

            for (pos_key_str, entry_val) in pos_obj {
                if let Some(pos_key) = PositionKey::from_str_spec(pos_key_str) {
                    let entry: PositionEntry =
                        serde_json::from_value(entry_val.clone()).map_err(|e| {
                            format!(
                                "failed to parse position entry for {uri} at {pos_key_str}: {e}"
                            )
                        })?;
                    index.insert(uri, pos_key.line, pos_key.character, entry);
                }
            }
        }

        Ok(index)
    }

    /// Insert an entry for `(uri, line, character)`.
    pub fn insert(&mut self, uri: &str, line: u32, character: u32, entry: PositionEntry) {
        let uri_map = self.entries.entry(uri.to_string()).or_default();
        uri_map.insert(PositionKey::new(line, character), entry);
    }

    /// Find the position map for a given query URI.
    ///
    /// Handles exact URI matches, relative path matching, and suffix matching.
    fn find_uri_entries(&self, query_uri: &str) -> Option<&HashMap<PositionKey, PositionEntry>> {
        // 1. Direct exact match
        if let Some(map) = self.entries.get(query_uri) {
            return Some(map);
        }

        // 2. Suffix / relative path matching
        let clean_query = query_uri.trim_start_matches("file://");
        for (indexed_uri, map) in &self.entries {
            let clean_indexed = indexed_uri.trim_start_matches("file://");
            if clean_query.ends_with(clean_indexed) || clean_indexed.ends_with(clean_query) {
                return Some(map);
            }
        }

        None
    }

    /// Look up a `PositionEntry` for `(uri, line, character)`.
    #[must_use]
    pub fn lookup(&self, uri: &str, line: u32, character: u32) -> Option<&PositionEntry> {
        let uri_map = self.find_uri_entries(uri)?;
        uri_map.get(&PositionKey::new(line, character))
    }

    /// Look up `textDocument/definition` for `(uri, line, character)`.
    #[must_use]
    pub fn lookup_definition(
        &self,
        uri: &str,
        line: u32,
        character: u32,
        ws_root: Option<&str>,
    ) -> Option<Value> {
        let entry = self.lookup(uri, line, character)?;
        entry
            .definition
            .as_ref()
            .map(|val| interpolate_workspace(val, ws_root))
    }

    /// Look up `textDocument/references` for `(uri, line, character)`.
    #[must_use]
    pub fn lookup_references(
        &self,
        uri: &str,
        line: u32,
        character: u32,
        ws_root: Option<&str>,
    ) -> Option<Value> {
        let entry = self.lookup(uri, line, character)?;
        entry
            .references
            .as_ref()
            .map(|val| interpolate_workspace(val, ws_root))
    }

    /// Look up `textDocument/prepareCallHierarchy` for `(uri, line, character)`.
    #[must_use]
    pub fn lookup_prepare_call_hierarchy(
        &self,
        uri: &str,
        line: u32,
        character: u32,
        ws_root: Option<&str>,
    ) -> Option<Value> {
        let entry = self.lookup(uri, line, character)?;
        entry
            .prepare_call_hierarchy
            .as_ref()
            .map(|val| interpolate_workspace(val, ws_root))
    }

    /// Look up `callHierarchy/incomingCalls` for `(uri, line, character)`.
    #[must_use]
    pub fn lookup_incoming_calls(
        &self,
        uri: &str,
        line: u32,
        character: u32,
        ws_root: Option<&str>,
    ) -> Option<Value> {
        let entry = self.lookup(uri, line, character)?;
        entry
            .incoming_calls
            .as_ref()
            .map(|val| interpolate_workspace(val, ws_root))
    }

    /// Look up `callHierarchy/outgoingCalls` for `(uri, line, character)`.
    #[must_use]
    pub fn lookup_outgoing_calls(
        &self,
        uri: &str,
        line: u32,
        character: u32,
        ws_root: Option<&str>,
    ) -> Option<Value> {
        let entry = self.lookup(uri, line, character)?;
        entry
            .outgoing_calls
            .as_ref()
            .map(|val| interpolate_workspace(val, ws_root))
    }

    /// Look up `textDocument/implementation` for `(uri, line, character)`.
    #[must_use]
    pub fn lookup_implementation(
        &self,
        uri: &str,
        line: u32,
        character: u32,
        ws_root: Option<&str>,
    ) -> Option<Value> {
        let entry = self.lookup(uri, line, character)?;
        entry
            .implementation
            .as_ref()
            .map(|val| interpolate_workspace(val, ws_root))
    }
}

/// Helper function to interpolate `${WORKSPACE}` or `$WORKSPACE_ROOT` placeholders in returned URIs.
fn interpolate_workspace(val: &Value, ws_root: Option<&str>) -> Value {
    let ws = match ws_root {
        Some(w) => w.trim_end_matches('/'),
        None => return val.clone(),
    };

    match val {
        Value::String(s) => {
            let replaced = s
                .replace("${WORKSPACE}", ws)
                .replace("$WORKSPACE_ROOT", ws)
                .replace("file:///mock/workspace", &format!("file://{ws}"));
            Value::String(replaced)
        }
        Value::Array(arr) => Value::Array(
            arr.iter()
                .map(|item| interpolate_workspace(item, ws_root))
                .collect(),
        ),
        Value::Object(obj) => {
            let mut map = serde_json::Map::new();
            for (k, v) in obj {
                map.insert(k.clone(), interpolate_workspace(v, ws_root));
            }
            Value::Object(map)
        }
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_canned_index_lookup_and_workspace_interpolation() {
        let raw_json = json!({
            "file:///mock/workspace/src/lib.rs": {
                "(10, 5)": {
                    "definition": {
                        "uri": "${WORKSPACE}/src/lib.rs",
                        "range": {
                            "start": { "line": 10, "character": 5 },
                            "end": { "line": 10, "character": 12 }
                        }
                    },
                    "references": [
                        {
                            "uri": "${WORKSPACE}/src/main.rs",
                            "range": { "start": { "line": 2, "character": 4 }, "end": { "line": 2, "character": 11 } }
                        }
                    ],
                    "prepare_call_hierarchy": [
                        {
                            "name": "login",
                            "kind": 12,
                            "uri": "${WORKSPACE}/src/lib.rs",
                            "range": { "start": { "line": 10, "character": 0 }, "end": { "line": 15, "character": 1 } },
                            "selectionRange": { "start": { "line": 10, "character": 5 }, "end": { "line": 10, "character": 12 } }
                        }
                    ],
                    "incoming_calls": [
                        {
                            "from": { "name": "handle_auth", "kind": 12, "uri": "${WORKSPACE}/src/main.rs" },
                            "fromRanges": [{ "start": { "line": 20, "character": 4 }, "end": { "line": 20, "character": 9 } }]
                        }
                    ],
                    "implementation": [
                        {
                            "uri": "${WORKSPACE}/src/impl.rs",
                            "range": { "start": { "line": 50, "character": 0 }, "end": { "line": 60, "character": 1 } }
                        }
                    ]
                }
            }
        });

        let index = CannedIndex::from_value(&raw_json).unwrap();

        let ws = "/test/my_project";
        let def = index
            .lookup_definition("file:///mock/workspace/src/lib.rs", 10, 5, Some(ws))
            .unwrap();

        assert_eq!(def["uri"], "/test/my_project/src/lib.rs");

        let refs = index
            .lookup_references("src/lib.rs", 10, 5, Some(ws))
            .unwrap();
        assert_eq!(refs[0]["uri"], "/test/my_project/src/main.rs");

        let incoming = index
            .lookup_incoming_calls("src/lib.rs", 10, 5, Some(ws))
            .unwrap();
        assert_eq!(incoming[0]["from"]["name"], "handle_auth");

        let impls = index
            .lookup_implementation("src/lib.rs", 10, 5, Some(ws))
            .unwrap();
        assert_eq!(impls[0]["uri"], "/test/my_project/src/impl.rs");
    }

    #[test]
    fn test_position_key_parsing() {
        assert_eq!(
            PositionKey::parse(&json!("(10, 5)")),
            Some(PositionKey::new(10, 5))
        );
        assert_eq!(
            PositionKey::parse(&json!("10:5")),
            Some(PositionKey::new(10, 5))
        );
        assert_eq!(
            PositionKey::parse(&json!([10, 5])),
            Some(PositionKey::new(10, 5))
        );
        assert_eq!(
            PositionKey::parse(&json!({ "line": 10, "character": 5 })),
            Some(PositionKey::new(10, 5))
        );
    }
}
