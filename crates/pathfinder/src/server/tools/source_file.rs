//! `read` tool (source file mode) — AST-based full file symbol extraction via Tree-sitter.

use crate::server::helpers::{
    millis_to_u64, pathfinder_to_error_data, serialize_metadata, treesitter_error_to_error_data,
};
use crate::server::types::{ReadParams, ReadSourceFileMetadata, SourceSymbol};
use crate::server::PathfinderServer;

use rmcp::model::{CallToolResult, Content, ErrorData};

fn map_symbols(
    syms: Vec<pathfinder_treesitter::surgeon::ExtractedSymbol>,
    filepath: &str,
) -> Vec<SourceSymbol> {
    syms.into_iter()
        .map(|s| SourceSymbol {
            name: s.name,
            semantic_path: format!("{}::{}", filepath, s.semantic_path),
            kind: format!("{:?}", s.kind),
            start_line: s.start_line + 1, // AST lines are 0-indexed, UI is 1-indexed
            end_line: s.end_line + 1,
            children: map_symbols(s.children, filepath),
        })
        .collect()
}

/// Render a tree-like representation of symbols for text output.
///
/// Output format:
/// ```text
/// src/main.rs (12 symbols)
/// ├── main [fn] L1-L45
/// ├── Config [struct] L47-L62
/// │   ├── name [field] L48
/// │   └── value [field] L49
/// └── parse [fn] L64-L80
/// ```
fn render_symbol_tree(symbols: &[SourceSymbol], file_path: &str) -> String {
    let mut lines = Vec::new();
    lines.push(format!("{} ({} symbols)", file_path, symbols.len()));

    // Render top-level symbols
    for (i, sym) in symbols.iter().enumerate() {
        let is_last = i == symbols.len() - 1;
        let connector = if is_last { "└── " } else { "├── " };
        let child_prefix = if is_last { "    " } else { "│   " };

        lines.push(format!(
            "{}{} [{}] L{}-L{} ({})",
            connector, sym.name, sym.kind, sym.start_line, sym.end_line, sym.semantic_path
        ));

        // Render children recursively
        render_recursive(&sym.children, child_prefix, &mut lines);
    }

    lines.join("\n")
}

/// Helper function to render symbol tree recursively.
fn render_recursive(symbols: &[SourceSymbol], prefix: &str, output: &mut Vec<String>) {
    for (i, sym) in symbols.iter().enumerate() {
        let is_last_item = i == symbols.len() - 1;
        let connector = if is_last_item {
            "└── "
        } else {
            "├── "
        };
        let child_prefix = if is_last_item { "    " } else { "│   " };

        output.push(format!(
            "{}{}{} [{}] L{}-L{}",
            prefix, connector, sym.name, sym.kind, sym.start_line, sym.end_line
        ));

        if !sym.children.is_empty() {
            render_recursive(&sym.children, &format!("{prefix}{child_prefix}"), output);
        }
    }
}

