# ISSUE REPORT: Large MCP Tool Text Results Rendered as Images in Agent History

## Affected Component

Likely: LLM provider (Anthropic Claude API) conversation history rendering.
Possibly: MCP client framework or AI coding agent.

## Reporter

Pathfinder MCP server maintainer (pathfinder codebase).

## Description

When `pathfinder_get_repo_map` returns a large text response (e.g., 20KB+ repo skeleton),
agents report receiving the result as an "image attachment (screenshot)" instead of parseable
text. This breaks programmatic consumption — agents cannot extract semantic paths from image
output to chain into subsequent tool calls.

The issue is intermittent — occurs on 2 of 3 calls in one report, always on large responses.

## Investigation

### Server side (Pathfinder): NOT the cause

The server always returns `Content::text(result.skeleton)`:

```rust
// crates/pathfinder/src/server/tools/repo_map.rs:112
let mut res = CallToolResult::success(vec![rmcp::model::Content::text(result.skeleton)]);
res.structured_content = serialize_metadata(&metadata);
```

The `rmcp` framework's `RawContent::text()` creates `{ type: "text", text: "..." }`.
No image conversion happens server-side. Confirmed by reading the rmcp 1.0.0 source.

### MCP Adapter (pi-mcp-adapter): NOT the cause

The adapter's `transformMcpContent` in `tool-registrar.ts` faithfully passes through content types:

```typescript
if (c.type === "text") {
  return { type: "text" as const, text: c.text ?? "" };
}
if (c.type === "image") {
  return { type: "image" as const, data: c.data ?? "", mimeType: c.mimeType ?? "image/png" };
}
```

Text content remains text. No conversion to image anywhere in the adapter code.

### Pi TUI (pi-coding-agent): NOT the cause

The TUI's `getTextOutput` in `render-utils.js` concatenates text blocks and renders image
blocks inline via kitty protocol. It does NOT convert text to images. The text content is
available to the agent in the `toolResult` message.

### Hypothesis: LLM Provider

The most likely cause is the LLM provider (e.g., Anthropic Claude API) rendering large
`tool_result` text content as screenshots/images in the conversation history. This would
explain:

1. **Intermittent**: Only happens when the text exceeds a certain size threshold
2. **Only affects large responses**: Small `get_repo_map` results (low `max_tokens`) work fine
3. **No client-side conversion found**: The entire pipeline from server → adapter → pi → LLM
   preserves text type

When Claude (or another provider) receives a `tool_result` with very large text content,
it may internally render it as an image/screenshot for display in its UI, and that rendered
version is what gets stored in the conversation history. Subsequent turns then see the image
instead of the original text.

## Reproduction

1. Call `pathfinder_get_repo_map` with `max_tokens: 50000` on a large codebase
2. Observe the response — if the text is very large, check what the agent sees in the next turn
3. If the agent cannot reference specific paths from the repo map, the text was likely
   converted to an image in the conversation history

## Suggested Investigation

1. Add logging in `pi-mcp-adapter`'s `executeCall` to record the content types returned
   from the MCP server vs. what's sent to the LLM
2. Check the LLM API request/response logs to see if large `tool_result` text blocks are
   being converted or truncated
3. Test with different LLM providers (OpenAI, Google) to see if the behavior differs

## Workaround (Server Side)

Pathfinder could mitigate this by:
1. Defaulting to a lower `max_tokens` (e.g., 8000 instead of 16000) to stay under
   whatever threshold triggers image conversion
2. Structuring the repo map output into the `structured_content` field (JSON) which
   is less likely to be rendered as an image
3. Adding pagination to `get_repo_map` (e.g., return partial results with a cursor)

## Questions for the Maintainer

1. Have you observed large text tool results being converted to images in the agent's
   conversation history?
2. Is there a known size threshold where text content gets rendered as images?
3. Does the pi agent framework do any content transformation before sending tool results
   back to the LLM provider?
4. Is the `structured_content` field from MCP `CallToolResult` forwarded to the LLM as-is,
   or is it processed/filtered?
