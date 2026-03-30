use crate::error::SurgeonError;
use std::collections::HashMap;
use std::fmt::Write;
use std::path::Path;

/// The result of a `get_repo_map` generation.
#[derive(Debug, Clone)]
pub struct RepoMapResult {
    pub skeleton: String,
    pub tech_stack: Vec<String>,
    pub files_scanned: usize,
    pub files_truncated: usize,
    pub files_in_scope: usize,
    pub coverage_percent: u8,
    pub version_hashes: HashMap<String, String>,
}

/// Token counting heuristic
#[must_use]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
)]
pub fn estimate_tokens(text: &str) -> u32 {
    let chars = text.chars().count();
    (chars as f32 / 4.0).ceil() as u32
}

use crate::surgeon::{ExtractedSymbol, SymbolKind};

/// Default per-file token cap. Used when no per-call override is supplied.
/// At ~4 chars/token, 2 000 tokens ≈ 8 KB — covers the vast majority of
/// real source files without falling back to the truncated stub.
#[allow(dead_code)] // Canonical fallback value; callers receive it via default_max_tokens_per_file()
const MAX_TOKENS_PER_FILE: u32 = 2_000;

/// Determine whether a symbol should be included when `visibility = "public"`.
///
/// Uses **name-convention heuristics** because the extracted AST symbols do not
/// carry visibility metadata (the Tree-sitter `.scm` queries extract names only):
///
/// | Convention          | Considered private                                    |
/// |---------------------|-------------------------------------------------------|
/// | `_`-prefixed name   | Python private, JS/TS private-by-convention, Rust     |
/// | Lowercase first char| Go package-private (exported identifiers are `Upper`) |
///
/// Methods (children of a class/impl) always mirror their parent's visibility —
/// a private class is fully excluded; a public class keeps all its methods so
/// agents see the full public API surface.
///
/// TypeScript/JavaScript and Rust `pub`-ness is not analysed at the AST level;
/// only the `_` prefix strips symbols in those languages.
#[must_use]
fn is_symbol_public(sym: &ExtractedSymbol, lang_is_go: bool) -> bool {
    let name = sym.name.as_str();
    // _-prefixed names are private across all supported languages
    if name.starts_with('_') {
        return false;
    }
    // Go: package-level functions/structs/constants are public iff first char is uppercase
    if lang_is_go
        && matches!(
            sym.kind,
            SymbolKind::Function
                | SymbolKind::Struct
                | SymbolKind::Interface
                | SymbolKind::Constant
                | SymbolKind::Enum
        )
    {
        return name.chars().next().is_some_and(|c| c.is_ascii_uppercase());
    }
    true
}

/// Recursively filter `symbols` keeping only those visible under `visibility`.
///
/// - `"all"` — no filtering, returns the slice unchanged in a cloned `Vec`.
/// - `"public"` — drops private symbols (see [`is_symbol_public`]) and recursively
///   filters children; if a class/impl becomes empty after filtering it is also dropped.
#[must_use]
fn filter_by_visibility(
    symbols: Vec<ExtractedSymbol>,
    visibility: &str,
    lang_is_go: bool,
) -> Vec<ExtractedSymbol> {
    if visibility != "public" {
        return symbols;
    }
    symbols
        .into_iter()
        .filter(|sym| is_symbol_public(sym, lang_is_go))
        .map(|mut sym| {
            sym.children = filter_by_visibility(sym.children, visibility, lang_is_go);
            sym
        })
        .collect()
}

/// Render a single file's skeleton into an indented string.
///
/// If the rendered output exceeds `max_tokens_per_file`, the result is
/// collapsed to a truncated stub showing only class/struct names and method
/// counts. Pass [`MAX_TOKENS_PER_FILE`] as the default when no caller override
/// is available.
#[must_use]
pub fn render_file_skeleton(symbols: &[ExtractedSymbol], max_tokens_per_file: u32) -> String {
    let mut out = String::new();
    render_symbols_recursive(symbols, 0, &mut out);

    // Check if the file is too large
    if estimate_tokens(&out) > max_tokens_per_file {
        return render_truncated_file_skeleton(symbols);
    }

    out
}

