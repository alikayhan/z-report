# CLI and transcript contracts

What Z Report relies on from the Claude Code and Codex CLIs, as verified against the versions named below. Parsing is defensive because both transcript formats are internal and undocumented.

## Evaluator contract (validated)

Verified against Claude Code 2.1.215:

- `claude -p --model claude-opus-5-5 --effort high --output-format json` returns a
  single JSON result whose `modelUsage` records the model that actually served the
  run, stored with every evaluation.
- `--json-schema` yields a validated `structured_output` object matching the
  achievement contract (no output parsing heuristics).
- `--tools "Read,Grep,Glob" --disallowedTools ... --no-session-persistence
  --setting-sources ""` provide read-only, ephemeral isolation; `--max-budget-usd`
  adds a fixed per-run safety cap when the cost limit is enabled. Permission denials
  are visible in the result.

Verified against Codex CLI 0.152.1:

- `codex exec --model gpt-6.1-sol -c model_reasoning_effort="high" --output-schema
  <file> -o <file> --json` writes the schema-checked answer to the `-o` file and streams
  typed events (`thread.started`, `item.completed`, `turn.completed` with token usage)
  to stdout. No event names the serving model, so the requested model is recorded.
- The schema must be strict for OpenAI structured outputs: every object closed with
  `additionalProperties: false` and every property required. The same schema is handed
  to both CLIs.
- `--sandbox read-only -c approval_policy="never" --ephemeral --ignore-user-config
  --skip-git-repo-check` provide read-only, ephemeral isolation: no rollout is written
  (so the run never shows up as a Codex session of its own), and the developer's MCP
  servers, hooks, and reviewer settings are not loaded. Auth still comes from
  `~/.codex`.
- `codex exec` waits on stdin when it is not a terminal, so stdin is closed explicitly.
  `codex login status` reports the auth method in prose ("Logged in using ChatGPT");
  an API-key login is treated as metered.
- There is no per-run budget flag, so the cost limit setting applies to Claude Code only.
- Fallback order is fixed: Claude Code runs when its CLI is found; Codex runs when it is
  not, or when the Claude Code run returns any error. A run that fell back keeps the
  Claude Code failure as its note, and the model column names the CLI that answered.
  There is no setting for this.
- Transcript JSONL records are typed (`user`, `assistant`, `system`, `pr-link`,
  `ai-title`, attachments, snapshots); parsing is defensive because the schema is
  internal to Claude Code and undocumented. Records carry `cwd`, `gitBranch`,
  `version`, and timestamps used for Git correlation.
- `ai-title` is emitted repeatedly through a session but never revised: across a
  20-session sample every session carried exactly one distinct title, re-emitted up to
  98 times. It describes the opening of the session, not its conclusion.
- Connected-tool calls appear as `tool_use` blocks named `mcp__<server>__<tool>`, paired
  with a `tool_result` the same way shell commands are. No field in the record, and no
  cached server manifest, states whether a call is read-only, so the tool name is the
  only local signal.
- Delegated runs live in sibling files (`<session-id>/subagents/agent-*.jsonl`) whose
  records are flagged `isSidechain`. They are read alongside the parent transcript and
  folded into the same session, and they contribute to the change-detection hash so
  delegated work alone re-triggers an evaluation.

## Codex transcript contract (validated)

Verified against Codex CLI and desktop 0.147 through 0.153. Older rollouts use several
earlier record shapes and are not read.

- One rollout per thread at `~/.codex/sessions/YYYY/MM/DD/rollout-<time>-<uuid>.jsonl`,
  plus `~/.codex/archived_sessions/`. The uuid is the session id. Every line carries a
  top-level `timestamp`, `type`, and `payload`.
- The first record is `session_meta` with `id`, `cwd`, `cli_version`, `originator`,
  `source`, and a `git` block whose `branch` is used when present. Each turn repeats
  `cwd` in a `turn_context` record. A resumed thread appends to the same file; a forked
  thread copies only the parent's metadata, under the parent's id, so metadata is taken
  only from records matching the file's own id.
- Facts arrive as `event_msg` records of type `item_completed`, keyed by `item.type`:
  `UserMessage` (prompts, already free of injected context), `AgentMessage` with
  `phase: final_answer` (final response), `CommandExecution` (`command` array, numeric
  `exit_code`, `status`), `FileChange` (`changes` keyed by absolute path with `add`,
  `update`, or `delete`), and `McpToolCall` (`server`, `tool`, `readOnlyHint`, `status`,
  `result.isError`). The read-only hint decides mutability when it is a boolean; the
  tool-name heuristic is the fallback.
- Spawned sub-agents are separate rollouts carrying `parent_thread_id`; they fold into
  the parent as delegated work, transitively. Threads whose `source` is a subagent of
  kind `other` are approval reviewers that quote the parent's transcript as their prompt
  and are skipped entirely.
- Desktop threads are named in `~/.codex/session_index.jsonl` (`id`, `thread_name`); the
  name is the session title. CLI threads have no name and, like any unnamed session, use
  the opening prompt unless prompt retention is off, in which case they carry no title.
- Codex records no pull-request link, so level 4 for a Codex session rests on local
  commits.
