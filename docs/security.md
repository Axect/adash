# Security

adash can type into your agents' terminals, and your agents can run commands on your machine. Its guards exist so that nothing other than you (and agent-to-agent threads you started) can make it type.

## Threat model

In scope:
- other users and processes on the same machine reaching the HTTP port;
- web pages in your browser making requests to `127.0.0.1:7878` (CSRF, DNS rebinding);
- devices on your tailnet reaching the dashboard through `tailscale serve`;
- text inside agent output or files (rendered in the browser) trying to run script;
- adash itself typing into the wrong pane, typing over your unsent text, or looping between agents.

Out of scope: a compromised user account (it can read the session files and drive tmux directly), and what your agents do with the messages you send them.

## Network surface

- The server binds to `127.0.0.1` only. Remote access goes through `tailscale serve`, which terminates TLS and forwards to localhost.
- **Host check** on every request: only `127.0.0.1:<port>`, `localhost:<port>`, `[::1]:<port>` and your tailnet MagicDNS name are accepted (DNS-rebinding defence).
- **Mutating requests** need the header `X-Adash: 1` and a same-origin `Origin`; browsers cannot send that header cross-origin without a CORS preflight, which adash never grants.
- Requests that arrive through a proxy (`X-Forwarded-For`) without a Tailscale identity header are refused.
- The request body is capped before parsing.
- The web app is served with `Content-Security-Policy: script-src 'self'; object-src 'none'; frame-ancestors 'none'; base-uri 'self'` and `X-Content-Type-Options: nosniff`. Agent Markdown is rendered with HTML disabled.

## Files

- `GET /api/agents/{name}/file` and `/stat` resolve paths only inside the agent's worktree or `$HOME`, reject `..`, and refuse credential locations (`.ssh`, `.gnupg`, `credentials*`, `*.pem`, `*.key`). Temporary files are served only when owned by you.
- Served files get an allowlisted content type; anything else is sent as a download.
- `/open` (open a file on the desktop) from a phone is limited to images, PDFs, Markdown and text.

## Phones (tailscale serve)

- Only tailnet logins listed in `meta.yaml` (default: the tailnet owner) can request access.
- Each device shows a code and waits until you approve it on the desktop; approval issues an HttpOnly cookie bound to that device. Pending requests expire.
- An approved phone can read the dashboard, send messages, answer questions and retry errors. It cannot change the login allowlist, and its desktop `/open` is limited as above.
- `mobile.tailscale = false` in `config.toml` turns tailnet access off entirely.

## Keystroke guards

- **One sender per terminal**: chat, answers, relays, retries, magi instructions and draft clearing share a per-terminal lock, so keystrokes never interleave.
- **Gates** before adash-initiated sends: never into an agent that is working, showing a question, or has unsent text in its input box; when the screen cannot be read, the send is held (the human can "send anyway").
- **Targets by handle**: a thread remembers the terminal handle of its target; when that pane is gone it never falls back to another agent with the same name. Sends by name are refused when two live agents share it.
- **Hop caps** on agent-to-agent threads; an escalation goes to you instead of looping.
- **Network gate**: while a provider is unreachable, adash-initiated sends to its agents wait (no failure, no hop).
- Text sent "as text" is protected from harness prefixes (`!`, `$ `) with a zero-width space, so a message never runs as a shell command by accident.
- Closing an agent sends the harness's own exit command and closes only panes that run an agent process.

## Themes

User themes are data, not CSS: only colors, parsed font-family lists, keywords and bounded numbers are accepted, and the stylesheet is generated from them, one declaration per line with every font name re-quoted. A theme with errors shows its validated base theme.

## Data at rest

Chat uploads are stored as data only: mode 0600 in 0700 folders, never executable whatever their extension, names stripped of path separators, `..` and control characters, and deleted after 30 days. Session files are read, never written. adash's own data (analytics store, threads, uploads, push subscriptions, VAPID key, approved devices) lives in the data directory with your user's permissions; the push subscription file and VAPID key are `chmod 600`. Config files that fail to parse are backed up and never overwritten with defaults.

## Reporting

See [SECURITY.md](../SECURITY.md).
