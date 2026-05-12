## 2024-05-12 - Prevent Git Argument Injection in git diff
**Vulnerability:** Command argument injection in `SystemGit::diff_name_only` via the `target` parameter, which could allow options like `--output` to be passed to `git diff`.
**Learning:** Untrusted inputs appended to command line arguments must be validated to ensure they are not parsed as flags/options, especially when `--` cannot be easily used to terminate option parsing for revisions.
**Prevention:** Reject inputs starting with `-` when they are expected to be positional arguments or revisions.
