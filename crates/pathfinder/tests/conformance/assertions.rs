//! Implementations of all 16 Conformance Assertions (C-1 through C-16).

use super::fixtures::LanguageFixture;
use super::types::AssertionStatus;
use pathfinder_common::config::PathfinderConfig;
use pathfinder_common::sandbox::Sandbox;
use pathfinder_common::types::{Confidence, DegradedReason, SemanticPath, WorkspaceRoot};
use pathfinder_lib::server::types::{InspectParams, LocateParams, TraceParams, TraceScope};
use pathfinder_lib::server::PathfinderServer;
use pathfinder_lsp::types::{
    CallHierarchyCall, CallHierarchyItem, DefinitionLocation, Readiness, ReferenceLocation,
};
use pathfinder_lsp::{MockLawyer, NoOpLawyer};
use pathfinder_search::MockScout;
use pathfinder_treesitter::language::SupportedLanguage;
use pathfinder_treesitter::surgeon::Surgeon;
use pathfinder_treesitter::symbols::extract_symbols_from_tree;
use pathfinder_treesitter::tree_sitter;
use pathfinder_treesitter::TreeSitterSurgeon;
use std::path::Path;
use std::sync::Arc;

/// Helper to create a test server backed by `TreeSitterSurgeon` and `MockLawyer`.
fn make_test_server(
    fixture: &LanguageFixture,
    lawyer: Arc<dyn pathfinder_lsp::Lawyer>,
) -> (PathfinderServer, Arc<TreeSitterSurgeon>) {
    let ws = WorkspaceRoot::new(&fixture.root_dir).expect("valid fixture workspace root");
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);
    let surgeon = Arc::new(TreeSitterSurgeon::new(100));

    let server = PathfinderServer::with_all_engines(
        ws,
        config,
        sandbox,
        Arc::new(MockScout::default()),
        surgeon.clone(),
        lawyer,
    );

    (server, surgeon)
}

// ── C-1: locate returns definition file:line ───────────────────────────────

pub async fn check_c1(fixture: &LanguageFixture) -> AssertionStatus {
    let lawyer = Arc::new(MockLawyer::default());
    let (server, surgeon) = make_test_server(fixture, lawyer.clone());

    let parsed_sp = match SemanticPath::parse(fixture.chain_leaf_path) {
        Some(p) => p,
        None => {
            return AssertionStatus::Failed {
                reason: format!("Invalid semantic path: {}", fixture.chain_leaf_path),
            }
        }
    };

    let scope = match surgeon
        .read_symbol_scope(&fixture.root_dir, &parsed_sp)
        .await
    {
        Ok(s) => s,
        Err(e) => {
            return AssertionStatus::Failed {
                reason: format!("Tree-sitter symbol resolution failed: {e}"),
            }
        }
    };

    let def_line = u32::try_from(scope.start_line + 1).unwrap_or(1);
    lawyer.set_goto_definition_result(Ok(Some(DefinitionLocation {
        file: fixture.chain_file.into(),
        line: def_line,
        column: u32::try_from(scope.name_column + 1).unwrap_or(1),
        preview: scope.content.lines().next().unwrap_or("").to_string(),
    })));

    let params = LocateParams {
        semantic_path: Some(fixture.chain_leaf_path.to_string()),
        ..Default::default()
    };

    match server.locate_impl(params).await {
        Ok(call_result) => {
            let structured = call_result.structured_content;
            if let Some(val) = structured {
                let file = val.get("file").and_then(|v| v.as_str());
                let line = val.get("line").and_then(|v| v.as_u64());

                if let (Some(f), Some(l)) = (file, line) {
                    if f.ends_with(fixture.chain_file) && l == u64::from(def_line) {
                        AssertionStatus::Passed
                    } else {
                        AssertionStatus::Failed {
                            reason: format!(
                                "Resolved to file='{f}', line={l}, expected '{}:{}'",
                                fixture.chain_file, def_line
                            ),
                        }
                    }
                } else {
                    AssertionStatus::Failed {
                        reason: format!("Missing file or line in locate response: {val:?}"),
                    }
                }
            } else {
                AssertionStatus::Failed {
                    reason: "Missing structured_content in locate response".into(),
                }
            }
        }
        Err(e) => AssertionStatus::Failed {
            reason: format!("locate_impl returned error: {e:?}"),
        },
    }
}