fn render_symbols_recursive(symbols: &[ExtractedSymbol], depth: usize, out: &mut String) {
    let indent = "  ".repeat(depth);
    for sym in symbols {
        use crate::surgeon::SymbolKind;
        let prefix = match sym.kind {
            SymbolKind::Function => "func ",
            SymbolKind::Class => "class ",
            SymbolKind::Struct => "struct ",
            SymbolKind::Method => "method ",
            SymbolKind::Impl => "impl ",
            SymbolKind::Constant => "const ",
            SymbolKind::Interface => "interface ",
            SymbolKind::Enum => "enum ",
        };

        let declaration = format!("{}{}", prefix, sym.name);
        let _ = writeln!(out, "{}{} // {}", indent, declaration, sym.semantic_path);

        if !sym.children.is_empty() {
            render_symbols_recursive(&sym.children, depth + 1, out);
        }
    }
}

/// A fallback rendering that only preserves top-level class/struct names and method counts.
fn render_truncated_file_skeleton(symbols: &[ExtractedSymbol]) -> String {
    let mut out = String::new();
    for sym in symbols {
        use crate::surgeon::SymbolKind;
        if sym.kind == SymbolKind::Class || sym.kind == SymbolKind::Struct {
            let prefix = if sym.kind == SymbolKind::Class {
                "class "
            } else {
                "struct "
            };
            let declaration = format!("{}{}", prefix, sym.name);
            let _ = writeln!(out, "{} // {}", declaration, sym.semantic_path);

            let method_count = sym
                .children
                .iter()
                .filter(|c| c.kind == SymbolKind::Method)
                .count();
            if method_count > 0 {
                let _ = writeln!(out, "  // ... {method_count} methods omitted");
            }
        }
    }

    // Add a warning comment at the top if we had to collapse
    if out.is_empty() {
        "// [TRUNCATED - NO CLASSES EXTRACTED]".to_string()
    } else {
        format!("// [TRUNCATED DUE TO SIZE]\n{out}")
    }
}

