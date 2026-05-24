## 2024-05-24 - Git Command Argument Injection
**Vulnerability:** The `get_changed_files_since` function passed user-supplied git revisions directly to `git diff` without sanitization. An attacker could supply a target starting with `-` to inject arbitrary arguments into the command line. For example, injecting `--output=/tmp/pwned`.
**Learning:** For git commands, `--` cannot be safely used to terminate option parsing for revisions because Git uses `--` to disambiguate revisions from paths.
**Prevention:** Explicitly validate any untrusted input intended as a revision to ensure it does not start with `-` before appending it to command arguments.
