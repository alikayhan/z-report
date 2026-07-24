// Fixture backend for running the UI in a plain browser (npm run dev outside Tauri).

import type { Candidate, EvalRun, JournalEntry, Overview, Settings } from "./api";

const today = new Date().toISOString().slice(0, 10);

const candidates: Candidate[] = [
  {
    id: "c1",
    day: today,
    title: "Fixed flaky auth-token refresh test that blocked CI",
    contribution:
      "Tracked an intermittent 401 in the session-refresh flow to a race between the keychain read and the token expiry check, serialized the refresh path, and hardened the test with a fake clock.",
    outcomes: [
      { claim: "Auth test suite passes locally", evidence_level: 3, evidence_refs: ["cmd:s1:4"], verified: true },
      { claim: "Refresh race fixed in SessionStore.swift", evidence_level: 2, evidence_refs: ["file:SessionStore.swift"], verified: true },
      { claim: "Change committed to feature branch", evidence_level: 4, evidence_refs: ["commit:ab12cd3"], verified: false },
    ],
    uncertainties: ['Claim "Change committed to feature branch" was stated at evidence level 4 but local facts only support level 2.'],
    confidence: 0.88,
    evidence_level: 3,
    session_ids: ["s1", "s2"],
    repo: "/Users/dev/gymondo-ios-app",
    model: "claude-opus-5",
    status: "pending",
    created_at: new Date().toISOString(),
  },
  {
    id: "c2",
    day: today,
    title: "Mapped the workout-sync pipeline ahead of the offline rewrite",
    contribution:
      "Traced how workout events flow from HealthKit ingestion through the sync queue to the API client, and documented the three places conflict resolution can drop events.",
    outcomes: [
      { claim: "Investigation documented across two sessions", evidence_level: 1, evidence_refs: ["session:s3"], verified: true },
    ],
    uncertainties: ["No code changes were produced; this was investigation only."],
    confidence: 0.71,
    evidence_level: 1,
    session_ids: ["s3"],
    repo: "/Users/dev/gymondo-ios-app",
    model: "claude-opus-5",
    status: "pending",
    created_at: new Date().toISOString(),
  },
];

const discarded: Candidate[] = [
  {
    ...candidates[1],
    id: "c9",
    title: "Renamed two variables in a scratch file",
    status: "discarded",
  },
];

const journal: JournalEntry[] = [
  {
    id: "j1",
    day: new Date(Date.now() - 86400000).toISOString().slice(0, 10),
    title: "Shipped incremental transcript ingestion for Z Report",
    contribution:
      "Built the session discovery and JSONL fact extraction with duplicate prevention, plus nine unit tests covering command classification and evidence verification.",
    outcomes: [
      { claim: "cargo test passes (9 tests)", evidence_level: 3, evidence_refs: ["cmd:s4:2"], verified: true },
      { claim: "Committed on main", evidence_level: 4, evidence_refs: ["commit:77aa21f"], verified: true },
    ],
    evidence_level: 4,
    session_ids: ["s4"],
    repo: "/Users/dev/z-report",
    model: "claude-opus-5",
    approved_at: new Date(Date.now() - 80000000).toISOString(),
    edited: false,
  },
];

const settings: Settings = {
  zread_time: "18:00",
  scan_interval_min: 30,
  retain_prompts: true,
  excluded_repos: [],
  claude_path: null,
  retention_days: 90,
  cost_limit_enabled: true,
};

const runs: EvalRun[] = [
  {
    id: "r1",
    day: today,
    kind: "zread",
    started_at: new Date().toISOString(),
    finished_at: new Date().toISOString(),
    status: "ok",
    model: "claude-opus-5",
    cost_usd: 0.41,
    num_turns: 6,
    duration_ms: 84000,
    session_count: 3,
    candidate_count: 2,
    error: null,
  },
];

export function mockInvoke(cmd: string, args?: Record<string, unknown>): Promise<unknown> {
  const respond = (v: unknown) => new Promise((r) => setTimeout(() => r(v), 60));
  switch (cmd) {
    case "overview":
      return respond({
        pending: candidates.length,
        journal_count: journal.length,
        session_count: 12,
        evaluating: false,
        last_scan_at: new Date().toISOString(),
        last_zread_day: null,
        zread_time: settings.zread_time,
        today,
        model: "claude-opus-5",
        claude_found: true,
        metered: false,
      } satisfies Overview);
    case "candidates":
      return respond((args?.status === "discarded" ? discarded : candidates).filter((c) => true));
    case "journal":
      return respond(journal);
    case "eval_runs":
      return respond(runs);
    case "get_settings":
      return respond(settings);
    case "export_markdown":
      return respond(
        `# Z Report — ${today}\n\n### Shipped incremental transcript ingestion\n_Committed_\n\nBuilt discovery, extraction, and verification.\n\n- ✓ cargo test passes _(Locally verified)_\n- ✓ Committed on main _(Committed)_\n`
      );
    default:
      return respond(null);
  }
}
