#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::redundant_closure_for_method_calls,
    clippy::semicolon_if_nothing_returned
)]

use criterion::{criterion_group, criterion_main, Criterion};
use std::hint::black_box;

// Import the functions we optimized
// Note: These are from the pathfinder crate, but we're benchmarking the algorithm itself

const DEFINITION_KEYWORDS: &[&str] = &[
    "fn",
    "function",
    "def",
    "struct",
    "class",
    "interface",
    "type",
    "enum",
    "trait",
    "const",
    "static",
    "var",
    "let",
    "mod",
    "impl",
];

#[inline]
fn is_valid_identifier_start(ch: char) -> bool {
    ch.is_ascii_alphabetic() || ch == '_'
}

#[inline]
fn is_valid_identifier_continue(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_'
}

fn extract_identifier_prefix(token: &str) -> Option<&str> {
    let mut chars = token.char_indices();

    let (_, first) = chars.next()?;
    if !is_valid_identifier_start(first) {
        return None;
    }

    let mut end_idx = 1;
    for (idx, ch) in chars {
        if is_valid_identifier_continue(ch) {
            end_idx = idx + 1;
        } else {
            break;
        }
    }

    Some(&token[..end_idx])
}

fn truncate_preview_optimized(content: &str, max_chars: usize) -> String {
    if content.is_empty() {
        return String::new();
    }

    if content.len() <= max_chars {
        return content.to_string();
    }

    if content.is_ascii() {
        return format!("{}...", &content[..max_chars]);
    }

    let Some((idx, _)) = content.char_indices().nth(max_chars) else {
        return content.to_string();
    };
    format!("{}...", &content[..idx])
}

fn truncate_preview_original(content: &str, max_chars: usize) -> String {
    if content.chars().count() > max_chars {
        content.chars().take(max_chars).collect::<String>() + "..."
    } else {
        content.to_string()
    }
}

fn extract_name_from_line_optimized(line: &str) -> String {
    let trimmed = line.trim();
    let tokens: Vec<&str> = trimmed.split_whitespace().collect();

    for window in tokens.windows(2) {
        if DEFINITION_KEYWORDS.contains(&window[0]) {
            if let Some(ident) = extract_identifier_prefix(window[1]) {
                return ident.to_string();
            }
        }
    }

    tokens
        .first()
        .and_then(|s| extract_identifier_prefix(s).map(|i| i.to_string()))
        .unwrap_or_else(|| tokens.first().map(|s| s.to_string()).unwrap_or_default())
}

// Simulate original regex-based implementation
fn extract_name_from_line_original(line: &str) -> String {
    let trimmed = line.trim();

    // Simulate regex compilation + match (the expensive part)
    let regex_pattern = r"(?:fn|function|def|struct|class|interface|type|enum|trait|const|static|var|let|mod|impl)\s+([a-zA-Z_][a-zA-Z0-9_]*)";
    let _ = regex::Regex::new(regex_pattern); // This was the expensive part

    // Simplified: just take first word after keyword
    let tokens: Vec<&str> = trimmed.split_whitespace().collect();
    for window in tokens.windows(2) {
        if DEFINITION_KEYWORDS.contains(&window[0]) {
            return window[1].to_string();
        }
    }
    tokens.first().map(|s| s.to_string()).unwrap_or_default()
}

fn bench_extract_name(c: &mut Criterion) {
    let test_cases = vec![
        "fn my_function() {",
        "struct MyStruct {",
        "class MyClass {",
        "let x = some_value",
        "impl MyTrait for MyStruct {",
        "pub async fn handle_request(req: Request) -> Response {",
        "def calculate_sum(a, b):",
        "function processData(input) {",
        "interface UserService {",
        "type UserID = string",
    ];

    c.bench_function("extract_name_optimized", |b| {
        b.iter(|| {
            for case in &test_cases {
                black_box(extract_name_from_line_optimized(black_box(case)));
            }
        })
    });

    c.bench_function("extract_name_original_regex", |b| {
        b.iter(|| {
            for case in &test_cases {
                black_box(extract_name_from_line_original(black_box(case)));
            }
        })
    });
}

fn bench_truncate_preview(c: &mut Criterion) {
    // ASCII content
    let ascii_content = "a".repeat(1000);
    // Unicode content (CJK characters)
    let unicode_content = "你".repeat(500);
    // Mixed content
    let mixed_content = "Hello世界".repeat(200);

    c.bench_function("truncate_preview_optimized_ascii", |b| {
        b.iter(|| {
            black_box(truncate_preview_optimized(black_box(&ascii_content), 100));
        })
    });

    c.bench_function("truncate_preview_original_ascii", |b| {
        b.iter(|| {
            black_box(truncate_preview_original(black_box(&ascii_content), 100));
        })
    });

    c.bench_function("truncate_preview_optimized_unicode", |b| {
        b.iter(|| {
            black_box(truncate_preview_optimized(black_box(&unicode_content), 100));
        })
    });

    c.bench_function("truncate_preview_original_unicode", |b| {
        b.iter(|| {
            black_box(truncate_preview_original(black_box(&unicode_content), 100));
        })
    });

    c.bench_function("truncate_preview_optimized_mixed", |b| {
        b.iter(|| {
            black_box(truncate_preview_optimized(black_box(&mixed_content), 100));
        })
    });

    c.bench_function("truncate_preview_original_mixed", |b| {
        b.iter(|| {
            black_box(truncate_preview_original(black_box(&mixed_content), 100));
        })
    });
}

criterion_group!(benches, bench_extract_name, bench_truncate_preview);
criterion_main!(benches);
