## 2024-03-24 - Vue SFC Zone Symbol Rendering Missing Coverage
**Learning:** The symbol rendering logic in `repo_map.rs` iterates over all `SymbolKind` variants, but tests only generated common language symbols (like `Function`, `Class`) without verifying Vue-specific zones (`Zone`, `Component`, `HtmlElement`, `CssSelector`, `CssAtRule`), leaving error-prone mapping arms untested.
**Action:** Always verify that every variant of a core `enum` like `SymbolKind` is exercised in formatting/rendering tests, especially when they represent distinct language paradigms (like Vue SFCs).
