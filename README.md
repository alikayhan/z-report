<p align="center">
  <img src="design/app-icon.svg" width="132" alt="Z Report icon">
</p>

<h1 align="center">Z Report</h1>

<p align="center"><em>It celebrates outcomes, not activity. No tracking, no scoring, no dashboards.</em></p>

A fully local macOS app that turns Claude Code-assisted work into a private,
evidence-backed record of achievements. It opens as a regular desktop window and
keeps a menu-bar icon; closing the window leaves it collecting evidence and running
the evening Z-read in the background. Like the end-of-day Z-report a cash register
prints, it totals what was actually recorded and closes the books on the day: each
evening it reconstructs accomplishments from local session transcripts and Git facts,
then asks you to approve, edit, merge, or discard them.

## How it works

```
macOS desktop window + menu-bar tray (Tauri)
        ↓
background worker + scheduler (Rust)
        ↓
transcript adapter (~/.claude/projects/*/*.jsonl) + Git evidence adapter
        ↓
normalized local evidence store (SQLite)
        ↓
constrained Claude evaluator (claude -p, Opus 5, xhigh effort)
        ↓
review queue → approved journal → Markdown export
```

Daily flow:

1. **Work normally.** Every 30 minutes (configurable) the worker scans local Claude
   Code transcripts, extracts facts (prompts, files changed, commands run, exit
   status, pull requests opened, external tools used to change something), and
   correlates them with local Git state (repo, branch, your commits). Work you
   delegated to a sub-session counts too.
2. **Z-read.** At your chosen time a notification announces the day's candidates
   ("3 achievements are ready"). "Review now" runs a mid-day X-read on demand. Either
   one evaluates every session from the last 15 days it hasn't evaluated yet, one
   evaluator run per day, so a first run backfills about two weeks of work.
3. **Confirm.** Approve, edit, merge, or discard each candidate — by click or with
   the keyboard (J/K moves through the queue, A approves, E edits, X discards).
   Cards that look like two halves of the same work say so, with the merge one
   click away. Nothing enters the journal without you, and nothing merges without
   you either.
4. **Export.** Copy or save daily/weekly/custom-range Markdown summaries for
   standups, weekly updates, or performance reviews.

## Evidence levels

Every claim carries the highest level local facts support, and the verifier
downgrades anything the evaluator overstated:

1. **Work observed** — investigation or implementation appears in a session
2. **Change produced** — a concrete change exists, in the repo or outside it
3. **Locally verified** — a relevant test/build/check passed
4. **Committed** — the change exists in a local commit, or the session recorded a
   pull request alongside a file change
5. **Impact confirmed** — you manually confirmed a real-world outcome

Verification is deterministic Rust code, not the model: commit refs are checked with
`git cat-file`, command refs against recorded exit status, file refs against the
session's change list, and pull request refs against links recorded in the session
itself. Unsupported claims are downgraded and labeled.

A recorded pull request proves the change was proposed, not that it merged — merge
state is not knowable offline, so it never reaches level 5 on its own. It does keep an
achievement at level 4 after a squash merge deletes the local commit. Links are carried
through to the journal and Markdown export, and only canonical `github.com` pull request
URLs are kept.

Not all work lands in the repository. When a session used a connected tool to change
something outside it — commenting on a ticket, updating a document — that call is
recorded as an external action and cited like any other fact. It stops at level 2: the
call is known to have succeeded, but nothing on your Mac can confirm what it did, so the
evaluator describes it as performed rather than as impact. Only calls that write are
cited; searches, fetches, and screenshots are how the work got done, not what it
produced. Nothing in a transcript says which is which, so it is inferred from the tool
name and recorded alongside the call — a sharper guess later can reclassify calls
already read, rather than being unable to recover what an earlier filter discarded.

A session that delegates work to a sub-session is credited with it: the edits and
commands from the delegated run fold into the parent session and carry the same evidence
weight, since you directed them. They are marked as delegated so the evaluator describes
them accurately rather than as hands-on work, and so a delegated session is no longer
mistaken for an empty one. Only the parent's prompts count as things you said, and only
the parent's working directory defines the repository — a delegate may run in its own
worktree.

## Work carried across days

An evaluation covers one day at a time, so a task you picked up on Tuesday and finished
on Wednesday used to arrive as two cards that never saw each other. Z Report now scores
each new card against the recent queue and the journal, and marks the ones that look like
one piece of work.

