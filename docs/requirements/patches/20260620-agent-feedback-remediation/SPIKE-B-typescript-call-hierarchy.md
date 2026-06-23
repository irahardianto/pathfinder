# SPIKE-B: TypeScript Call Hierarchy — Findings

## Status: RESOLVED

## Problem

TypeScript and Vue LSP servers (`typescript-language-server`, `vtsls`, `volar`) require
the client to declare `textDocument.callHierarchy` capability during the `initialize`
handshake. Without this declaration, the server does not activate `callHierarchyProvider`
in its response, causing all `callHierarchy/incomingCalls` and `callHierarchy/outgoingCalls`
requests to fail.

This makes the `trace` tool's `callers` and `callees` scopes non-functional for
TypeScript/Vue projects, forcing fallback to grep-based heuristics.

Similarly, `textDocument.references` was not declared, potentially limiting the server's
`referencesProvider` support.

## Root Cause

`build_initialize_request` (process.rs:829) only declared two `textDocument` capabilities:

```json
"textDocument": {
    "definition": { "dynamicRegistration": false, "linkSupport": false },
    "publishDiagnostics": { "relatedInformation": false }
}
```

The `callHierarchy` and `references` capabilities were never registered, even though
the client code (`lawyer_impl.rs`, `capabilities.rs`) already handles `callHierarchyProvider`
detection and dynamic registration.

## Fix

Added both capabilities with `dynamicRegistration: true`:

```json
"textDocument": {
    "definition": { "dynamicRegistration": false, "linkSupport": false },
    "references": { "dynamicRegistration": true },
    "callHierarchy": { "dynamicRegistration": true },
    "publishDiagnostics": { "relatedInformation": false }
}
```

The `callHierarchy` and `references` objects in the client capabilities
signal to `typescript-language-server` that the client supports these
features. TS LS checks for `textDocument?.callHierarchy` during the
`initialize` handshake and statically sets `callHierarchyProvider: true`
in the initialize result (requires TypeScript 3.8.0+). This is a static
capability check, NOT dynamic registration — the `dynamicRegistration:
true` sub-field is harmless but not what triggers enablement. Any value
(even `{}`) for the `callHierarchy` object would satisfy TS LS's check.

> NOTE: As of v0.22.0, no end-to-end test proves TS call hierarchy
> works with a real `typescript-language-server` binary. PATCH-005
> (v2 remediation) adds this test.

## Tests Added

| Test | Validates |
|------|-----------|
| `test_build_initialize_request_declares_call_hierarchy` | `callHierarchy` present with `dynamicRegistration: true` |
| `test_build_initialize_request_declares_references` | `references` capability present |

## Files Changed

- `crates/pathfinder-lsp/src/client/process.rs` — added capabilities to `build_initialize_request`, 2 new tests