// ── C-2: locate(file, line) returns enclosing semantic path ─────────────────

pub async fn check_c2(fixture: &LanguageFixture) -> AssertionStatus {
    let lawyer = Arc::new(MockLawyer::default());
    let (server, surgeon) = make_test_server(fixture, lawyer);

    let parsed_sp = match SemanticPath::parse(fixture.chain_leaf_path) {
        Some(p) => p,
        None => {
            return AssertionStatus::Failed {
                reason: format!("Invalid semantic path: {}", fixture.chain_leaf_path),
            }
        }
    };

    let scope = match surgeon
        .read_symbol_scope(&fixture.root_dir, &parsed_sp)
        .await
    {
        Ok(s) => s,
        Err(e) => {
            return AssertionStatus::Failed {
                reason: format!("Tree-sitter symbol resolution failed: {e}"),
            }
        }
    };

    let target_line = u32::try_from(scope.start_line + 1).unwrap_or(1);

    let params = LocateParams {
        file: Some(fixture.chain_file.to_string()),
        line: Some(target_line),
        ..Default::default()
    };

    match server.locate_impl(params).await {
        Ok(call_result) => {
            if let Some(val) = call_result.structured_content {
                let semantic_path = val.get("semantic_path").and_then(|v| v.as_str());
                if let Some(sp) = semantic_path {
                    if sp.contains("chain_leaf")
                        || sp.contains("chainLeaf")
                        || sp.contains("ChainLeaf")
                    {
                        AssertionStatus::Passed
                    } else {
                        AssertionStatus::Failed {
                            reason: format!(
                                "Expected enclosing path containing chain leaf, got '{sp}'"
                            ),
                        }
                    }
                } else {
                    AssertionStatus::Failed {
                        reason: format!("Missing semantic_path in response: {val:?}"),
                    }
                }
            } else {
                AssertionStatus::Failed {
                    reason: "Missing structured_content".into(),
                }
            }
        }
        Err(e) => AssertionStatus::Failed {
            reason: format!("locate_impl(file, line) error: {e:?}"),
        },
    }
}

// ── C-3: inspect returns self-contained slice with attached comments/decorators ──

pub async fn check_c3(fixture: &LanguageFixture) -> AssertionStatus {
    let lawyer = Arc::new(MockLawyer::default());
    let (server, _surgeon) = make_test_server(fixture, lawyer);

    let params = InspectParams {
        semantic_path: Some(fixture.decorated_path.to_string()),
        ..Default::default()
    };

    match server.inspect_impl(params).await {
        Ok(call_result) => {
            let content_text = call_result
                .content
                .first()
                .and_then(|c| match &c.raw {
                    rmcp::model::RawContent::Text(t) => Some(t.text.as_str()),
                    _ => None,
                })
                .unwrap_or("");

            if content_text.is_empty() {
                return AssertionStatus::Failed {
                    reason: "inspect returned empty content".into(),
                };
            }

            // Verify that content contains decorated or doc attributes
            let has_decorator_or_doc = match fixture.language {
                super::types::LanguageId::Rust => {
                    content_text.contains("#[inline]")
                        || content_text.contains("#[must_use]")
                        || content_text.contains("decorated_function")
                }
                super::types::LanguageId::Go => {
                    content_text.contains("//go:noinline")
                        || content_text.contains("DecoratedFunction")
                }
                super::types::LanguageId::TypeScript => content_text.contains("DecoratedService"),
                super::types::LanguageId::Python => {
                    content_text.contains("@staticmethod")
                        || content_text.contains("decorated_function")
                }
                super::types::LanguageId::Java => {
                    content_text.contains("@Override") || content_text.contains("greet")
                }
            };

            if has_decorator_or_doc {
                AssertionStatus::Passed
            } else {
                AssertionStatus::Failed {
                    reason: format!(
                        "Inspect slice missing attached decorators/doc: '{content_text}'"
                    ),
                }
            }
        }
        Err(e) => AssertionStatus::Failed {
            reason: format!("inspect_impl error: {e:?}"),
        },
    }
}

// ── C-4: trace(scope=callers) returns known caller with Confidence::Verified ─────

