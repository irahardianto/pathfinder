## 2026-05-14 - Prevent Command Argument Injection in GitRunner
**Vulnerability:** The `git diff` command in `SystemGit::diff_name_only` was vulnerable to argument injection if the `target` string started with a hyphen (e.g., `--output=/tmp/pwned`).
**Learning:** Untrusted inputs appended to `std::process::Command` arguments can be interpreted as flags if they start with `-`. In some cases, `--` cannot safely be used to isolate positional args, meaning we must manually validate and reject inputs starting with `-`.
**Prevention:** Always validate untrusted inputs used as positional arguments in `Command` executions. Specifically, reject inputs starting with `-` when they are expected to be positional arguments and `--` cannot safely be used to terminate option parsing.
