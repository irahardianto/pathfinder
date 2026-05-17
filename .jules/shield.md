
## 2024-05-18 - Semantic Path Separator Confusion Tests
**Learning:** Adding tests for `PathfinderError::SymbolNotFound` hint strings revealed the code successfully detects common user errors (like using `.` instead of `::` or multiple `::`) and suggests actionable alternatives. The `hint()` method output is dynamically shaped by the type of separator error identified, making the resulting string critical to verify against regex/pattern-based assumptions.
**Action:** When validating complex diagnostic/hint messages, write separate parameterized tests for distinct detection cases (e.g., missing specific substrings or counting occurrences of separators) rather than a single check.