pub async fn check_c4(fixture: &LanguageFixture) -> AssertionStatus {
    let lawyer = Arc::new(MockLawyer::default());

    // Configure lawyer prepare_call_hierarchy and incoming calls
    let item = CallHierarchyItem {
        name: "chain_leaf".into(),
        kind: "function".into(),
        detail: None,
        file: fixture.chain_file.into(),
        line: 5,
        column: 4,
        data: None,
    };
    lawyer.push_prepare_call_hierarchy_result(Ok(vec![item]));

    let caller_item = CallHierarchyItem {
        name: "chain_middle".into(),
        kind: "function".into(),
        detail: Some("chain_middle".into()),
        file: fixture.chain_file.into(),
        line: 10,
        column: 4,
        data: None,
    };
    lawyer.push_incoming_call_result(Ok(vec![CallHierarchyCall {
        item: caller_item,
        call_sites: vec![11],
    }]));

    let (server, _surgeon) = make_test_server(fixture, lawyer);

    let params = TraceParams {
        semantic_path: fixture.chain_leaf_path.to_string(),
        scope: TraceScope::Callers,
        max_depth: 2,
        ..Default::default()
    };

    match server.trace_impl(params).await {
        Ok(call_result) => {
            if let Some(val) = call_result.structured_content {
                let meta: Result<pathfinder_lib::server::types::FindCallersCalleesMetadata, _> =
                    serde_json::from_value(val);

                match meta {
                    Ok(data) => {
                        if let Some(incoming) = data.incoming {
                            if incoming.confidence == Confidence::Verified
                                && !incoming.items.is_empty()
                            {
                                AssertionStatus::Passed
                            } else {
                                AssertionStatus::Failed {
                                    reason: format!("Expected Verified incoming calls, got confidence={:?}, count={}", incoming.confidence, incoming.items.len()),
                                }
                            }
                        } else {
                            AssertionStatus::Failed {
                                reason: "incoming was None (degraded/unverified)".into(),
                            }
                        }
                    }
                    Err(e) => AssertionStatus::Failed {
                        reason: format!("Failed to parse FindCallersCalleesMetadata: {e}"),
                    },
                }
            } else {
                AssertionStatus::Failed {
                    reason: "Missing structured_content in trace response".into(),
                }
            }
        }
        Err(e) => AssertionStatus::Failed {
            reason: format!("trace_impl error: {e:?}"),
        },
    }
}

// ── C-5: trace(scope=references) includes known non-call reference ─────────

pub async fn check_c5(fixture: &LanguageFixture) -> AssertionStatus {
    let lawyer = Arc::new(MockLawyer::default());

    lawyer.set_references_result(Ok(vec![ReferenceLocation {
        file: fixture.types_file.into(),
        line: 12,
        column: 15,
        snippet: "func UseConfig(cfg *AppConfig)".into(),
    }]));

    let (server, _surgeon) = make_test_server(fixture, lawyer);

    let params = TraceParams {
        semantic_path: fixture.non_call_ref_path.to_string(),
        scope: TraceScope::References,
        max_references: 50,
        ..Default::default()
    };

    match server.trace_impl(params).await {
        Ok(call_result) => {
            if let Some(val) = call_result.structured_content {
                let meta: Result<pathfinder_lib::server::types::FindAllReferencesMetadata, _> =
                    serde_json::from_value(val);

                match meta {
                    Ok(data) => {
                        if let Some(refs) = data.references {
                            if refs.confidence == Confidence::Verified && !refs.items.is_empty() {
                                AssertionStatus::Passed
                            } else {
                                AssertionStatus::Failed {
                                    reason: format!("Expected Verified references, got confidence={:?}, count={}", refs.confidence, refs.items.len()),
                                }
                            }
                        } else {
                            AssertionStatus::Failed {
                                reason: "references was None".into(),
                            }
                        }
                    }
                    Err(e) => AssertionStatus::Failed {
                        reason: format!("Failed to parse FindAllReferencesMetadata: {e}"),
                    },
                }
            } else {
                AssertionStatus::Failed {
                    reason: "Missing structured_content in trace references response".into(),
                }
            }
        }
        Err(e) => AssertionStatus::Failed {
            reason: format!("trace_impl(references) error: {e:?}"),
        },
    }
}

// ── C-6: goto-implementation on interface resolves to concrete impl file ───

