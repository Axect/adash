# Security policy

adash types into terminals that run coding agents, so a bug that lets someone else make it type is serious.

## Reporting a vulnerability

Please do not open a public issue. Use GitHub's private vulnerability reporting ("Security" → "Report a vulnerability") on this repository, with the version, platform, and steps to reproduce. You will get an answer within a week.

## Scope

In scope: anything that lets a web page, another local user, or a tailnet device read agent data or send input without the owner's approval; path traversal in file serving; script injection through rendered agent output; adash typing into the wrong pane or over unsent text.

The threat model and the guards are described in [docs/security.md](docs/security.md).
