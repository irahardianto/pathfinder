## 2024-11-23 - Command Injection in rg call
**Vulnerability:** Command injection via `symbol_name` argument to `rg`.
**Learning:** The symbol name is parsed from user input (SemanticPath) and passed directly as an argument to `rg` via `tokio::process::Command`. While `Command::new()` passes arguments safely without a shell, a malicious user can provide a symbol name starting with `-` to inject flags (e.g. `--exec=...` or `--... `).
**Prevention:** We should add the `--` argument before `symbol_name` or otherwise sanitize the argument.