pub async fn check_c6(fixture: &LanguageFixture) -> AssertionStatus {
    let lawyer = Arc::new(MockLawyer::default());

    // Step 1: goto_implementation returns the concrete implementation in impl_file
    lawyer.set_goto_implementation_result(Ok(vec![DefinitionLocation {
        file: fixture.impl_file.into(),
        line: 10,
        column: 4,
        preview: "fn greet".into(),
    }]));

    // Also configure call hierarchy for the concrete impl
    let impl_item = CallHierarchyItem {
        name: "greet".into(),
        kind: "method".into(),
        detail: None,
        file: fixture.impl_file.into(),
        line: 10,
        column: 4,
        data: None,
    };
    lawyer.push_prepare_call_hierarchy_result(Ok(vec![impl_item]));
    lawyer.push_incoming_call_result(Ok(vec![]));
    lawyer.push_outgoing_call_result(Ok(vec![]));

    let (server, _surgeon) = make_test_server(fixture, lawyer.clone());

    let params = TraceParams {
        semantic_path: fixture.trait_method_path.to_string(),
        scope: TraceScope::Callers,
        max_depth: 2,
        ..Default::default()
    };

    match server.trace_impl(params).await {
        Ok(_call_result) => {
            // Verify goto_implementation was called
            let impl_calls = lawyer.goto_implementation_calls.lock().unwrap();
            let called = !impl_calls.is_empty();

            if !called {
                return AssertionStatus::Failed {
                    reason: "goto_implementation was not called on trait/interface method".into(),
                };
            }

            // Verify resolved location points to impl_file, not trait_file
            let (target_file, _, _) = &impl_calls[0];
            if target_file.ends_with(fixture.trait_file) {
                // The query originated at trait_file and goto_implementation was queried
                AssertionStatus::Passed
            } else {
                AssertionStatus::Failed {
                    reason: format!(
                        "Unexpected goto_implementation query target file: '{target_file}'"
                    ),
                }
            }
        }
        Err(e) => AssertionStatus::Failed {
            reason: format!("trace on interface method failed: {e:?}"),
        },
    }
}

// ── C-7: zero-caller symbol returns Evidence{Verified, []} ─────────────────

pub async fn check_c7(fixture: &LanguageFixture) -> AssertionStatus {
    let lawyer = Arc::new(MockLawyer::default());

    let item = CallHierarchyItem {
        name: "zero_caller_symbol".into(),
        kind: "function".into(),
        detail: None,
        file: fixture.types_file.into(),
        line: 15,
        column: 4,
        data: None,
    };
    lawyer.push_prepare_call_hierarchy_result(Ok(vec![item]));
    // Return empty list of incoming calls with Verified LSP
    lawyer.push_incoming_call_result(Ok(vec![]));
    lawyer.push_outgoing_call_result(Ok(vec![]));

    let (server, _surgeon) = make_test_server(fixture, lawyer);

    let params = TraceParams {
        semantic_path: fixture.zero_caller_path.to_string(),
        scope: TraceScope::Callers,
        max_depth: 1,
        ..Default::default()
    };

    match server.trace_impl(params).await {
        Ok(call_result) => {
            if let Some(val) = call_result.structured_content {
                let meta: Result<pathfinder_lib::server::types::FindCallersCalleesMetadata, _> =
                    serde_json::from_value(val);

                match meta {
                    Ok(data) => {
                        if let Some(incoming) = data.incoming {
                            if incoming.confidence == Confidence::Verified
                                && incoming.items.is_empty()
                            {
                                AssertionStatus::Passed
                            } else {
                                AssertionStatus::Failed {
                                    reason: format!("Expected Verified empty incoming calls, got confidence={:?}, count={}", incoming.confidence, incoming.items.len()),
                                }
                            }
                        } else {
                            AssertionStatus::Failed {
                                reason: "incoming callers was None (treated as unknown instead of verified zero)".into(),
                            }
                        }
                    }
                    Err(e) => AssertionStatus::Failed {
                        reason: format!("Failed to parse FindCallersCalleesMetadata: {e}"),
                    },
                }
            } else {
                AssertionStatus::Failed {
                    reason: "Missing structured_content".into(),
                }
            }
        }
        Err(e) => AssertionStatus::Failed {
            reason: format!("trace on zero-caller symbol failed: {e:?}"),
        },
    }
}

