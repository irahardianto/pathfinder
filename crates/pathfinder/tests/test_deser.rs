#![allow(clippy::uninlined_format_args)]
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