fn map_symbols_compact(
    syms: Vec<pathfinder_treesitter::surgeon::ExtractedSymbol>,
    filepath: &str,
) -> Vec<SourceSymbol> {
    syms.into_iter()
        .map(|s| SourceSymbol {
            name: s.name,
            semantic_path: format!("{}::{}", filepath, s.semantic_path),
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
            String::default()
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
    #[tracing::instrument(skip(self, params), fields(file = %params.filepath.as_deref().unwrap_or("")))]
    pub(crate) async fn read_source_file_impl(
        &self,
        params: ReadParams,
    ) -> Result<CallToolResult, ErrorData> {
        let start = std::time::Instant::now();
        tracing::info!(tool = "read_source_file", "read_source_file: start");

        let filepath = params
            .filepath
            .as_ref()
            .ok_or_else(|| rmcp::model::ErrorData::invalid_params("filepath is required", None))?;
        let file_path = std::path::Path::new(filepath);

        if let Err(e) = self.sandbox.check(file_path) {
            tracing::warn!(tool = "read_source_file", error = %e, "sandbox check failed");
            return Err(pathfinder_to_error_data(&e));
        }

        let ts_start = std::time::Instant::now();
        match self
            .surgeon
            .read_source_file(self.workspace_root.path(), file_path)
            .await
        {
            Ok((content, language, symbols)) => {
                let tree_sitter_ms = ts_start.elapsed().as_millis();
                let duration_ms = start.elapsed().as_millis();
                self.lawyer_touch_language_for_file(file_path);

                Ok(build_supported_response(
                    content,
                    language,
                    symbols,
                    &params,
                    duration_ms,
                    tree_sitter_ms,
                ))
            }
            Err(e) => {
                let tree_sitter_ms = ts_start.elapsed().as_millis();
                let duration_ms = start.elapsed().as_millis();

                if let pathfinder_treesitter::error::SurgeonError::UnsupportedLanguage(_) = e {
                    self.handle_unsupported_language_fallback(
                        file_path,
                        &params,
                        tree_sitter_ms,
                        start,
                    )
                    .await
                } else {
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

    /// LT-4: Touch LSP idle timer for supported languages.
    fn lawyer_touch_language_for_file(&self, file_path: &std::path::Path) {
        if let Some(ext) = file_path.extension().and_then(|e| e.to_str()) {
            let lang_id = match ext {
                "rs" => Some("rust"),
                "go" => Some("go"),
                "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" | "vue" => Some("typescript"),
                "py" | "pyi" => Some("python"),
                "java" => Some("java"),
                _ => None,
            };
            if let Some(lang) = lang_id {
                self.lawyer.touch_language(lang);
            }
        }
    }

    /// Graceful fallback: read raw file content without AST parsing.
    #[tracing::instrument(skip(self, params, start), fields(file = %params.filepath.as_deref().unwrap_or("")))]
    async fn handle_unsupported_language_fallback(
        &self,
        file_path: &std::path::Path,
        params: &ReadParams,
        tree_sitter_ms: u128,
        start: std::time::Instant,
    ) -> Result<CallToolResult, ErrorData> {
        let abs_path = self.workspace_root.path().join(file_path);
        let language = file_path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("unknown")
            .to_lowercase();

        let raw_content = match tokio::fs::read_to_string(&abs_path).await {
            Ok(c) => c,
            Err(io_err) => {
                let duration_ms = start.elapsed().as_millis();
                tracing::warn!(
                    tool = "read_source_file",
                    error = %io_err,
                    tree_sitter_ms,
                    duration_ms,
                    "read_source_file: unsupported language + failed to read raw"
                );
                return Err(treesitter_error_to_error_data(
                    pathfinder_treesitter::error::SurgeonError::Io(std::sync::Arc::new(io_err)),
                ));
            }
        };

        let content = truncate_content(&raw_content, params.start_line, params.end_line);
        let duration_ms = start.elapsed().as_millis();

        tracing::info!(
            tool = "read_source_file",
            tree_sitter_ms,
            duration_ms,
            %language,
            engines_used = ?["raw_file"],
            "read_source_file: graceful fallback for unsupported language"
        );

        let text = format!(
            "{content}\n[completed in {duration_ms}ms; unsupported language: raw content only]"
        );

        let metadata = ReadSourceFileMetadata {
            language,
            content: Some(content),
            symbols: vec![],
            duration_ms: Some(millis_to_u64(duration_ms)),
            unsupported_language: Some(true),
        };

        let mut result = CallToolResult::success(vec![Content::text(text)]);
        result.structured_content = serialize_metadata(&metadata);

        Ok(result)
    }
}

/// Build response for successfully-parsed file with AST symbols.
fn build_supported_response(
    mut content: String,
    language: String,
    mut symbols: Vec<pathfinder_treesitter::surgeon::ExtractedSymbol>,
    params: &ReadParams,
    duration_ms: u128,
    tree_sitter_ms: u128,
) -> CallToolResult {
    tracing::info!(
        tool = "read_source_file",
        tree_sitter_ms,
        duration_ms,
        engines_used = ?["tree-sitter"],
        "read_source_file: complete"
    );

    let start_idx = params.start_line.saturating_sub(1) as usize;
    if params.start_line > 1 || params.end_line.is_some() {
        content = truncate_content(&content, params.start_line, params.end_line);
        let end_line_0 = params
            .end_line
            .map_or(usize::MAX, |l| l.saturating_sub(1) as usize);
        symbols = filter_symbols(symbols, start_idx, end_line_0);
    }

    let filepath_str = params.filepath.as_deref().unwrap_or("");

    let (final_content, final_symbols) = match params.detail_level.as_str() {
        "source_only" => (Some(content), vec![]),
        "symbols" => {
            let syms = map_symbols(symbols, filepath_str);
            let tree_text = render_symbol_tree(&syms, filepath_str);
            (Some(tree_text), syms)
        }
        "full" => (Some(content), map_symbols(symbols, filepath_str)),
        _ => (Some(content), map_symbols_compact(symbols, filepath_str)),
    };

    let mut contents = Vec::new();
    if let Some(ref text) = final_content {
        contents.push(Content::text(format!(
            "{text}\n[completed in {duration_ms}ms]"
        )));
    }

    let metadata = ReadSourceFileMetadata {
        language,
        content: final_content,
        symbols: final_symbols,
        duration_ms: Some(millis_to_u64(duration_ms)),
        unsupported_language: None,
    };

    let mut result = CallToolResult::success(contents);
    result.structured_content = serialize_metadata(&metadata);

    result
}

#[cfg(test)]
#[path = "source_file_test.rs"]
mod tests;
