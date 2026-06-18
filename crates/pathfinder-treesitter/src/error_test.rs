use super::*;
use std::io::Error;

#[test]
fn test_from_io_error() {
    let io_err = Error::new(std::io::ErrorKind::NotFound, "test");
    let surgeon_err = SurgeonError::from(io_err);
    assert!(matches!(surgeon_err, SurgeonError::Io(_)));

    let pathfinder_err: pathfinder_common::error::PathfinderError = surgeon_err.into();
    assert!(matches!(
        pathfinder_err,
        pathfinder_common::error::PathfinderError::IoError { .. }
    ));
}

#[test]
fn test_surgeon_error_symbol_not_found_converts() {
    let err = SurgeonError::SymbolNotFound {
        path: "some::path".to_string(),
        did_you_mean: vec!["some::peth".to_string()],
    };
    let pf_err: pathfinder_common::error::PathfinderError = err.into();
    assert!(matches!(
        pf_err,
        pathfinder_common::error::PathfinderError::SymbolNotFound {
            semantic_path,
            did_you_mean,
            retry_after_seconds,
        } if semantic_path == "some::path"
            && did_you_mean == vec!["some::peth"]
            && retry_after_seconds.is_none()
    ));
}

#[test]
fn test_surgeon_error_file_not_found_converts() {
    let err = SurgeonError::FileNotFound(std::path::PathBuf::from("missing/file.rs"));
    let pf_err: pathfinder_common::error::PathfinderError = err.into();
    assert!(matches!(
        pf_err,
        pathfinder_common::error::PathfinderError::FileNotFound { ref path }
        if path == &std::path::PathBuf::from("missing/file.rs")
    ));
}

#[test]
fn test_surgeon_error_unsupported_language_converts() {
    let err = SurgeonError::UnsupportedLanguage(std::path::PathBuf::from("file.xyz"));
    let pf_err: pathfinder_common::error::PathfinderError = err.into();
    assert!(matches!(
        pf_err,
        pathfinder_common::error::PathfinderError::UnsupportedLanguage { ref path }
        if path == &std::path::PathBuf::from("file.xyz")
    ));
}

#[test]
fn test_surgeon_error_parse_error_converts() {
    let err = SurgeonError::ParseError {
        path: std::path::PathBuf::from("broken.rs"),
        reason: "unexpected token".to_string(),
    };
    let pf_err: pathfinder_common::error::PathfinderError = err.into();
    assert!(matches!(
        pf_err,
        pathfinder_common::error::PathfinderError::ParseError {
            ref path,
            ref reason,
        } if path == &std::path::PathBuf::from("broken.rs")
            && reason == "unexpected token"
    ));
}