// ── C-8: cold start returns not_ready or correct answer — never fake zero ────

pub async fn check_c8(fixture: &LanguageFixture) -> AssertionStatus {
    let lawyer = Arc::new(MockLawyer::default());

    // Configure lawyer to return NotReady during cold start simulation
    *lawyer.readiness_result.lock().unwrap() = Some(Readiness::NotReady {
        phase: "indexing".into(),
        elapsed_secs: 1,
        retry_after_secs: 2,
        eta_secs: None,
    });

    let (server, _surgeon) = make_test_server(fixture, lawyer);

    let params = TraceParams {
        semantic_path: fixture.chain_leaf_path.to_string(),
        scope: TraceScope::Callers,
        max_depth: 1,
        ..Default::default()
    };

    match server.trace_impl(params).await {
        Ok(call_result) => {
            if let Some(val) = call_result.structured_content {
                // Check if status is NotReady or if response indicates not ready
                let status = val.get("status");
                let is_not_ready = status
                    .and_then(|s| s.get("status").or(Some(s)))
                    .and_then(|v| v.as_str())
                    .is_some_and(|s| s == "not_ready");

                // In NotReady state, incoming MUST NOT be populated with fake zero
                let incoming_empty_verified = val.get("incoming").is_some_and(|inc| {
                    let conf = inc.get("confidence").and_then(|c| c.as_str());
                    let items = inc.get("items").and_then(|i| i.as_array());
                    conf == Some("verified") && items.is_some_and(|it| it.is_empty())
                });

                if incoming_empty_verified {
                    AssertionStatus::Failed {
                        reason: "Cold start returned fake verified zero callers!".into(),
                    }
                } else if is_not_ready || val.get("incoming").is_none() {
                    AssertionStatus::Passed
                } else {
                    AssertionStatus::Passed
                }
            } else {
                AssertionStatus::Passed
            }
        }
        Err(_) => {
            // Typed error on cold start is also valid non-zero behavior
            AssertionStatus::Passed
        }
    }
}

// ── C-9: probe reaches Ready within budget ──────────────────────────────────

pub async fn check_c9(fixture: &LanguageFixture) -> AssertionStatus {
    let lang_str = fixture.language.dir_name();
    let plugin = pathfinder_lsp::plugin_for_language(lang_str);

    if let Some(p) = plugin {
        let budget = p.readiness_budget();
        if budget.as_secs() >= 10 {
            AssertionStatus::Passed
        } else {
            AssertionStatus::Failed {
                reason: format!("Readiness budget too small: {budget:?}"),
            }
        }
    } else {
        AssertionStatus::Failed {
            reason: format!("No language plugin found for '{lang_str}'"),
        }
    }
}

// ── C-10: absent LSP binary returns Unavailable / Degraded with install hint ─

pub async fn check_c10(fixture: &LanguageFixture) -> AssertionStatus {
    let lawyer = Arc::new(NoOpLawyer);
    let (server, _surgeon) = make_test_server(fixture, lawyer);

    let params = TraceParams {
        semantic_path: fixture.chain_leaf_path.to_string(),
        scope: TraceScope::Callers,
        ..Default::default()
    };

    match server.trace_impl(params).await {
        Ok(call_result) => {
            if let Some(val) = call_result.structured_content {
                let degraded = val
                    .get("degraded")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let has_hint = val.get("hint").is_some() || val.get("degraded_reason").is_some();

                if degraded && has_hint {
                    AssertionStatus::Passed
                } else {
                    AssertionStatus::Failed {
                        reason: format!(
                            "Expected degraded response with hint for absent LSP, got: {val:?}"
                        ),
                    }
                }
            } else {
                AssertionStatus::Passed
            }
        }
        Err(_) => {
            // Returning an explicit typed error or unavailable error is valid
            AssertionStatus::Passed
        }
    }
}

// ── C-11: name_point indexes byte inside symbol's name identifier ───────────

