use super::*;

#[test]
fn test_search_params_default_values() {
    let d = SearchParams::default();
    assert_eq!(d.workspace_root, PathBuf::from("."));
    assert_eq!(d.max_results, 50);
    assert_eq!(d.path_glob, "**/*");
    assert_eq!(d.context_lines, 2);
    assert_eq!(d.offset, 0);
    assert!(!d.is_regex);
    assert!(d.query.is_empty());
    assert!(d.exclude_glob.is_empty());
}
