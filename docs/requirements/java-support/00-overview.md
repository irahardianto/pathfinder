# Java Support for Pathfinder — Overview & Goals

## Problem Statement

Pathfinder currently supports 5 languages (Rust, Go, TypeScript/JavaScript, Python, Vue). Java is the world's most widely-used enterprise language and adding support for Java 8–25 would dramatically expand Pathfinder's reach.

## Goals

1. **Full Tree-sitter integration** — symbol extraction, repo maps, search, `read_symbol_scope`, `read_source_file` for `.java` files
2. **Full LSP integration** — `get_definition`, `analyze_impact`, `lsp_health` via Eclipse JDT Language Server (jdtls)
3. **Java 8–25 compatibility** — all language features from lambdas (8) through records (16), sealed classes (17), pattern matching (21), and beyond
4. **Zero regression** — existing language support must remain unaffected

## Non-Goals

- Kotlin, Scala, or other JVM language support (separate effort)
- Build system orchestration (Maven/Gradle task execution)
- Annotation processor output analysis (Lombok-generated code, etc.)

## Phased Delivery

| Phase | Scope | External Deps | Risk |
|-------|-------|---------------|------|
| **Phase 0** | `AccessLevel` enum refactoring (prerequisite) | None | Medium |
| **Phase 1** | Tree-sitter Java (symbols, repo map, search) | `tree-sitter-java` crate | Low |
| **Phase 2** | LSP integration (jdtls, navigation) | JDK 21+, jdtls binary | High |

Each phase is independently mergeable and verifiable.

## Key Design Decisions

1. **LSP server**: jdtls (Eclipse JDT Language Server) — only production-grade Java LSP
2. **Semantic paths**: File-relative (`UserService.login`), consistent with all other languages
3. **Visibility model**: New `AccessLevel` enum (`Public`/`Protected`/`Package`/`Private`) replacing `is_public: bool`
4. **JDK requirement**: JDK 21 LTS to *run* jdtls; jdtls can *analyze* Java 8–25 projects

## Document Index

- [00-overview.md](./00-overview.md) — This document
- [01-phase0-access-level.md](./01-phase0-access-level.md) — `AccessLevel` enum refactoring
- [02-phase1-treesitter.md](./02-phase1-treesitter.md) — Tree-sitter Java integration
- [03-phase2-lsp.md](./03-phase2-lsp.md) — jdtls LSP integration
- [04-file-change-manifest.md](./04-file-change-manifest.md) — Every file that needs modification
- [05-risk-matrix.md](./05-risk-matrix.md) — Risk matrix and mitigation strategies
