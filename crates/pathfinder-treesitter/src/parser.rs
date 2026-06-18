use crate::error::SurgeonError;
use crate::language::SupportedLanguage;
use std::cell::RefCell;
use std::ops::ControlFlow;
use std::time::Instant;
use tracing::instrument;
use tree_sitter::{ParseOptions, Parser, Tree};

const PARSE_TIMEOUT_MICROS: u64 = 500_000;

thread_local! {
    static PARSER: RefCell<Parser> = RefCell::new(Parser::new());
}

#[derive(Debug, Default)]
pub struct AstParser;

impl AstParser {
    /// Parse the given source code bytes into a tree-sitter `Tree`.
    ///
    /// Uses a thread-local parser pool to avoid per-call `Parser` allocation.
    /// Tree-sitter `Parser` is `!Send`, so a `thread_local!` `RefCell` is safe.
    ///
    /// # Errors
    ///
    /// Returns a `SurgeonError` if the parser cannot be created or parsing fails.
    #[instrument(skip_all, fields(language = ?lang))]
    pub fn parse_source(
        path: &std::path::Path,
        lang: SupportedLanguage,
        source: &[u8],
    ) -> Result<Tree, SurgeonError> {
        PARSER.with(|cell| {
            let mut parser = cell.borrow_mut();

            parser
                .set_language(&lang.grammar())
                .map_err(|e| SurgeonError::ParseError {
                    path: path.to_path_buf(),
                    reason: format!("Failed to set language: {e}"),
                })?;

            let start = Instant::now();
            let timeout = std::time::Duration::from_micros(PARSE_TIMEOUT_MICROS);
            let source_len = source.len();

            let mut progress_cb = |_state: &tree_sitter::ParseState| {
                if start.elapsed() > timeout {
                    ControlFlow::Break(())
                } else {
                    ControlFlow::Continue(())
                }
            };

            let options = ParseOptions::new().progress_callback(&mut progress_cb);

            let result = parser.parse_with_options(
                &mut |i, _| {
                    if i < source_len {
                        &source[i..]
                    } else {
                        &[]
                    }
                },
                None,
                Some(options),
            );

            result.ok_or_else(|| SurgeonError::ParseError {
                path: path.to_path_buf(),
                reason: "Parser returned None (timed out or no language set)".into(),
            })
        })
    }
}

#[cfg(test)]
#[path = "parser_test.rs"]
mod tests;
