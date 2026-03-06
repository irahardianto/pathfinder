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

use crate::surgeon::ExtractedSymbol;

const MAX_TOKENS_PER_FILE: u32 = 512;

/// Render a single file's skeleton into an indented string.
#[must_use]
pub fn render_file_skeleton(symbols: &[ExtractedSymbol]) -> String {
    let mut out = String::new();
    render_symbols_recursive(symbols, 0, &mut out);

    // Check if the file is too large
    if estimate_tokens(&out) > MAX_TOKENS_PER_FILE {
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
/// # Errors
/// Returns `SurgeonError` if an operation on the AST fails.
pub async fn generate_skeleton_text(
    surgeon: &impl crate::surgeon::Surgeon,
    workspace_root: &Path,
    target_path: &Path,
    max_tokens: u32,
    depth: u32,
    _visibility: &str, // Not yet implemented; all symbols are included regardless of visibility.
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

        files_in_scope += 1;

        // Strip prefix carefully
        let rel_path = path.strip_prefix(workspace_root).unwrap_or(path);

        // Only parse supported languages
        let Some(lang) = crate::language::SupportedLanguage::detect(path) else {
            continue;
        };

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

        // AST extraction
        let Ok(symbols) = surgeon.extract_symbols(workspace_root, rel_path).await else {
            continue;
        };

        if symbols.is_empty() {
            continue;
        }

        files_scanned += 1;

        let file_skeleton = render_file_skeleton(&symbols);
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

        let output = render_file_skeleton(&symbols);
        assert!(output.contains("class MyClass // MyClass"));
        assert!(output.contains("  method my_method // MyClass.my_method"));
    }

    #[test]
    fn test_render_truncated_file_skeleton_fallback() {
        // Construct massive nested symbol structure that exceeds token limits
        let mut methods = Vec::new();
        for i in 0..100 {
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

        // This class with 100 methods with long names easily exceeds 512 tokens (~2000 chars)
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
        let output = render_file_skeleton(&symbols);
        assert!(output.contains("[TRUNCATED DUE TO SIZE]"));
        assert!(output.contains("class MyGiganticClass // MyGiganticClass"));
        assert!(output.contains("100 methods omitted"));
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
