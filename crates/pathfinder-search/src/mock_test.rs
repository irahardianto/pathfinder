use super::*;

fn params() -> SearchParams {
    let temp = tempfile::tempdir().expect("create tempdir");
    SearchParams {
        workspace_root: temp.path().to_path_buf(),
        query: "test".to_owned(),
        ..Default::default()
    }
}

#[tokio::test]
async fn test_mock_defaults_to_empty_result() {
    let mock = MockScout::default();
    let result = mock.search(&params()).await.expect("should succeed");
    assert!(result.matches.is_empty());
    assert_eq!(result.total_matches, 0);
}

#[tokio::test]
async fn test_mock_returns_configured_result() {
    let mock = MockScout::default();
    mock.set_result(Ok(SearchResult {
        matches: vec![],
        total_matches: 42,
        truncated: true,
        files_searched: 0,
        files_in_scope: 0,
        binary_skipped: 0,
        gitignored_skipped: 0,
        other_skipped: 0,
    }));
    let result = mock.search(&params()).await.expect("should succeed");
    assert_eq!(result.total_matches, 42);
    assert!(result.truncated);
}

#[tokio::test]
async fn test_mock_records_calls() {
    let mock = MockScout::default();
    let _ = mock.search(&params()).await;
    let _ = mock.search(&params()).await;
    assert_eq!(mock.call_count(), 2);
}

#[tokio::test]
async fn test_mock_returns_error_when_configured() {
    let mock = MockScout::default();
    mock.set_result(Err("something broke".to_owned()));
    let result = mock.search(&params()).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_mock_set_results_returns_sequentially() {
    let mock = MockScout::default();
    mock.set_results(vec![
        Ok(SearchResult {
            matches: vec![],
            total_matches: 1,
            truncated: false,
            files_searched: 0,
            files_in_scope: 0,
            binary_skipped: 0,
            gitignored_skipped: 0,
            other_skipped: 0,
        }),
        Ok(SearchResult {
            matches: vec![],
            total_matches: 2,
            truncated: false,
            files_searched: 0,
            files_in_scope: 0,
            binary_skipped: 0,
            gitignored_skipped: 0,
            other_skipped: 0,
        }),
        Ok(SearchResult {
            matches: vec![],
            total_matches: 3,
            truncated: false,
            files_searched: 0,
            files_in_scope: 0,
            binary_skipped: 0,
            gitignored_skipped: 0,
            other_skipped: 0,
        }),
    ]);

    let r1 = mock.search(&params()).await.expect("call 1");
    assert_eq!(r1.total_matches, 1);
    let r2 = mock.search(&params()).await.expect("call 2");
    assert_eq!(r2.total_matches, 2);
    let r3 = mock.search(&params()).await.expect("call 3");
    assert_eq!(r3.total_matches, 3);
    // Queue exhausted — falls back to empty
    let r4 = mock.search(&params()).await.expect("call 4");
    assert_eq!(r4.total_matches, 0);
}
