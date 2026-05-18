## 2025-02-13 - Git Subprocess Argument Injection
**Vulnerability:** User-provided inputs passed to `get_changed_files_since` as the `target` argument could be exploited for argument injection (e.g., `-injection`) since they were passed directly to `tokio::process::Command::new("git")` as positional arguments.
**Learning:** For git commands specifically, we cannot simply append `--` to terminate option parsing before passing positional arguments, because `git` uses `--` to disambiguate revisions from paths. When the `target` is meant to be a revision, `--` cannot safely isolate it.
**Prevention:** Reject untrusted input strings that start with `-` when appending them as arguments to `tokio::process::Command` if they are not expected to be options and `--` cannot be safely used.