The signal that carries this is the session's own title, which Claude Code generates from
how the session opened. That makes it a statement of what you set out to do, so two
sessions on one thread tend to share wording even when the finished cards do not — the
evaluator rewrites each day's card in outcome-first language, which erases the overlap
between them. Matching requires the same repository and title wording in common; shared
files and a shared feature branch only strengthen a match that already exists. They
cannot create one, because the earlier half of a thread is usually a scoping session that
changed no files at all, and a rename-everything session would otherwise look related to
everything in its repo. Branches every session shares, like `main`, count for nothing.

Nothing merges on its own. A match is a suggestion on the card with the work that
continues it, and dismissing one is remembered against the sessions involved, so it stays
dismissed even after that day is evaluated again. Merging is still the same manual path
it always was. A merged achievement spans from the day the work started to the day it
finished, and appears in any export whose range overlaps it rather than falling through
the gap at either end. Its outcomes are carried across untouched so verified evidence
survives; only the prose is rewritten, in the background, and if that rewrite fails the
merged card simply keeps its assembled text.

Work already approved into the journal is flagged rather than hidden: if a session you
have already written up comes back through evaluation, the new card says so and links to
the entry, instead of quietly appearing as a second copy of something you have read.

## Privacy and network boundary

- All product data (evidence, candidates, journal, settings) lives in
  `~/Library/Application Support/com.zreport.app/` — SQLite, no accounts, no sync.
- Z Report has **no backend, no analytics, and no telemetry**.
- The one thing that leaves your Mac: each evaluation runs `claude -p` on **your own
  Claude Code account**, sending the prepared evidence package (session excerpts,
  file paths, command results including those from delegated sub-sessions, the names of
  external tools used to change something, commit and pull request metadata) to
  Anthropic — the same boundary as using Claude Code itself. This is disclosed in
  Settings. Arguments passed to external tools are never included, only the server and
  tool name.
- The evaluator is sandboxed: fresh ephemeral run, read-only tool allowlist
  (`Read,Grep,Glob`), working directory containing only the evidence package,
  no session persistence, no user settings, and an optional per-run safety cap.
- Prompt excerpts in evidence are optional (Settings → Privacy). Full transcripts
  are never copied — only referenced. The retention setting governs the review queue:
  unreviewed candidates age out, approved entries stay. Extracted session facts are
  kept for 90 days, and dropped once their transcript is gone. "Delete all data"
  erases everything.

## Development

Requirements: Rust (stable), Node 20+, Claude Code CLI installed and authenticated.

```sh
npm install
npm run tauri dev      # run the app
cargo test             # backend tests (from src-tauri/)
npm run tauri build    # release bundle (.app + .dmg)
```

The UI can be developed without Tauri: `npm run dev` serves it in a browser against
fixture data (`src/mock.ts`).

## Evaluator contract (validated)

Verified against Claude Code 2.1.215:

- `claude -p --model claude-opus-5 --effort xhigh --output-format json` returns a
  single JSON result whose `modelUsage` records the model that actually served the
  run — stored with every evaluation.
- `--json-schema` yields a validated `structured_output` object matching the
  achievement contract (no output parsing heuristics).
- `--tools "Read,Grep,Glob" --disallowedTools ... --no-session-persistence
  --setting-sources ""` provide read-only, ephemeral isolation; `--max-budget-usd`
  adds a fixed per-run safety cap when the cost limit is enabled. Permission denials
  are visible in the result.
- Transcript JSONL records are typed (`user`, `assistant`, `system`, `pr-link`,
  `ai-title`, attachments, snapshots); parsing is defensive because the schema is
  internal to Claude Code and undocumented. Records carry `cwd`, `gitBranch`,
  `version`, and timestamps used for Git correlation.
- `ai-title` is emitted repeatedly through a session but never revised: across a
  20-session sample every session carried exactly one distinct title, re-emitted up to
  98 times. It describes the opening of the session, not its conclusion.
- Connected-tool calls appear as `tool_use` blocks named `mcp__<server>__<tool>`, paired
  with a `tool_result` the same way shell commands are. No field in the record — and no
  cached server manifest — states whether a call is read-only, so the tool name is the
  only local signal.
- Delegated runs live in sibling files (`<session-id>/subagents/agent-*.jsonl`) whose
  records are flagged `isSidechain`. They are read alongside the parent transcript and
  folded into the same session, and they contribute to the change-detection hash so
  delegated work alone re-triggers an evaluation.

## Non-goals (MVP)

Cloud sync, accounts, other coding agents, Slack/ticketing/GitHub API integrations,
manager analytics, time/token reporting, monetary estimates, automatic publishing,
cross-platform support.
