#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::format_push_string,
    clippy::unnecessary_trailing_comma,
    clippy::too_many_lines
)]

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use pathfinder_treesitter::language::SupportedLanguage;
use pathfinder_treesitter::parser::AstParser;
use pathfinder_treesitter::symbols::extract_symbols_from_tree;

fn generate_rust_source(n_functions: usize) -> Vec<u8> {
    let mut src = String::from("struct Foo { x: i32 }\n\nimpl Foo {\n");
    for i in 0..n_functions {
        src.push_str(&format!(
            "    pub fn method_{i}(&self) -> i32 {{ self.x + {i} }}\n"
        ));
    }
    src.push_str("}\n");
    for i in 0..n_functions {
        src.push_str(&format!("fn standalone_{i}() -> i32 {{ {i} }}\n"));
    }
    src.into_bytes()
}

fn generate_go_source(n_functions: usize) -> Vec<u8> {
    let mut src = String::from("package main\n\n");
    for i in 0..n_functions {
        src.push_str(&format!("func Handle{i:04}() int {{ return {i} }}\n"));
    }
    src.push_str("type Server struct { ID int }\n");
    for i in 0..n_functions {
        src.push_str(&format!(
            "func (s *Server) Process{i:04}() int {{ return s.ID + {i} }}\n"
        ));
    }
    src.into_bytes()
}

fn generate_typescript_source(n_classes: usize, methods_per_class: usize) -> Vec<u8> {
    let mut src = String::from(
        "export abstract class BaseEntity {\n  constructor(public id: string) {}\n}\n\n",
    );
    for i in 0..n_classes {
        src.push_str(&format!(
            "export class Service{i:04} extends BaseEntity {{\n"
        ));
        src.push_str(&format!("  private data{i}: number = {i};\n",));
        for j in 0..methods_per_class {
            src.push_str(&format!(
                "  async process_{j:03}(): Promise<number> {{ return this.data{i} + {j}; }}\n"
            ));
        }
        src.push_str("}\n\n");
    }
    for i in 0..n_classes {
        src.push_str(&format!("const handler{i:04} = () => {{ return {i}; }};\n"));
    }
    src.into_bytes()
}

fn generate_python_source(n_functions: usize) -> Vec<u8> {
    let mut src = String::from("import os\nimport sys\n\n");
    for i in 0..n_functions {
        src.push_str(&format!(
            "def process_{i:04}(data: str) -> str:\n    \"\"\"Process data.{i}\"\"\"\n    return data.strip()\n\n"
        ));
    }
    src.push_str("class DataProcessor:\n");
    for i in 0..n_functions {
        src.push_str(&format!(
            "    def transform_{i:04}(self, value: int) -> int:\n        return value * {i}\n\n"
        ));
    }
    src.into_bytes()
}

fn generate_java_source(n_classes: usize, methods_per_class: usize) -> Vec<u8> {
    let mut src = String::new();
    for i in 0..n_classes {
        src.push_str(&format!(
            "public class Service{i:04} {{\n  private int id = {i};\n\n"
        ));
        for j in 0..methods_per_class {
            src.push_str(&format!(
                "  public int compute_{j:03}() {{ return this.id + {j}; }}\n\n"
            ));
        }
        src.push_str("}\n\n");
    }
    src.into_bytes()
}

fn bench_extraction(c: &mut Criterion) {
    let mut group = c.benchmark_group("extract_symbols");

    for &size in &[50, 200, 500] {
        let source = generate_rust_source(size);
        let tree = AstParser::parse_source(
            std::path::Path::new("bench.rs"),
            SupportedLanguage::Rust,
            &source,
        )
        .expect("parse rust");
        group.throughput(Throughput::Bytes(source.len() as u64));
        group.bench_with_input(
            BenchmarkId::new("rust", size),
            &(&tree, &source),
            |b, (tree, src)| {
                b.iter(|| {
                    extract_symbols_from_tree(
                        black_box(tree),
                        black_box(src),
                        SupportedLanguage::Rust,
                    )
                });
            },
        );

        let source = generate_go_source(size);
        let tree = AstParser::parse_source(
            std::path::Path::new("bench.go"),
            SupportedLanguage::Go,
            &source,
        )
        .expect("parse go");
        group.throughput(Throughput::Bytes(source.len() as u64));
        group.bench_with_input(
            BenchmarkId::new("go", size),
            &(&tree, &source),
            |b, (tree, src)| {
                b.iter(|| {
                    extract_symbols_from_tree(
                        black_box(tree),
                        black_box(src),
                        SupportedLanguage::Go,
                    )
                });
            },
        );

        let source = generate_python_source(size);
        let tree = AstParser::parse_source(
            std::path::Path::new("bench.py"),
            SupportedLanguage::Python,
            &source,
        )
        .expect("parse python");
        group.throughput(Throughput::Bytes(source.len() as u64));
        group.bench_with_input(
            BenchmarkId::new("python", size),
            &(&tree, &source),
            |b, (tree, src)| {
                b.iter(|| {
                    extract_symbols_from_tree(
                        black_box(tree),
                        black_box(src),
                        SupportedLanguage::Python,
                    )
                });
            },
        );
    }

    let methods_per_class = 10;
    for &n_classes in &[10, 50, 100] {
        let source = generate_typescript_source(n_classes, methods_per_class);
        let tree = AstParser::parse_source(
            std::path::Path::new("bench.ts"),
            SupportedLanguage::TypeScript,
            &source,
        )
        .expect("parse ts");
        group.throughput(Throughput::Bytes(source.len() as u64));
        group.bench_with_input(
            BenchmarkId::new("typescript", n_classes),
            &(&tree, &source),
            |b, (tree, src)| {
                b.iter(|| {
                    extract_symbols_from_tree(
                        black_box(tree),
                        black_box(src),
                        SupportedLanguage::TypeScript,
                    )
                });
            },
        );

        let source = generate_java_source(n_classes, methods_per_class);
        let tree = AstParser::parse_source(
            std::path::Path::new("bench.java"),
            SupportedLanguage::Java,
            &source,
        )
        .expect("parse java");
        group.throughput(Throughput::Bytes(source.len() as u64));
        group.bench_with_input(
            BenchmarkId::new("java", n_classes),
            &(&tree, &source),
            |b, (tree, src)| {
                b.iter(|| {
                    extract_symbols_from_tree(
                        black_box(tree),
                        black_box(src),
                        SupportedLanguage::Java,
                    )
                });
            },
        );
    }

    group.finish();
}

criterion_group!(benches, bench_extraction);
criterion_main!(benches);
