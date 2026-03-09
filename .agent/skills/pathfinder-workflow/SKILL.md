---
name: pathfinder-workflow
description: Structured workflows for using Pathfinder MCP tools effectively. Use when exploring a codebase, refactoring code, implementing features, or auditing code quality.
---

# Pathfinder Workflow Skill

## Purpose

Provides task-specific tool chains that leverage Pathfinder's semantic tools for maximum effectiveness. Each workflow is a concrete sequence of tool calls optimized for a specific goal.

## When to Invoke

- Starting work on an **unfamiliar codebase** → use Explore workflow
- **Refactoring** a function or class → use Refactor workflow
- **Implementing a new feature** → use Implement workflow
- **Auditing code quality** → use Audit workflow
- **Debugging** a failing function → use Debug workflow

---

## Workflow 1: Explore (Understand a Codebase)

**Goal:** Build a mental model of the project from zero context.

```
Step 1: get_repo_map(path=".", depth=3, visibility="public")
        → Get the full project skeleton with semantic paths
        → Note the tech_stack and files_scanned to understand project scale

Step 2: search_codebase(query="<entry point pattern>", path_glob="src/**/*")
        → Find main entry points, API handlers, or CLI commands
        → Use enclosing_semantic_path from results for next step

Step 3: read_with_deep_context(semantic_path="<chosen entry point>")
        → Read the entry point + all functions it calls
        → Follow the dependency chain to understand data flow

Step 4: analyze_impact(semantic_path="<key function>", max_depth=2)
        → Understand who calls this function (incoming)
        → Understand what it depends on (outgoing)
```

**When to stop:** You can explain the project's architecture, identify its core modules, and trace a request through the system.

---

## Workflow 2: Refactor (Safely Change Code)

**Goal:** Modify existing code without breaking callers or dependencies.

```
Step 1: read_with_deep_context(semantic_path="<target>")
        → Read the target function and everything it calls
        → Save the version_hash for editing

Step 2: analyze_impact(semantic_path="<target>", max_depth=2)
        → Identify ALL callers — these are your blast radius
        → Version hashes are returned for all referenced files

Step 3: validate_only(semantic_path="<target>", edit_type="replace_body",
                      new_code="<your refactored code>", base_version="<hash>")
        → Dry-run the edit to check for LSP errors BEFORE writing

Step 4: replace_body(semantic_path="<target>",
                     new_code="<your refactored code>", base_version="<hash>")
        → Apply the edit with semantic addressing + auto-indentation
        → Check the validation result in the response

Step 5: (If callers need updating) For each caller from Step 2:
        read_symbol_scope → replace_body → verify

Step 6: run_command("cargo test" / "npm test")  [built-in tool]
        → Verify the refactoring didn't break anything
```

**Key rule:** ALWAYS run `analyze_impact` before refactoring. Agents that skip this step risk breaking unknown callers.

---

## Workflow 3: Implement (Add New Code)

**Goal:** Add a new function, class, or feature to an existing codebase.

```
Step 1: get_repo_map(path="<relevant directory>")
        → Understand existing structure and naming patterns
        → Identify the right file to add the new code to

Step 2: read_symbol_scope(semantic_path="<neighboring function>")
        → Read an existing function in the same file for style reference
        → Save the version_hash

Step 3: insert_after(semantic_path="<anchor symbol>",
                     new_code="<your new function>", base_version="<hash>")
        → Add the new code after an appropriate symbol
        → Use bare file path (no "::") to append at EOF

Step 4: (If adding imports) insert_before(semantic_path="<filepath>",
                     new_code="<import statements>", base_version="<new hash>")
        → Use bare file path to insert at the top of the file

Step 5: run_command("cargo test" / "npm test")  [built-in tool]
```

---

## Workflow 4: Audit (Review Code Quality)

**Goal:** Systematically review a codebase for issues.

```
Step 1: get_repo_map(path=".", depth=4, visibility="all")
        → Get complete project overview including private symbols

Step 2: For each module/feature area:
        a. read_symbol_scope(semantic_path="<public API function>")
           → Review the public interface
        b. read_with_deep_context(semantic_path="<complex function>")
           → Check that dependencies are reasonable
        c. search_codebase(query="unwrap|expect|panic", path_glob="src/**/*.rs")
           → Find potential crash points
        d. search_codebase(query="TODO|FIXME|HACK", filter_mode="comments_only")
           → Find technical debt markers

Step 3: For critical findings:
        analyze_impact(semantic_path="<problematic function>")
        → Assess blast radius before recommending changes
```

---

## Workflow 5: Debug (Trace a Problem)

**Goal:** Understand why a specific function is failing.

```
Step 1: read_with_deep_context(semantic_path="<failing function>")
        → See the function AND all its dependencies

Step 2: get_definition(semantic_path="<suspicious call within the function>")
        → Jump to the definition of a called function to inspect its contract

Step 3: analyze_impact(semantic_path="<failing function>", max_depth=1)
        → Find all callers to understand what inputs are being passed

Step 4: search_codebase(query="<error message or pattern>")
        → Find where the error originates in the codebase
```

---

## Error Recovery Patterns

### SYMBOL_NOT_FOUND

```
Error: SYMBOL_NOT_FOUND for "src/auth.ts::AuthServce.login"
       did_you_mean: ["AuthService.login", "AuthService.logout"]

Recovery:
→ Use the corrected path from did_you_mean
→ Retry: read_symbol_scope(semantic_path="src/auth.ts::AuthService.login")
```

### VERSION_MISMATCH

```
Error: VERSION_MISMATCH — file was modified since your last read

Recovery:
→ Re-read the file: read_symbol_scope(semantic_path="<target>")
→ Get the fresh version_hash
→ Retry the edit with the new base_version
```

### Validation Failures (introduced_errors)

```
Response: validation.status = "failed"
          introduced_errors: [{ message: "cannot find name 'foo'", ... }]

Recovery:
→ Read the introduced_errors to understand what broke
→ Fix your new_code to address the errors
→ Use validate_only to dry-run before committing the fix
→ Apply the corrected edit
```

---

## Tool Chain Quick Reference

| I want to... | Tool chain |
|---|---|
| Understand a new project | `get_repo_map` → `read_with_deep_context` |
| Find and read a function | `search_codebase` → `read_symbol_scope` |
| Edit a function body | `read_symbol_scope` → `replace_body` |
| Add a new function to a file | `read_symbol_scope` (neighbor) → `insert_after` |
| Rename/restructure a function | `analyze_impact` → `replace_full` (+ update callers) |
| Delete a function safely | `analyze_impact` → `delete_symbol` |
| Check an edit before applying | `read_symbol_scope` → `validate_only` → (if ok) → `replace_body` |
| Find all usages before refactoring | `analyze_impact` (max_depth=2) |
| Add imports to a file | `insert_before` (bare file path, no `::`) |
| Append a class to end of file | `insert_after` (bare file path, no `::`) |
