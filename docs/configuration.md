# Configuration

Two files in the config directory (Linux `$XDG_CONFIG_HOME/adash` or `~/.config/adash`; macOS `~/Library/Application Support/adash` unless `XDG_CONFIG_HOME` is set).

## config.toml

Machine settings, written with comments by `adash config init` (JSON Schema: [schema/config.schema.json](schema/config.schema.json); editors with taplo validate it). `adash config show` prints the effective values. A `config.yaml` from an earlier build is converted once and kept as `config.yaml.migrated`. Restart the service after editing (`systemctl --user restart adash` or `launchctl kickstart -k gui/$(id -u)/dev.adash.adash`).

| key | default | meaning |
|---|---|---|
| `port` | `7878` | HTTP port (`ADASH_PORT` wins) |
| `harnesses.<name>` | `true` | recognize omp / claude / codex / antigravity |
| `terminal.emulator` | `auto` | opened by "open terminal" when no tmux client is attached (`ADASH_TERMINAL` wins). `auto`: `$TERMINAL`, then the first installed of x-terminal-emulator, ghostty, kitty, wezterm, alacritty, foot, konsole, gnome-terminal, xfce4-terminal, xterm (macOS: Ghostty, iTerm, Terminal). When none is found the dashboard offers the `tmux -L adash attach -t <project>` command to copy |
| `terminal.tmux` | `tmux` | tmux binary |
| `notifications.backend` | `auto` | `notify-send`, `terminal-notifier`, `osascript` or `none` |
| `notifications.silenceHarness` | `true` | turn off the agents' own desktop notifications in adash terminals so each event notifies once, from adash: omp via `PI_NO_DESKTOP_NOTIFY=1` in the terminal environment, Claude Code via `--settings '{"preferredNotifChannel":"notifications_disabled"}'`, Codex via `-c tui.notifications=false`. Your config files are not changed and agents started outside adash keep their notifications. Antigravity has no documented switch |
| `notifications.stallMinutes` | `10` | a working agent whose screen, session file and CPU stay still this long is flagged as stalled; `0` turns it off |
| `open.editor` | unset | editor for code and text files opened from chat (e.g. `nvim`); terminal editors run in the terminal emulator, at the line when the link has `path:line`. Unset uses `$VISUAL` / `$EDITOR` when they name a real program, else the system opener. Right-click a file link for editor / default app / dashboard view |
| `open.markdownApp` | unset | app for Markdown files opened from chat |
| `retry.message` | unset | message that retries Claude Code, Codex and Antigravity after a provider error (they have no retry key). Unset: `continue` while the dashboard is in English, `계속 진행해` while it is in Korean. omp is retried with its F5 key |
| `launch.autoTrust` | `true` | agents started from the dashboard, or by an agent inside its own folder, accept their harness's "trust this folder?" screen (you chose the folder); `adash term new --no-trust` turns it off per launch. See [security.md](security.md) |
| `sleep.enabled` | `true` | close agents that stayed done or idle for `sleep.afterHours` (gracefully; the session stays) and keep them on the board as **asleep**. A message, an answer, **Reopen**, an `adash ask`/`reply` thread or a magi run reopens the same session first and waits for its input box. Never slept: open questions, errors, sign-in or trust prompts, unsent drafts, unread answers, snoozed agents, open threads, pending restarts, a terminal a tmux client is showing, and agents pinned with **Keep awake**. Sleeping agents are not restored after a reboot. `adash sleep --dry-run` lists who would sleep |
| `sleep.afterHours` | `24` | hours done or idle before an agent sleeps |
| `sleep.perRound` | `0` | agents per harness put to sleep in one round (rounds run every 30 minutes); `0` = every one that is due |
| `restart.autoOutdated` | `false` | restart idle agents on the same session as soon as their harness was updated (`adash restart --outdated` does it on demand) |
| `reports.chrome` | unset | Chromium for PDF reports (searched on PATH when unset) |
| `magi.skill` | unset | SKILL.md of a multi-agent review skill; the magi feature is off when unset |
| `network.enabled`, `network.probes` | on, one URL per provider | provider reachability probes (HEAD/GET to the provider API hosts, no credentials) |
| `mobile.tailscale` | `true` | accept phones through `tailscale serve` |
| `theme.dark`, `theme.light` | `dark`, `catppuccin-latte` | themes a device uses until it picks its own ([themes.md](themes.md)) |

## meta.yaml

Edited from the dashboard:

- projects: goal, impact, leverage, cost, deadline, note, hidden / archived ([projects.md](projects.md));
- your own tasks;
- prompt-cache TTLs per harness, provider or model (`cacheTtl`): the countdown on each agent is the last activity plus this value, an estimate, not a time the provider reports. Defaults: omp and Claude Code with Anthropic 60 min, OpenAI 40 min (measured; OpenAI states no fixed TTL), z.ai 5 min, Antigravity 5 min (no measurement);
- `autoRetryOnNetworkRecovery` (default on): retry agents stopped by a network error once their provider is reachable again;
- the tailnet logins allowed on phones.

## Data

The data directory (Linux `~/.local/share/adash`, macOS `~/Library/Application Support/adash`) holds the analytics store, threads, uploads, reports and approved devices. `adash uninstall-service` never deletes it.
