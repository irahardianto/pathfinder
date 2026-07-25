# Pathfinder Project Conventions & Guidelines

## Rust Language & Idioms
- Rust 2021 Edition.
- Use `Default::default()` or `Type::default()` for initializing default collections/types (`Vec::default()`, `HashMap::default()`, `String::default()`, `HashSet::default()`) when implementing `Default` or clearing lint warnings (RS-W1079).
- Document all public items (`struct`, `enum`, `fn`, `trait`, `const`) with `///` doc comments (RS-D1001). Include a `# Errors` section for fallible functions returning `Result`.
- Use `str::split_once` instead of `splitn(2, ...).collect()` (RS-W1066).
- Use `Ordering::then_with` for multi-field comparisons instead of nested `match a.cmp(&b)` (RS-W1219).
- Avoid redundant let-bindings before return statements (`let res = ...; res` -> return directly) (RS-W1218).
- Avoid unnecessary `.clone()` on `Copy` types (RS-W1110).
- Use lazy `.unwrap_or_else` or `.map_or_else` for non-trivial fallback expressions (RS-W1031).
- Extract repeated string literals (e.g. environment variable names like `"CONDA_PREFIX"`, `"VIRTUAL_ENV"`) into module constants (`const ENV_...: &str = ...;`) (RS-W1015).
- Maintain low cyclomatic complexity (cyclomatic complexity < 10 per function). Decompose functions exceeding 100 lines into private helper functions (RS-R1000).

## Code Structure & Modules
- Unit/integration test files are co-located alongside target files or linked via `#[cfg(test)] #[path = "foo_test.rs"] mod tests;`.
- Use `thiserror::Error` for error enums.
- Use `tracing` macros (`tracing::info!`, `tracing::warn!`, `tracing::debug!`, `tracing::error!`) for logging.
- Struct excessive parameters: group parameters into a dedicated config/params struct.
- Type complexity: define clear type aliases (`type TaskResult<T> = Result<T, TaskError>;`).

## Quality & Compliance Mandate
- No forbidden clippy suppressions (`too_many_lines`, `too_many_arguments`, `type_complexity`, `struct_excessive_bools`) in production or test code without explicit architectural justification.
- Maintain 100% compilation and test passing state.
