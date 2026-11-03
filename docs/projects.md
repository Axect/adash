# Projects and your tasks

Agents are grouped by project: the git worktree (or folder) they run in. Projects need no setup; open **Project settings** in the dashboard menu to give one a goal and a priority, and to keep a short list of your own tasks. Everything here is stored in `meta.yaml` in the config directory.

## Project settings

| field | values | meaning |
|---|---|---|
| `goal` | text | what the project is for; shown on the project's header |
| `impact` | 1–5, default 3 | how much finishing it matters |
| `leverage` | 1–5, default 3 | how much your attention moves it (an answer from you unblocks a lot) |
| `cost` | 1–5, default 3 | how much of your attention it takes |
| `deadline` | date or empty | shown next to the project |
| `note` | text | free notes |
| `paths` | folders | extra folders that belong to the project (agents started there join it) |

### ★ score

```
★ = (impact + 2 × leverage) / cost
```

Leverage counts twice: the score favours projects where a little of your time unblocks a lot. With every field at its default the score is 3 for all projects, so the board hides ★ until at least one project differs; then each project header shows its score (hover for the calculation).

### Where the score is used

- **"Missed first" sort**: project groups are ordered by `(unread answers + 2 × open questions) × ★`, so an unanswered question in a high-★ project comes first. Inside a group, open questions and errors lead, then unread answers and other waiting agents, oldest first.
- **"Recent interaction" sort** (the default) ignores ★: groups follow your last message.
- **My-turn queue** (`/api/state.queue`, `adash ls --json`): open questions first, then by the project's ★, then by how long they have waited.

### Bottleneck

The **Bottleneck** mark goes to the project whose agents are, added together, waiting on you longest right now. It is measured from waiting time only; project settings do not change it.

## Hide, close, archive

From a project's header menu:

- **Hide** for an hour, for today, or until you show it again. Its agents keep working; they leave your queue, counts and notifications. A bar at the top lists hidden projects.
- **Close all** stops every agent of the project. Agents that are working, asking, or have unsent text in their input box are listed for confirmation first.
- **Archive** closes all its agents and hides the project for good, remembering the sessions that were open. A project with no running agents (only closed sessions left on the board) can be archived too; nothing is closed then.
- **Restore** (in the archived projects list) shows an archived project again; **Reopen all** starts its remembered sessions again on the same session files.

Nothing here deletes session files or analytics.

## Your tasks

**My tasks** is a short list of things only you can do (send a form, call someone, review a draft), each with an optional project and due date. Open tasks appear in the my-turn queue next to the agents waiting on you, ordered by their project's ★; tick one off when it is done.
