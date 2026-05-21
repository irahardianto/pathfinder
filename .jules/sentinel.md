## 2025-05-21 - [CRITICAL] Prevent argument injection in git runner
**Vulnerability:** The `SystemGit::diff_name_only` method allowed unsanitized input `target` to be appended to `git diff --name-only <target>`, which could lead to argument injection if `target` started with a hyphen (since `--` wasn't used).
**Learning:** For git subprocess commands, we can't safely use `--` to terminate option parsing for revisions because Git uses `--` to disambiguate revisions from paths. Therefore, we must explicitly reject inputs starting with `-`.
**Prevention:** Always validate untrusted revision strings for git commands by rejecting those starting with `-`, and use `--` to isolate path arguments whenever possible.
