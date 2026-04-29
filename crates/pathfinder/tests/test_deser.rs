#![allow(
    clippy::uninlined_format_args,
    clippy::expect_used,
    clippy::unwrap_used
)]
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct ReadSourceFileParams {
    pub filepath: String,
    #[serde(default = "default_detail_level")]
    pub detail_level: String,
    #[serde(default = "default_start_line")]
    pub start_line: u32,
    #[serde(default)]
    pub end_line: Option<u32>,
}

fn default_detail_level() -> String {
    "compact".to_string()
}

const fn default_start_line() -> u32 {
    1
}

#[test]
fn do_test() {
    let json = r#"
    {
      "filepath": "crates/pathfinder-common/src/types.rs",
      "detail_level": "full"
    }
    "#;
    let res: Result<ReadSourceFileParams, _> = serde_json::from_str(json);
    println!("Deserialized: {:?}", res);
    assert!(res.is_ok());
}

// ── E3.1 BatchEdit deserialization tests ────────────────────────────────────

#[test]
fn test_batch_edit_text_target_deser() {
    // A batch edit using text targeting (Option B) should deserialize with the flat schema.
    let json = r#"{
        "filepath": "src/App.vue",
        "base_version": "sha256:abc",
        "edits": [
            {
                "old_text": "<button>Click me</button>",
                "context_line": 14,
                "replacement_text": "<button>Submit</button>",
                "normalize_whitespace": false
            }
        ]
    }"#;
    let params: pathfinder_lib::server::types::ReplaceBatchParams =
        serde_json::from_str(json).expect("flat text-targeting edit should deserialize");
    assert_eq!(params.filepath, "src/App.vue");
    assert_eq!(params.edits.len(), 1);
    let edit = &params.edits[0];
    assert_eq!(edit.old_text.as_deref(), Some("<button>Click me</button>"));
    assert_eq!(edit.context_line, Some(14));
    assert_eq!(
        edit.replacement_text.as_deref(),
        Some("<button>Submit</button>")
    );
    assert!(!edit.normalize_whitespace);
    // Semantic fields should be absent
    assert!(edit.new_code.is_none());
}

#[test]
fn test_batch_edit_semantic_target_deser() {
    // A batch edit using semantic targeting (Option A) with defaults should deserialize.
    let json = r#"{
        "filepath": "src/lib.rs",
        "base_version": "sha256:def",
        "edits": [
            {
                "semantic_path": "src/lib.rs::MyStruct.my_method",
                "edit_type": "replace_body",
                "new_code": "    println!(\"patched\");\n"
            }
        ]
    }"#;
    let params: pathfinder_lib::server::types::ReplaceBatchParams =
        serde_json::from_str(json).expect("semantic edit should deserialize");
    assert_eq!(params.edits.len(), 1);
    let edit = &params.edits[0];
    assert_eq!(edit.semantic_path, "src/lib.rs::MyStruct.my_method");
    assert_eq!(edit.edit_type, "replace_body");
    // Text targeting fields should all be absent for a pure semantic edit
    assert!(edit.old_text.is_none());
    assert!(edit.context_line.is_none());
    assert!(edit.replacement_text.is_none());
    assert!(!edit.normalize_whitespace); // default
}
