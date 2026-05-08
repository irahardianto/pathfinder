# Risk Matrix & Mitigation Strategies

## Risk Assessment

| ID | Risk | Phase | Likelihood | Impact | Severity | Mitigation |
|----|------|-------|-----------|--------|----------|------------|
| R1 | `AccessLevel` refactoring breaks existing tests | P0 | Medium | High | **High** | Mechanical change — run `cargo test` after each file. Tests are the safety net. |
| R2 | Naming collision: new `AccessLevel` confused with existing `Visibility` enum | P0 | Low | Medium | **Medium** | Use distinct name `AccessLevel` in `surgeon.rs`; existing `Visibility` stays in `pathfinder-common/types.rs` as repo-map filter. |
| R3 | Go visibility semantics change (uppercase heuristic moves from repo_map to extraction) | P0 | Medium | Medium | **Medium** | Current `is_symbol_public` in `repo_map.rs` uses Go uppercase heuristic at render time. Moving to `detect_access_level` at extraction time is more accurate but changes when the check runs. Verify with existing Go tests. |
| R4 | `pub(crate)` mod test assertion changes from `true` → `Package` | P0 | Certain | Low | **Low** | Intentional semantic improvement. Document in PR description. |
| R5 | tree-sitter-java grammar missing Java 22-25 syntax nodes | P1 | Low | Low | **Low** | tree-sitter-java 0.23.5 tracks JDK releases. If a node is missing, symbol extraction degrades gracefully (skips unknown nodes). |
| R6 | Anonymous class extraction produces garbage symbols | P1 | Medium | Medium | **Medium** | Anonymous classes have no `name` field → `resolve_name_node()` returns `None` → extraction skips them. Add explicit test case to verify. |
| R7 | Java inner class nesting produces incorrect semantic paths | P1 | Medium | Medium | **Medium** | `extract_nested_symbols` already handles nesting for TS/Rust classes. Test with `InnerClasses.java` fixture. |
| R8 | Java generics `<T>` interfere with name resolution | P1 | Low | Medium | **Medium** | `type_parameters` is a separate node from `name` in tree-sitter-java — `child_by_field_name("name")` returns the class/method name without generics. Verify with `GenericClass.java` fixture. |
| R9 | jdtls launch requires complex equinox launcher pattern | P2 | High | High | **Critical** | Many distros package jdtls as a wrapper script (`jdtls` binary that handles launcher internally). If not, `spawn_lsp_child` needs custom args. **Run spike first** to determine exact invocation. |
| R10 | jdtls startup time (5-30s) causes timeout on large projects | P2 | High | Medium | **High** | Set `init_timeout_secs: 180` for Java. Add progress watcher integration (jdtls sends `$/progress`). |
| R11 | jdtls needs per-workspace data directory | P2 | Certain | High | **High** | Create `.pathfinder/jdtls-data/` per workspace. Already covered by `.pathfinder/` in gitignore. |
| R12 | `LanguageLsp.python_path` → `init_options` migration breaks Python LSP | P2 | Medium | High | **High** | Run full Python LSP integration test after migration. The pyright `pythonPath` init option must still be sent correctly. Also verify: if `init_options` is non-null for TypeScript (unlikely but possible), it must not override the `plugins` branch in `build_initialize_request`. |
| R13 | `detect_languages` grows to 400+ lines, becoming unmaintainable | P2 | Certain | Medium | **Medium** | Accept for now. Java detection block should follow the exact existing pattern (macros, marker scanning, validation, missing-language tracking). Document as tech debt for future plugin-driven refactor. |
| R14 | Java build system marker file false positives | P2 | Low | Low | **Low** | `validate_marker_file` checks for `<project` in pom.xml. Gradle files only check non-empty. `settings.gradle` may match non-Java Gradle projects (Android/Kotlin-only) — acceptable false positive, jdtls will idle harmlessly. |
| R15 | jdtls sends `workspace/configuration` requests that Pathfinder doesn't handle | P2 | High | Medium | **High** | jdtls dynamically queries settings via `workspace/configuration`. Without a handler, it may use defaults or fail silently. The **mandatory spike** must determine if a no-op handler is needed. |

## Critical Path Items

```
R9 (jdtls launch) ──► MANDATORY spike BEFORE Phase 2 implementation
R15 (workspace/configuration) ──► Must be resolved during jdtls spike
R12 (Python migration) ──► Must be tested immediately after LanguageLsp change
R1 (AccessLevel tests) ──► Must pass before Phase 1 begins
```

## Verification Plan

### Phase 0 Verification
```bash
cargo test --workspace                    # All tests pass
cargo clippy --workspace -- -D warnings   # Zero warnings
cargo fmt --check                         # Formatting clean
```

### Phase 1 Verification
```bash
cargo test --workspace                    # All tests including new Java tests pass
cargo test -p pathfinder-mcp-treesitter   # Tree-sitter crate specifically
# Manual: run `get_repo_map` on a Java project and verify symbols
```

### Phase 2 Verification
```bash
cargo test --workspace                    # All tests pass
cargo test -p pathfinder-mcp-lsp          # LSP crate specifically
# Manual: with jdtls installed, verify:
#   - lsp_health shows java as detected
#   - get_definition works on a Java project
#   - analyze_impact returns callers/callees
```

## Edge Cases Checklist

- [ ] `module-info.java` (Java 9+) — parsed but no symbols extracted (no crash)
- [ ] `package-info.java` — parsed but typically only annotations (no crash)
- [ ] Java file with no class (just package + imports) — empty symbol list, no crash
- [ ] Java file with multiple top-level classes — all classes extracted
- [ ] Enum constants with anonymous class bodies — enum extracted, anonymous bodies skipped
- [ ] Method with lambda body — method extracted, lambda not a separate symbol
- [ ] Static initializer blocks — not extracted (no name), no crash
- [ ] Annotation with nested annotation type — both extracted
- [ ] Record with compact constructor — record and constructor extracted
- [ ] Interface with static methods (Java 8+) — interface with method children
- [ ] Abstract class with abstract methods — class with method children
- [ ] Class extending generic type (`class Foo extends Bar<String>`) — name is `Foo`, not `Foo extends Bar<String>`
- [ ] Wildcard imports (`import java.util.*`) — not symbols, ignored
- [ ] Try-with-resources, enhanced for-loop — not symbols, ignored
- [ ] Very large Java file (10000+ lines) — perf test, should complete in <1s