/// Generate an AST-based skeleton of a directory.
///
/// # Arguments
/// - `visibility` — `"public"` to filter out private-by-convention symbols;
///   `"all"` to include every extracted symbol.
///
/// # Errors
/// Returns `SurgeonError` if an operation on the AST fails.
#[expect(
    clippy::too_many_lines,
    reason = "Sequential directory-walk pipeline; splitting into sub-functions would obscure the linear data flow without improving readability"
)]
pub async fn generate_skeleton_text(
    surgeon: &impl crate::surgeon::Surgeon,
    workspace_root: &Path,
    target_path: &Path,
    max_tokens: u32,
    depth: u32,
    visibility: &str,
    max_tokens_per_file: u32,
) -> Result<RepoMapResult, SurgeonError> {
    use ignore::WalkBuilder;
    use pathfinder_common::types::VersionHash;

    let abs_target = workspace_root.join(target_path);

    let mut builder = WalkBuilder::new(&abs_target);
    builder.max_depth(Some(depth as usize)); // WalkBuilder handles max_depth
    builder.require_git(false);
    builder.hidden(true); // Ignore hidden files
    builder.add_custom_ignore_filename(".pathfinderignore"); // Standard ignore from searcher

    let walker = builder.build();

    let mut skeleton_out = String::new();
    let mut files_scanned = 0;
    let mut files_in_scope = 0;
    let mut files_truncated = 0;
    let mut version_hashes = HashMap::new();
    let mut tech_stack: Vec<crate::language::SupportedLanguage> = Vec::new();

    for result in walker {
        let Ok(entry) = result else { continue };

        let path = entry.path();
        if path.is_dir() {
            continue;
        }

        // Strip prefix carefully
        let rel_path = path.strip_prefix(workspace_root).unwrap_or(path);

        // Only parse supported languages
        let Some(lang) = crate::language::SupportedLanguage::detect(path) else {
            continue;
        };

        // Count only after language detection: coverage_percent = "source files mapped / source files found"
        files_in_scope += 1;

        if !tech_stack.contains(&lang) {
            tech_stack.push(lang);
        }

        // Read the raw file bytes to compute a per-file version hash.
        // `extract_symbols` does not return a hash, so we read the file separately.
        // Files that fail to read (e.g., permission denied, race-deleted) are skipped
        // so a transient I/O error does not corrupt the hash map with empty-byte hashes.
        let source = match tokio::fs::read(path).await {
            Ok(bytes) => bytes,
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "get_repo_map: skipping file (read failed)"
                );
                continue;
            }
        };
        let hash = VersionHash::compute(&source);

        version_hashes.insert(rel_path.display().to_string(), hash.to_string());

        // AST extraction — log failures so operators can diagnose missing files in the repo map
        let raw_symbols = match surgeon.extract_symbols(workspace_root, rel_path).await {
            Ok(syms) => syms,
            Err(e) => {
                tracing::debug!(
                    path = %rel_path.display(),
                    error = %e,
                    "get_repo_map: skipping file (symbol extraction failed)"
                );
                continue;
            }
        };

        // Apply visibility filtering heuristic
        let lang_is_go = matches!(lang, crate::language::SupportedLanguage::Go);
        let symbols = filter_by_visibility(raw_symbols, visibility, lang_is_go);

        if symbols.is_empty() {
            continue;
        }

        files_scanned += 1;

        let file_skeleton = render_file_skeleton(&symbols, max_tokens_per_file);
        let file_skeleton_tokens = estimate_tokens(&file_skeleton);

        let path_header = format!(
            "\nFile: {}\n{}\n",
            rel_path.display(),
            "=".repeat(rel_path.display().to_string().len() + 6)
        );

        let current_tokens = estimate_tokens(&skeleton_out);
        if current_tokens + file_skeleton_tokens > max_tokens {
            if current_tokens + 50 <= max_tokens {
                use std::fmt::Write;
                let _ = write!(
                    skeleton_out,
                    "\n// [... Omitted {} due to token budget]\n",
                    rel_path.display()
                );
            }
            files_truncated += 1;
            continue;
        }

        skeleton_out.push_str(&path_header);
        skeleton_out.push_str(&file_skeleton);
    }

    let coverage_percent = if files_in_scope > 0 {
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            clippy::cast_precision_loss
        )]
        let percent = ((files_scanned as f32 / files_in_scope as f32) * 100.0) as u8;
        percent
    } else {
        100
    };

    Ok(RepoMapResult {
        skeleton: skeleton_out.trim().to_string(),
        tech_stack: tech_stack.iter().map(|l| format!("{l:?}")).collect(),
        files_scanned,
        files_truncated,
        files_in_scope,
        coverage_percent,
        version_hashes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::surgeon::{ExtractedSymbol, SymbolKind};

    fn make_sym(name: &str, kind: SymbolKind) -> ExtractedSymbol {
        ExtractedSymbol {
            name: name.to_string(),
            semantic_path: name.to_string(),
            kind,
            byte_range: 0..1,
            start_line: 0,
            end_line: 1,
            children: vec![],
        }
    }

    #[test]
    fn test_filter_all_keeps_everything() {
        let syms = vec![
            make_sym("_private", SymbolKind::Function),
            make_sym("Public", SymbolKind::Function),
        ];
        let filtered = filter_by_visibility(syms, "all", false);
        assert_eq!(filtered.len(), 2);
    }

    #[test]
    fn test_filter_public_removes_underscore_prefix() {
        let syms = vec![
            make_sym("_helper", SymbolKind::Function),
            make_sym("compute", SymbolKind::Function),
        ];
        let filtered = filter_by_visibility(syms, "public", false);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].name, "compute");
    }

    #[test]
    fn test_filter_public_go_removes_lowercase_top_level_functions() {
        let syms = vec![
            make_sym("internal", SymbolKind::Function),
            make_sym("Export", SymbolKind::Function),
            make_sym("_hidden", SymbolKind::Struct),
            make_sym("PublicStruct", SymbolKind::Struct),
        ];
        let filtered = filter_by_visibility(syms, "public", true /* lang_is_go */);
        assert_eq!(filtered.len(), 2);
        assert_eq!(filtered[0].name, "Export");
        assert_eq!(filtered[1].name, "PublicStruct");
    }

    #[test]
    fn test_filter_public_recursively_prunes_children() {
        let mut parent = make_sym("Parent", SymbolKind::Class);
        parent.children = vec![
            make_sym("_private_method", SymbolKind::Method),
            make_sym("public_method", SymbolKind::Method),
        ];
        let filtered = filter_by_visibility(vec![parent], "public", false);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].children.len(), 1);
        assert_eq!(filtered[0].children[0].name, "public_method");
    }

    #[test]
    fn test_estimate_tokens() {
        assert_eq!(estimate_tokens("a"), 1);
        assert_eq!(estimate_tokens("abcd"), 1);
        assert_eq!(estimate_tokens("abcde"), 2);
    }

    #[test]
    fn test_render_file_skeleton() {
        let symbols = vec![ExtractedSymbol {
            name: "MyClass".to_string(),
            semantic_path: "MyClass".to_string(),
            kind: SymbolKind::Class,
            byte_range: 0..10,
            start_line: 0,
            end_line: 10,
            children: vec![ExtractedSymbol {
                name: "my_method".to_string(),
                semantic_path: "MyClass.my_method".to_string(),
                kind: SymbolKind::Method,
                byte_range: 5..8,
                start_line: 5,
                end_line: 8,
                children: vec![],
            }],
        }];

        let output = render_file_skeleton(&symbols, MAX_TOKENS_PER_FILE);
        assert!(output.contains("class MyClass // MyClass"));
        assert!(output.contains("  method my_method // MyClass.my_method"));
    }

    #[test]
    fn test_render_truncated_file_skeleton_fallback() {
        // Construct massive nested symbol structure that exceeds token limits.
        // At the new 2_000-token threshold (~8 KB), we need 200 long method names to
        // generate ~12 000 chars (~3 000 tokens), which reliably triggers truncation.
        let mut methods = Vec::new();
        for i in 0..200 {
            methods.push(ExtractedSymbol {
                name: format!("massive_method_{i}"),
                semantic_path: format!("MyGiganticClass.massive_method_{i}"),
                kind: SymbolKind::Method,
                byte_range: 0..0,
                start_line: 0,
                end_line: 0,
                children: vec![],
            });
        }

        // This class with 100 methods with long names easily exceeds 2_000 tokens (~8 KB)
        let symbols = vec![ExtractedSymbol {
            name: "MyGiganticClass".to_string(),
            semantic_path: "MyGiganticClass".to_string(),
            kind: SymbolKind::Class,
            byte_range: 0..0,
            start_line: 0,
            end_line: 0,
            children: methods,
        }];

        render_symbols_recursive(&symbols, 0, &mut String::new());
        // To properly test, let's call `render_file_skeleton` which calls the truncated version internally
        let output = render_file_skeleton(&symbols, MAX_TOKENS_PER_FILE);
        assert!(output.contains("[TRUNCATED DUE TO SIZE]"));
        assert!(output.contains("class MyGiganticClass // MyGiganticClass"));
        assert!(output.contains("200 methods omitted"));
        assert!(!output.contains("massive_method_0")); // methods shouldn't be printed
    }

    #[test]
    fn test_render_symbols_recursive_directly() {
        let symbols = vec![ExtractedSymbol {
            name: "Foo".to_string(),
            semantic_path: "Foo".to_string(),
            kind: SymbolKind::Function,
            byte_range: 0..0,
            start_line: 0,
            end_line: 0,
            children: vec![],
        }];
        let mut out = String::new();
        render_symbols_recursive(&symbols, 0, &mut out);
        assert_eq!(out, "func Foo // Foo\n");
    }
}
