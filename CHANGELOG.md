# Changelog

## Unreleased

- omp: a turn cut by a restart or reboot ("Previous OMP process exited before completing the turn") shows as an error labelled "interrupted" with the F5 retry; omp's " F5 to retry" hint on screen does the same when no error was read. Restarting a working agent offers "continue the interrupted work" (on by default): F5 for omp, the retry message for the others, once the input box is ready (`resumeWork` on `POST /api/agents/{name}/restart`).
- Input box reading takes only what lies left of the terminal cursor: omp's grey predicted next message, agy's mode hint and other text a TUI draws after the cursor in an empty box no longer count as an unsent draft (which held sends, kept agents from sleeping and offered to send the prediction).
- Subagents: an omp agent whose `task` subagents still run after its turn (Claude Code Task subagents too, untested) stays working, out of My turn, unnotified and never slept; the row and chat header show "↳ N subagents", the info panel lists each with its state, last tool and age, and opens its conversation read-only (`GET /api/agents/{name}/subagent?file=`).
- Projects: a new agent in an archived project un-archives it, and one in a hidden project is announced (the project stays hidden), with a desktop notification and a dashboard toast; `/api/state` `projects[]` carries `archived` and `hidden`.
- omp form answers: a text field is cleared before typing and read back from the screen (cleared and typed once more when it differs), so a retry over a half-filled form no longer types the text twice; after submitting, the form leaving the screen counts as success and the record is awaited up to 120 s (a large form recorded its answer 42 s late and was reported as failed); typed answers the harness recorded differently are named with a correction draft.
- README rewritten: what adash is and its tmux-server scope up front, a 30-second try, the core loop, an accurate harness table, quick start with bringing existing sessions in and uninstalling, the defaults that act on their own, and an accurate privacy note. Settings moved to `docs/configuration.md`.
- `adash uninstall-service [--tmux [--yes]]`: stops and removes the dashboard service; `--tmux` also the tmux service after a warning that it ends every agent terminal. Config and data are kept.
- Retry message: `continue` with an English dashboard, `계속 진행해` with a Korean one; `[retry] message` sets your own.
- Tested versions: omp 18.13–18.20, Antigravity 1.4–1.5; a test keeps the README table and `adash harnesses` in agreement.
- English UI: "1 agent keeps working".

## 0.4.0 — 2026-11-17