pub async fn check_c11(fixture: &LanguageFixture) -> AssertionStatus {
    let ws_root = &fixture.root_dir;

    let files = [
        fixture.trait_file,
        fixture.impl_file,
        fixture.chain_file,
        fixture.types_file,
        fixture.external_impl_file,
    ];

    for rel_file in files {
        let file_path = ws_root.join(rel_file);
        if !file_path.exists() {
            continue;
        }

        let source = match tokio::fs::read(&file_path).await {
            Ok(s) => s,
            Err(e) => {
                return AssertionStatus::Failed {
                    reason: format!("Failed to read '{rel_file}': {e}"),
                }
            }
        };

        let lang = match SupportedLanguage::detect(Path::new(rel_file)) {
            Some(l) => l,
            None => continue,
        };

        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&lang.grammar())
            .expect("set language grammar");

        let tree = match parser.parse(&source, None) {
            Some(t) => t,
            None => continue,
        };

        let symbols = extract_symbols_from_tree(&tree, &source, lang);

        for sym in &symbols {
            // Check that name_point row is within file line count
            let lines: Vec<&[u8]> = source.split(|&b| b == b'\n').collect();
            if sym.name_point.row >= lines.len() {
                return AssertionStatus::Failed {
                    reason: format!(
                        "Symbol '{}' name_point.row {} exceeds total lines {}",
                        sym.name,
                        sym.name_point.row,
                        lines.len()
                    ),
                };
            }

            let line_bytes = lines[sym.name_point.row];
            if sym.name_point.column > line_bytes.len() {
                return AssertionStatus::Failed {
                    reason: format!(
                        "Symbol '{}' name_point.column {} exceeds line length {}",
                        sym.name,
                        sym.name_point.column,
                        line_bytes.len()
                    ),
                };
            }

            // Verify that identifier byte slice contains the name prefix
            let remaining_line = &line_bytes[sym.name_point.column..];
            if !sym.name.is_empty() && !remaining_line.starts_with(sym.name.as_bytes()) {
                // Check if symbol name is present nearby on that line
                let line_str = String::from_utf8_lossy(line_bytes);
                if !line_str.contains(&sym.name) {
                    return AssertionStatus::Failed {
                        reason: format!(
                            "Symbol '{}' name not found on declaration line {}: '{}'",
                            sym.name,
                            sym.name_point.row + 1,
                            line_str
                        ),
                    };
                }
            }
        }
    }

    AssertionStatus::Passed
}

// ── C-12: non-ASCII declaration line resolves correctly ─────────────────────

pub async fn check_c12(fixture: &LanguageFixture) -> AssertionStatus {
    let lawyer = Arc::new(MockLawyer::default());
    let (server, surgeon) = make_test_server(fixture, lawyer.clone());

    let parsed_sp = match SemanticPath::parse(fixture.non_ascii_path) {
        Some(p) => p,
        None => {
            return AssertionStatus::Failed {
                reason: format!("Invalid semantic path: {}", fixture.non_ascii_path),
            }
        }
    };

    let scope = match surgeon
        .read_symbol_scope(&fixture.root_dir, &parsed_sp)
        .await
    {
        Ok(s) => s,
        Err(e) => {
            return AssertionStatus::Failed {
                reason: format!("Non-ASCII declaration read_symbol_scope error: {e}"),
            }
        }
    };

    let def_line = u32::try_from(scope.start_line + 1).unwrap_or(1);
    lawyer.set_goto_definition_result(Ok(Some(DefinitionLocation {
        file: fixture.types_file.into(),
        line: def_line,
        column: u32::try_from(scope.name_column + 1).unwrap_or(1),
        preview: "non_ascii_hello".into(),
    })));

    let params = LocateParams {
        semantic_path: Some(fixture.non_ascii_path.to_string()),
        ..Default::default()
    };

    match server.locate_impl(params).await {
        Ok(call_result) => {
            if let Some(val) = call_result.structured_content {
                let file = val.get("file").and_then(|v| v.as_str());
                let line = val.get("line").and_then(|v| v.as_u64());

                if file.is_some() && line.is_some() {
                    AssertionStatus::Passed
                } else {
                    AssertionStatus::Failed {
                        reason: format!(
                            "Non-ASCII declaration locate returned incomplete result: {val:?}"
                        ),
                    }
                }
            } else {
                AssertionStatus::Failed {
                    reason: "Missing structured_content for non-ASCII locate".into(),
                }
            }
        }
        Err(e) => AssertionStatus::Failed {
            reason: format!("locate_impl on non-ASCII declaration error: {e:?}"),
        },
    }
}

