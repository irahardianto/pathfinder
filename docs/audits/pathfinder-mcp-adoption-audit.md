# Pathfinder MCP Adoption Audit

**Date:** 2026-04-25
**Problem:** Agent (Claude) ignores Pathfinder MCP tools despite documentation in AGENTS.md and skills.

---

## Root Cause Analysis

4 compounding failures, each sufficient to cause the problem on its own:

### 1. Discovery Gap — MCP tools are hidden behind a gateway

The MCP tools aren't surfaced as individual tools. They live behind `mcp({ tool: "pathfinder_xxx" })` — a single meta-tool. The agent sees 4 first-class tools (`read`, `bash`, `edit`, `write`) and one opaque `mcp()` function. There's no affordance telling the agent "18 more tools are available inside this gateway."

**Fix applied:** Added §0 to `APPEND_SYSTEM.md` — a mandatory pre-flight check that forces `mcp({ server: "pathfinder" })` before any code task. This section is ALWAYS in context (APPEND_SYSTEM.md is loaded into every session).

### 2. Routing buried at the bottom of AGENTS.md

The "Pathfinder Tool Routing" section was the last section in a 300-line AGENTS.md file. By the time the agent starts executing tool calls, it's operating on cached intent — not re-reading AGENTS.md. The routing table was present but not top-of-mind.

**Fix applied:** Moved the routing table to `APPEND_SYSTEM.md` §0 (always loaded, always visible). Kept the detailed version in AGENTS.md but added a cross-reference pointing to APPEND_SYSTEM.md for the always-on version.

### 3. Skill description too vague

`pathfinder-workflow` had description: "Use when exploring a codebase, refactoring code, implementing features, or auditing code quality effectively." This is generic — "auditing code quality" didn't trigger for "coverage analysis." The skill was listed alongside 35+ other skills and didn't stand out.

**Fix applied:**
- Renamed description to include explicit tool names (`get_repo_map`, `search_codebase`, `analyze_impact`) — keyword matching
- Made description state "MANDATORY after pathfinder-first bootstrap" — creates dependency chain
- Created new `pathfinder-first` skill with description "MANDATORY pre-flight check" that triggers on any code task

### 4. No forced checkpoint

Nothing made the agent pause and evaluate "should I use Pathfinder here?" before defaulting to built-in tools. The fallthrough to `bash`/`read`/`grep` was frictionless.

**Fix applied:** APPEND_SYSTEM.md §0 header says "MANDATORY" and provides a 3-step checklist. The word "MANDATORY" + numbered steps creates a psychological anchor that forces evaluation.

---

## Changes Made

| File | Change | Why |
|------|--------|-----|
| `.pi/APPEND_SYSTEM.md` | Added §0 "Pathfinder MCP — Session Bootstrap (MANDATORY)" with tool routing table | Always-on visibility — APPEND_SYSTEM is loaded into every session |
| `.pi/AGENTS.md` | Updated "Pathfinder Tool Routing" header to cross-reference APPEND_SYSTEM.md and add pre-flight check | Remove duplication, point to canonical source |
| `.pi/skills/pathfinder-first/SKILL.md` | **New file.** Mandatory session bootstrap skill | Forces MCP discovery before any code task |
| `.pi/skills/pathfinder-workflow/SKILL.md` | Updated description to include explicit tool names and "MANDATORY" language | Better keyword matching for skill trigger |

---

## What This Doesn't Fix

These changes improve but cannot guarantee adoption:

1. **Model compliance.** LLMs are probabilistic. "MANDATORY" in a system prompt increases compliance but doesn't guarantee it. ~95% compliance is realistic.

2. **MCP gateway opacity.** The fundamental UX problem — `mcp()` is a single tool hiding 18 tools — is a pi platform limitation, not fixable via prompting. An ideal solution would surface MCP tools as first-class tools in the function list.

3. **Sub-agent access.** AGENTS.md already notes "Sub-agents may NOT have access to MCP tools." This is an orchestration limitation, not a prompting issue.

4. **Tasks where built-in tools are genuinely better.** Coverage analysis (bulk JSON parsing, statistics) is correctly handled by `bash` + `python`. The routing table in §0 explicitly lists exceptions. This is working as intended.

---

## Recommended Next Steps

### Pi Platform Level (requires pi changes)

1. **Surface MCP tools as first-class tools.** Instead of one `mcp()` gateway, expose each MCP tool as a named function (`pathfinder_get_repo_map`, `pathfinder_search_codebase`, etc.) in the tool list. This is the single highest-impact change possible.

2. **Add MCP health-check to session startup.** Pi could auto-run `mcp({ server: "pathfinder" })` at session start and inject the result into the system prompt. This eliminates the "discovery gap" entirely.

3. **Support `always-load` skills.** Allow skills to be marked as always-loaded (full content, not just description) in the system prompt. This would make `pathfinder-first` and `pathfinder-workflow` always available without the agent having to actively load them.

### Project Level (you can do now)

4. **Add a CI check** that verifies Pathfinder MCP is running in the test environment.

5. **Consider `disable-model-invocation: true`** on `pathfinder-first` skill so it can only be invoked via `/skill:pathfinder-first` at session start, not guessed at by the model. This makes the bootstrap intentional.
