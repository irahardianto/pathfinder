//! `read_source_file` tool — AST-based full file symbol extraction via Tree-sitter.

use crate::server::helpers::{pathfinder_to_error_data, treesitter_error_to_error_data};
use crate::server::types::{ReadSourceFileParams, ReadSourceFileResponse, SourceSymbol};
use crate::server::PathfinderServer;
use rmcp::handler::server::wrapper::Json;
use rmcp::model::ErrorData;

fn map_symbols(syms: Vec<pathfinder_treesitter::surgeon::ExtractedSymbol>) -> Vec<SourceSymbol> {
    syms.into_iter()
        .map(|s| SourceSymbol {
            name: s.name,
            semantic_path: s.semantic_path,
            kind: format!("{:?}", s.kind),
            start_line: s.start_line + 1, // AST lines are 0-indexed, UI is 1-indexed
            end_line: s.end_line + 1,
            children: map_symbols(s.children),
        })
        .collect()
}

fn map_symbols_compact(
    syms: Vec<pathfinder_treesitter::surgeon::ExtractedSymbol>,
) -> Vec<SourceSymbol> {
    syms.into_iter()
        .map(|s| SourceSymbol {
            name: s.name,
            semantic_path: s.semantic_path,
            kind: format!("{:?}", s.kind),
            start_line: s.start_line + 1,
            end_line: s.end_line + 1,
            children: vec![],
        })
        .collect()
}

fn filter_symbols(
    syms: Vec<pathfinder_treesitter::surgeon::ExtractedSymbol>,
    start_line_0: usize,
    end_line_0: usize,
) -> Vec<pathfinder_treesitter::surgeon::ExtractedSymbol> {
    syms.into_iter()
        .filter_map(|mut s| {
            if s.end_line >= start_line_0 && s.start_line <= end_line_0 {
                s.children = filter_symbols(s.children, start_line_0, end_line_0);
                Some(s)
            } else {
                None
            }
        })
        .collect()
}

fn truncate_content(content: &str, start_line: u32, end_line: Option<u32>) -> String {
    let start_idx = start_line.saturating_sub(1) as usize;
    if start_line > 1 || end_line.is_some() {
        let lines: Vec<&str> = content.split_inclusive('\n').collect();
        let end_idx = end_line
            .map_or(lines.len(), |l| l as usize)
            .min(lines.len());

        if start_idx < lines.len() && start_idx < end_idx {
            lines[start_idx..end_idx].concat()
        } else {
            String::new()
        }
    } else {
        content.to_string()
    }
}

impl PathfinderServer {
    /// Core logic for the `read_source_file` tool.
    ///
    /// Performs a sandbox check, then delegates to the `Surgeon` to extract
    /// the AST hierarchy and read the full source context.
    #[tracing::instrument(skip(self, params), fields(file = %params.filepath))]
    pub(crate) async fn read_source_file_impl(
        &self,
        params: ReadSourceFileParams,
    ) -> Result<Json<ReadSourceFileResponse>, ErrorData> {
        let start = std::time::Instant::now();

        tracing::info!(tool = "read_source_file", "read_source_file: start");

        let file_path = std::path::Path::new(&params.filepath);

        // Sandbox check on the file path
        if let Err(e) = self.sandbox.check(file_path) {
            tracing::warn!(tool = "read_source_file", error = %e, "sandbox check failed");
            return Err(pathfinder_to_error_data(&e));
        }

        // Delegate to surgeon
        let ts_start = std::time::Instant::now();
        match self
            .surgeon
            .read_source_file(self.workspace_root.path(), file_path)
            .await
        {
            Ok((mut content, version_hash, language, mut symbols)) => {
                let tree_sitter_ms = ts_start.elapsed().as_millis();

                // Line filtering
                let start_idx = params.start_line.saturating_sub(1) as usize;
                if params.start_line > 1 || params.end_line.is_some() {
                    content = truncate_content(&content, params.start_line, params.end_line);

                    let end_line_0 = params
                        .end_line
                        .map_or(usize::MAX, |l| l.saturating_sub(1) as usize);
                    symbols = filter_symbols(symbols, start_idx, end_line_0);
                }

                // Detail level
                let (final_content, final_symbols) = match params.detail_level.as_str() {
                    "symbols" => (None, map_symbols(symbols)),
                    "full" => (Some(content), map_symbols(symbols)),
                    _ => (Some(content), map_symbols_compact(symbols)), // "compact"
                };

                let duration_ms = start.elapsed().as_millis();
                tracing::info!(
                    tool = "read_source_file",
                    tree_sitter_ms,
                    duration_ms,
                    engines_used = ?["tree-sitter"],
                    "read_source_file: complete"
                );

                Ok(Json(ReadSourceFileResponse {
                    content: final_content,
                    version_hash: version_hash.to_string(),
                    language,
                    symbols: final_symbols,
                }))
            }
            Err(e) => {
                let tree_sitter_ms = ts_start.elapsed().as_millis();
                let duration_ms = start.elapsed().as_millis();
                tracing::warn!(
                    tool = "read_source_file",
                    error = %e,
                    tree_sitter_ms,
                    duration_ms,
                    engines_used = ?["tree-sitter"],
                    "read_source_file: failed"
                );
                Err(treesitter_error_to_error_data(e))
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use pathfinder_treesitter::surgeon::{ExtractedSymbol, SymbolKind};

    fn make_symbol(
        name: &str,
        start_line: usize,
        end_line: usize,
        children: Vec<ExtractedSymbol>,
    ) -> ExtractedSymbol {
        ExtractedSymbol {
            name: name.to_string(),
            semantic_path: name.to_string(),
            kind: SymbolKind::Function,
            byte_range: 0..0,
            start_line,
            end_line,
            children,
        }
    }

    #[test]
    fn test_truncate_content() {
        let content = "line 1\nline 2\nline 3\nline 4\nline 5";

        let c1 = truncate_content(content, 2, Some(4));
        assert_eq!(c1, "line 2\nline 3\nline 4\n"); // Split inclusive keeps newlines

        let c2 = truncate_content(content, 4, None);
        assert_eq!(c2, "line 4\nline 5");

        let c3 = truncate_content(content, 10, Some(15));
        assert_eq!(c3, "");
    }

    #[test]
    fn test_filter_symbols() {
        let syms = vec![
            make_symbol("a", 0, 10, vec![]),
            make_symbol("b", 15, 20, vec![]),
            make_symbol("c", 10, 15, vec![]),
        ];

        // Ranges: overlap 10-15
        let filtered = filter_symbols(syms.clone(), 10, 15);
        assert_eq!(filtered.len(), 3); // All overlap line 10-15

        // Ranges: overlap 11-14
        let filtered2 = filter_symbols(syms, 11, 14);
        assert_eq!(filtered2.len(), 1);
        assert_eq!(filtered2[0].name, "c");
    }

    #[test]
    fn test_map_symbols_modes() {
        let syms = vec![make_symbol(
            "parent",
            0,
            10,
            vec![make_symbol("child", 2, 5, vec![])],
        )];

        let compact = map_symbols_compact(syms.clone());
        assert_eq!(compact.len(), 1);
        assert!(
            compact[0].children.is_empty(),
            "Compact should drop children"
        );

        let full = map_symbols(syms);
        assert_eq!(full.len(), 1);
        assert_eq!(full[0].children.len(), 1, "Full should keep children");
    }
}