- Sleeping agents (`[sleep] enabled`, `afterHours = 24`, `perRound = 0`): agents done or idle that long are closed gracefully in rounds every 30 minutes (`perRound` caps each harness per round; 0 = no cap) and stay on the board as "잠듦 · N일 전". A message, an answer, "다시 열기", an adash thread or a magi run reopens them on the same session and waits for the input box first. Never slept: questions, errors, sign-in, drafts, unread answers, snoozed, open threads, pending restarts, a terminal a tmux client shows, pinned agents. Not restored after a reboot. `adash sleep --dry-run | now | wake | pin | unpin`; the agent menu has 잠재우기 and 잠들지 않게 고정, the top bar 잠듦 N · 모두 깨우기.
- Memory: `GET /api/health` (adash RSS, peak, transcript cache, agents' total); each agent's memory in its info panel; unread live transcripts leave the cache after 30 minutes.
- Questions: Claude `AskUserQuestion` is answered from the dashboard; HTML-form fields without `required` may stay empty and preselected choices show picked; the send button shows how many are empty; an omp question box seen before its record shows a loading card instead of a guessed question.
- Antigravity: errors left at the prompt (also under the satisfaction survey) become errors, 401 becomes sign-in needed, "model not found" offers restart with the same or another model and can resend the last message; the survey and notices are closed before typing; "Interrupted" with the mode hint is stopped; an answer still being written shows "작성 중…" and redraws as it grows; provider-error retries turn into errors after 5 minutes.
- Chat: harness built-in commands (`/reload-plugins`) show as a command line with their output; pending bubbles turn "not confirmed" after 30 s; code files open in an editor (`[open] editor`) at `path:line`, with a right-click menu; phones view files in the dashboard.
- Decisions: ratios and ranges ("44 / 26 / 30") stay inside one option.
- Reports: a weekly report covers this week by default (`prev` = last whole week; the scheduled one keeps the last whole week); the dialog lists dated periods and asks before replacing a report.
- Capacity: refresh drops omp's usage cache first; Anthropic is checked against `claude /usage`; source and time in the tooltip; limits from Claude, Codex and Antigravity without omp.
- omp config-change confirmations show as approval with a card; stalled working agents are flagged (`notifications.stallMinutes`), counting across adash restarts; model lists refresh every 10 minutes and when the harness changes.
- Build: web push goes through the `curl` program (libcurl failed to link against OpenSSL 4).

## 0.3.0 — 2026-11-11

- Questions: omp's HTML-form `ask` (omp 18.18) is shown as a question card with its text and images and answered from the dashboard, page by page, checked against the form's `k/N` progress; a failure says how many fields went in. While an agent asks, the question card replaces the message box and the draft returns afterwards. Claude Code sessions map exactly through their pid file, so their questions show too.
- Terminal view with key input for every agent. Sign-in prompts become state "sign-in needed" with a card (open the URL, copy the code, restart when another window is signed in); folder-trust prompts become state "trust" with a card, held messages are delivered once the folder is trusted, and launches from the dashboard or by an agent inside its own folder accept it automatically (`launch.autoTrust`, `adash term new --no-trust`).
- Restart an agent on the same session. Agents running a harness that was updated since they started are marked (running vs installed version, update and start times) and can be restarted together, now or when they finish (`adash restart --outdated`, optional `restart.autoOutdated`). "Fix all" retries errors and restarts agents stuck at sign-in; after a reboot one agent per harness starts first so a sign-in screen does not stop them all.
- Model and effort: omp switches through `/switch` when available, with a restart-then-switch for sessions older than the installed omp; a switch shows at once in the badge, info panel and context size. Badges name the model (`opus`, `fable`, `astra`, `argon`, `sol`); Antigravity's model is read from the conversation when it was not on the command line, and resume keeps the original flags.
- Slash commands come from the installed omp and Claude Code, so every built-in (for example `/reload-plugins`) completes.
- Chat: images returned by tools are shown; a copy button on every message, and copied selections become Markdown with formula sources; Typst-style inline math is converted for rendering and failures show the source; the decision card folds to one line (folded by default on phones); a pending bubble no longer stays "sending".
- Antigravity: first step, thinking and notices in the transcript; input queued by agy is shown with a send-now button; an interrupt ends the working state, a second Ctrl+C is not sent; its news box is dismissed before input.
- New session: folder completion in the project path, and projects in subfolders of a repository are listed and searchable.
- Snooze for 1, 6, 12 or 24 hours or a custom length up to 30 days.
- Phone: swipes take effect at once and a tap opens the chat. Popovers stay on screen at any zoom and in fullscreen; short names on my-turn rows are no longer cut.
- Build: changes under `web/` rebuild the binary.

## 0.2.0 — 2026-11-04

- Session files are read line by line safely: a line whose newline is visible before the rest of it (a torn read during a large write) is read again next round instead of being skipped, so an agent no longer stays "working" after its last answer; a file rewritten under the reader is read again from the start. The Antigravity transcript picks up model steps updated in place.
- One notification per event: adash terminals start omp, Claude Code and Codex with their own desktop notifications off (`notifications.silenceHarness`, default on). Clicking an adash notification opens that agent's chat on the dashboard, a finished turn within a minute of another notification of the same agent folds into it, and a newer notification of an agent replaces the one still on screen.
- Phone access: an approved phone is recognised by its Tailscale node, so opening the dashboard from the home-screen app, another browser or after clearing site data no longer asks for a new approval code; the device list shows the node name, and revoking a device revokes its node.
- Antigravity agents no longer stay "working" after they finish: steps still running are read again until agy marks them finished in place.
- New-session dialog: the model field is a searchable list of every model the harness offers (omp's catalog, Claude Code's aliases and models seen in its sessions, `codex debug models`, `agy models`), grouped by provider, with each model's effort levels and the harness's own default model; Claude Code and Antigravity sessions can start with an effort.
- The dashboard's CSS and JavaScript are revalidated on every load (ETag, `Cache-Control: no-cache`), so an upgrade takes effect without clearing the browser cache.
- Change an agent's model and reasoning effort from the chat info panel where the harness allows it (omp, Claude Code and Codex: both; Antigravity: read-only), confirmed from the session record or the harness's reply (`GET /api/agents/{name}/models`, `POST /api/agents/{name}/model`).
- Thinking level in the chat info panel again: read from the session (omp `thinking_level_change`, Codex `effort`), omp's `--thinking` flag, or Antigravity's model name; null when the harness records none (Claude Code).
- Snoozed agents stay quiet: no desktop or phone notification while snoozed (provider errors included), no highlight on the board, and report bottlenecks no longer count snoozed time as waiting on you.

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