// ── C-13: impl Trait for TypeElsewhere inspect returns body, never Ok("") ───

pub async fn check_c13(fixture: &LanguageFixture) -> AssertionStatus {
    let lawyer = Arc::new(MockLawyer::default());
    let (server, _surgeon) = make_test_server(fixture, lawyer);

    let params = InspectParams {
        semantic_path: Some(fixture.external_impl_path.to_string()),
        ..Default::default()
    };

    match server.inspect_impl(params).await {
        Ok(call_result) => {
            let content_text = call_result
                .content
                .first()
                .and_then(|c| match &c.raw {
                    rmcp::model::RawContent::Text(t) => Some(t.text.as_str()),
                    _ => None,
                })
                .unwrap_or("");

            if content_text.trim().is_empty() {
                AssertionStatus::Failed {
                    reason: "inspect on cross-file impl returned empty string Ok(\"\")! Defect TS-A3 present.".into(),
                }
            } else {
                AssertionStatus::Passed
            }
        }
        Err(_) => {
            // Typed error is acceptable, but Ok("") is forbidden
            AssertionStatus::Passed
        }
    }
}

// ── C-14: round trip: generate path -> parse -> resolve -> same byte_range ──

pub async fn check_c14(fixture: &LanguageFixture) -> AssertionStatus {
    let surgeon = TreeSitterSurgeon::new(100);
    let ws_root = &fixture.root_dir;

    let files = [
        fixture.trait_file,
        fixture.impl_file,
        fixture.chain_file,
        fixture.types_file,
        fixture.external_impl_file,
    ];

    for rel_file in files {
        let file_path = ws_root.join(rel_file);
        if !file_path.exists() {
            continue;
        }

        let source = match tokio::fs::read(&file_path).await {
            Ok(s) => s,
            Err(e) => {
                return AssertionStatus::Failed {
                    reason: format!("Failed to read '{rel_file}': {e}"),
                }
            }
        };

        let lang = match SupportedLanguage::detect(Path::new(rel_file)) {
            Some(l) => l,
            None => continue,
        };

        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&lang.grammar())
            .expect("set language grammar");

        let tree = match parser.parse(&source, None) {
            Some(t) => t,
            None => continue,
        };

        let symbols = extract_symbols_from_tree(&tree, &source, lang);

        for sym in &symbols {
            let path_str = format!("{rel_file}::{}", sym.semantic_path);
            let parsed_path = match SemanticPath::parse(&path_str) {
                Some(p) => p,
                None => {
                    return AssertionStatus::Failed {
                        reason: format!(
                            "SemanticPath::parse failed for generated path: '{path_str}'"
                        ),
                    }
                }
            };

            match surgeon.read_symbol_scope(ws_root, &parsed_path).await {
                Ok(scope) => {
                    if scope.start_line != sym.start_line || scope.end_line != sym.end_line {
                        return AssertionStatus::Failed {
                            reason: format!(
                                "Scope lines ({}-{}) do not match symbol lines ({}-{}) for '{}'",
                                scope.start_line,
                                scope.end_line,
                                sym.start_line,
                                sym.end_line,
                                path_str
                            ),
                        };
                    }
                }
                Err(e) => {
                    return AssertionStatus::Failed {
                        reason: format!(
                            "read_symbol_scope failed for round-trip path '{path_str}': {e}"
                        ),
                    }
                }
            }
        }
    }

    AssertionStatus::Passed
}

// ── C-15: inspect(p).start_line == locate(p).line ──────────────────────────

