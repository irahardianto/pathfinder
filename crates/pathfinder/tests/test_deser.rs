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

fn default_start_line() -> u32 {
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
    // A batch edit using text targeting (Option B) should deserialize fully.
    let json = r#"{
        "filepath": "src/App.vue",
        "base_version": "sha256:abc",
        "edits": [
            {
                "text_target": {
                    "old_text": "<button>Click me</button>",
                    "context_line": 14
                },
                "new_text": "<button>Submit</button>",
                "normalize_whitespace": false
            }
        ]
    }"#;
    let params: pathfinder_lib::server::types::ReplaceBatchParams =
        serde_json::from_str(json).expect("text_target edit should deserialize");
    assert_eq!(params.filepath, "src/App.vue");
    assert_eq!(params.edits.len(), 1);
    let edit = &params.edits[0];
    let tt = edit
        .text_target
        .as_ref()
        .expect("text_target should be set");
    assert_eq!(tt.old_text, "<button>Click me</button>");
    assert_eq!(tt.context_line, 14);
    assert_eq!(edit.new_text.as_deref(), Some("<button>Submit</button>"));
    assert!(!edit.normalize_whitespace);
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
    assert!(edit.text_target.is_none());
    assert!(!edit.normalize_whitespace); // default
}
