# Changelog

## 0.1.0 — 2026-11-03

First public release.

- Dashboard for agents running in adash's own tmux server: my-turn queue, 6/24/72 h timeline, chat with Markdown/math/Mermaid, question answering, decisions, prompt-cache countdown, usage panel.
- Harnesses: omp, Claude Code, Codex CLI, Antigravity.
- Agent-to-agent threads with delivery gates and hop caps; magi multi-agent review runs (optional).
- Provider error detection and retry; provider reachability probes with held automatic sends.
- Analytics store, `adash stats`, full-text search, weekly/monthly reports.
- Phone access through `tailscale serve` with device approval; Web Push.
- Project settings (goal, impact, leverage, cost, deadline, note) with a ★ score used by the "missed first" sort and the my-turn queue; your own tasks next to the agents; project hide / close all / archive (also for projects with only closed sessions) / restore / reopen all. Throwaway test sessions leave no trace on the board, timeline or analytics.
- Themes in TOML: 42 built-in themes (adash's own four and 38 from published palettes: Dracula, Catppuccin, Tokyo Night, Nord, Gruvbox, Solarized, Rosé Pine, Kanagawa, Everforest, GitHub, Modus, One, Ayu, Flexoki, Nightfox, Okabe–Ito), mapped to roles by one rule set; defaults adash dark (`dark`) and catppuccin-latte (light); `adash theme import` for Ghostty, base16/base24, VS Code, kitty, Alacritty, WezTerm and iTerm2; user themes reloaded on save. JSON Schemas for theme files and `config.toml` share the server's input rules (per-token transparency, font-list syntax and length); contrast on the drawn color pairs and inheritance are checked by the server.
- `config.toml` replaces `config.yaml`; an existing YAML is converted once, verified by reading it back, and kept as `config.yaml.migrated`.
- Agent rows keep the core state, error code and retry button visible (tested at 1024 and 1280 px with font size 18); extra markers fold first, and when space is still short, tool names and secondary times are shortened with an ellipsis.
- Chat attachments: any file (up to 100 MB) pasted, dropped or picked in the composer is stored privately under the data directory (0600, never executable, kept 30 days, same content reused) and handed to the agent by path (`@path` for omp and Claude Code).
- Sending: a multi-line message is confirmed submitted (the agent's input box empties; one more Enter, else reported as not submitted); clearing a draft deletes every line. Apps and terminals opened from the dashboard run in their own systemd scope on Linux, so restarting the service no longer closes them.
- Hourly usage of a session (`GET /api/agents/{name}/usage/hourly`): tokens, cost, replies, user messages and tool calls per local hour, subagents included and counted apart; harnesses without token records report null, not zero.
- English and Korean UI.
- Linux (systemd) and macOS (launchd) services; `config.toml`; `adash demo`.