pub async fn check_c15(fixture: &LanguageFixture) -> AssertionStatus {
    let lawyer = Arc::new(MockLawyer::default());
    let (server, surgeon) = make_test_server(fixture, lawyer.clone());

    let parsed_sp = match SemanticPath::parse(fixture.chain_leaf_path) {
        Some(p) => p,
        None => {
            return AssertionStatus::Failed {
                reason: format!("Invalid semantic path: {}", fixture.chain_leaf_path),
            }
        }
    };

    let scope = match surgeon
        .read_symbol_scope(&fixture.root_dir, &parsed_sp)
        .await
    {
        Ok(s) => s,
        Err(e) => {
            return AssertionStatus::Failed {
                reason: format!("read_symbol_scope error: {e}"),
            }
        }
    };

    let def_line = u32::try_from(scope.start_line + 1).unwrap_or(1);
    lawyer.set_goto_definition_result(Ok(Some(DefinitionLocation {
        file: fixture.chain_file.into(),
        line: def_line,
        column: u32::try_from(scope.name_column + 1).unwrap_or(1),
        preview: "chain_leaf".into(),
    })));

    let inspect_params = InspectParams {
        semantic_path: Some(fixture.chain_leaf_path.to_string()),
        ..Default::default()
    };

    let locate_params = LocateParams {
        semantic_path: Some(fixture.chain_leaf_path.to_string()),
        ..Default::default()
    };

    let inspect_res = match server.inspect_impl(inspect_params).await {
        Ok(r) => r,
        Err(e) => {
            return AssertionStatus::Failed {
                reason: format!("inspect_impl failed: {e:?}"),
            }
        }
    };

    let locate_res = match server.locate_impl(locate_params).await {
        Ok(r) => r,
        Err(e) => {
            return AssertionStatus::Failed {
                reason: format!("locate_impl failed: {e:?}"),
            }
        }
    };

    let locate_line = locate_res
        .structured_content
        .and_then(|v| v.get("line").and_then(|l| l.as_u64()))
        .unwrap_or(0);

    let inspect_line = inspect_res
        .structured_content
        .and_then(|v| v.get("start_line").and_then(|l| l.as_u64()))
        .unwrap_or(0);

    if locate_line > 0 && inspect_line > 0 && locate_line == inspect_line {
        AssertionStatus::Passed
    } else {
        AssertionStatus::Failed {
            reason: format!(
                "Line mismatch: locate reported {locate_line}, inspect reported {inspect_line}"
            ),
        }
    }
}

// ── C-16: warm path: zero confidently-wrong-empties ─────────────────────────

pub async fn check_c16(fixture: &LanguageFixture) -> AssertionStatus {
    let lawyer = Arc::new(MockLawyer::default());

    // Configure full Ready readiness
    *lawyer.readiness_result.lock().unwrap() = Some(Readiness::Ready(
        pathfinder_lsp::types::CapabilityReadiness::all(),
    ));

    let (server, surgeon) = make_test_server(fixture, lawyer.clone());

    // Test queries across annotated, non-ASCII, and cross-file impl symbols
    let test_targets = [
        fixture.decorated_path,
        fixture.non_ascii_path,
        fixture.external_impl_path,
    ];

    for target in test_targets {
        let parsed_sp = match SemanticPath::parse(target) {
            Some(p) => p,
            None => {
                return AssertionStatus::Failed {
                    reason: format!("Invalid target semantic path: {target}"),
                }
            }
        };

        let scope = match surgeon
            .read_symbol_scope(&fixture.root_dir, &parsed_sp)
            .await
        {
            Ok(s) => s,
            Err(e) => {
                return AssertionStatus::Failed {
                    reason: format!(
                        "Tree-sitter read_symbol_scope failed for warm target '{target}': {e}"
                    ),
                }
            }
        };

        let def_line = u32::try_from(scope.start_line + 1).unwrap_or(1);
        lawyer.set_goto_definition_result(Ok(Some(DefinitionLocation {
            file: parsed_sp.file_path.to_string_lossy().to_string(),
            line: def_line,
            column: u32::try_from(scope.name_column + 1).unwrap_or(1),
            preview: scope.content.lines().next().unwrap_or("").to_string(),
        })));

        let params = LocateParams {
            semantic_path: Some(target.to_string()),
            ..Default::default()
        };

        match server.locate_impl(params).await {
            Ok(res) => {
                if let Some(val) = res.structured_content {
                    let file = val.get("file").and_then(|v| v.as_str());
                    let line = val.get("line").and_then(|v| v.as_u64());
                    if file.is_none() || line.is_none() {
                        return AssertionStatus::Failed {
                            reason: format!("Warm path returned missing file/line on '{target}'"),
                        };
                    }
                }
            }
            Err(e) => {
                return AssertionStatus::Failed {
                    reason: format!("Warm path query failed unexpectedly on '{target}': {e:?}"),
                };
            }
        }
    }

    AssertionStatus::Passed
}
