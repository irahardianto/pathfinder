## 2025-05-18 - Parameterized tests for specific hint logic
**Learning:** When generating dynamic hints like `PathfinderError::SymbolNotFound`'s path separator detection (where `.` vs `::` confusion is caught), tests should specifically check the different branches of that hint generation logic to ensure the agent receives the correct guidance.
**Action:** In the future, verify complex hint/error generation logic using targeted tests that validate the string contains the expected substrings, covering different variations of malformed input.
