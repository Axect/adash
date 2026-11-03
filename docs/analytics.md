# adash analytics store

`<data dir>/analytics.db` (Linux `~/.local/share/adash`, macOS `~/Library/Application Support/adash`) — SQLite (WAL), schema version in `PRAGMA user_version` (currently 2; v2 added `capacity_snapshots`).
Append-only facts with natural keys (`UNIQUE rec_key`), so re-ingesting or replaying a file never double counts.
Never rotated or pruned. Daily backups: `~/.local/share/adash/backups/analytics-YYYYMMDD.db` (SQLite backup API; last 14 days + first backup of each month).

Sources (read-only): omp `~/.omp/agent/sessions/**` (nested `<session>/<Name>.jsonl` = sub-agent sessions), `~/.claude/projects/**`, `~/.codex/sessions/**`, `~/.local/state/adash/history.jsonl` (state transitions, dashboard sends), `~/.local/share/adash/threads/*`.
Ingest is incremental by file offset (`ingest_files`) and runs on `adash analytics backfill` and every minute in `adash serve`. history.jsonl transitions are ingested at server start, before its 14-day compaction.

All timestamps are epoch milliseconds (UTC). Local-time grouping in SQL: `datetime(ts/1000, 'unixepoch', 'localtime')`.

## Tables

| table | one row per | key columns |
|---|---|---|
| `ingest_files` | source file | `path`, `source`, `size`, `mtime`, `offset` (bytes consumed), `state` (JSON parser state: open turn, pending tool calls, open ask, model, codename) |
| `sessions` | session file | `file` (UNIQUE), `agent_type` (omp/claude/codex), `session_uuid`, `parent_file` + `is_subagent` (omp sub-agent files), `origin` (codex `turn_trigger`, e.g. `exec`), `cwd`, `project`, `worktree`, `first_ts`, `last_ts`, `titles` / `models` / `handles` / `names` (JSON arrays; handles and adash names only while the server saw the session live), `mapping` (exact/title) |
| `human_messages` | user record in a session | `rec_key` (`file#recordId` or `file#byteOffset`), `ts`, `project`, `agent_name` (live adash name, else title codename at the time), `agent_type`, `model`, `kind`, `channel`, `chars`, `lines`, `lang` (ko/en/mixed/none), `attachments`, `is_slash`, `steering`, `text`, `sha256` |
| `agent_turns` | turn | `rec_key` (= key of the message that started it), `trigger_kind`, `start_ts`, `end_ts` (last agent record of the turn), `duration_ms`, `human_gap_ms` (previous turn end → this start, same session), `project`, `agent_name`, `agent_type`, `model`, `assistant_chars`, `tool_calls`, `subagent_spawns`, token columns, `cost`, `interrupted` |
| `tool_calls` | tool call | `rec_key` (`file#tool:callId`), `turn_key`, `ts`, `tool`, `duration_ms` (call → result), `is_error`, `project`, `agent_type`, `model` |
| `usage_events` | model call with usage | `rec_key`, `ts`, `purpose` (`turn` or omp `model_usage` purpose such as `job-widget`), `provider`, `model`, `project`, `agent_type`, `input_tokens` (uncached), `output_tokens`, `cache_read_tokens`, `cache_write_tokens`, `cost` (USD; omp only — claude/codex logs carry no cost) |
| `asks` | interactive question (omp `ask`, claude `AskUserQuestion`) | `rec_key` (`file#ask:callId`), `asked_at`, `question`, `options` (JSON), `multi`, `answered_at`, `answer` (JSON details), `cancelled`, `channel` (terminal/dashboard) |
| `state_transitions` | adash-observed state change | `identity`, `ts`, `state` (working/waiting/asking/snoozed/idle/offline), `prev_state`, `name`, `project`; UNIQUE(identity, ts, state) |
| `relay_threads` | adash ask thread | `id`, `from_agent`, `to_agent`, `created_at`, `delivered_at`, `answered_at`, `relayed_at`, `hops`, `status` (upserted as the thread advances) |
| `notifications` | desktop notification | `ts`, `kind` (transition/ask/relay), `title`, `body`, `target` (terminal handle) |
| `dashboard_actions` | successful mutating API call | `ts`, `action` (send, focus, rename, snooze, request-name, open, answer, interrupt, iam, deliver-anyway, upload, ask, human-tasks, meta …), `target`, `detail` (JSON; chat text is not stored here, only sizes) |
| `capacity_snapshots` | AI usage limit per `omp usage --json --redact` fetch (every 5 min) | `ts`, `vendor` (claude/openai/google/other), `provider`, `account` (redacted), `limit_id`, `label`, `window_label`, `used_fraction`, `resets_at`, `status` |
| `snapshots` | 5-minute tick of the server | `ts`, `live`, `working`, `waiting`, `asking`, `idle`, `per_project` (JSON) |

Views: `state_segments` (transitions with `end_ts` = next transition), `latencies` (per turn: `agent_response_ms`, `human_response_ms`, `after_cache_5m`, `after_cache_60m`).

## Field semantics

- `kind`: `human`; `relay` (`[adash <id> …` lines between agents); `adash` (`[adash] …` system lines such as name requests); `subagent` (task prompt inside a sub-agent session); `exec` (codex `exec` prompt from a program).
- `channel` for human messages: `terminal` (typed in the agent's adash tmux terminal), `dashboard-chat` / `cli-tell` (matched to a send logged in history.jsonl: same text prefix within ±5 min), `dashboard-answer` (ask answered via `/api/agents/{name}/answer`), `terminal-answer` (ask answered in the terminal). Agent→agent messages are matched to the sender's shell command: `cli-tell` (`adash tell`), `term-send` (`adash term send`).
- `steering`: a human message that arrived while the agent was mid-turn (last agent record asked for tools / codex task not complete). It does not start a new turn.
- Turns: a non-steering user message starts a turn; agent output after the turn finished (async results, background-job notices) starts a separate `auto` turn. `human_gap_ms > 300000` / `> 3600000` = the human answered after the prompt-cache 5 min / 1 h TTL.
- Slash command: first token `/word` with no further `/` or `.` (paths such as `/srv/notes/x.md` are not commands).
- `lang`: ≥70 % Hangul letters → ko, ≤10 % → en, else mixed.

## Queries

`adash stats [--from YYYY-MM-DD] [--to YYYY-MM-DD] [--by agent|project|model|type|day|hour|weekday] [--top N] [--json]` and `GET /api/stats?from=&to=&by=` report per group: `messages` (human), `activeDays`, `turns` (human-triggered), `workingHours` (all turns), `medianAgentResponseSec`, `medianHumanResponseSec`, `humanResponsesAfterCacheTtl` (share of replies after the cache TTL of that turn's harness/model, `meta.yaml cacheTtl.byHarness`) / `humanResponsesAfterCache60m`, `topTools`, `tokens`, `cost`.

Ad-hoc example — human messages per weekday and language:

```sql
SELECT strftime('%w', datetime(ts/1000,'unixepoch','localtime')) AS wd, lang, COUNT(*)
FROM human_messages WHERE kind = 'human' GROUP BY 1, 2 ORDER BY 1;
```
