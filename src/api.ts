import { invoke as tauriInvoke } from "@tauri-apps/api/core";

export const inTauri = "__TAURI_INTERNALS__" in window;

async function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  if (inTauri) return tauriInvoke<T>(cmd, args);
  const { mockInvoke } = await import("./mock");
  return mockInvoke(cmd, args) as Promise<T>;
}

export interface Outcome {
  claim: string;
  evidence_level: number;
  evidence_refs: string[];
  verified: boolean;
}

export interface PrLink {
  number: number;
  url: string;
  repository: string;
  ts: string | null;
}

export interface RelatedLink {
  kind: "continuation" | "journaled";
  target_id: string;
  target_title: string;
  target_day: string;
  score: number;
  pair_key: string;
}

export interface Candidate {
  id: string;
  day: string;
  day_end: string | null;
  title: string;
  contribution: string;
  outcomes: Outcome[];
  uncertainties: string[];
  confidence: number;
  evidence_level: number;
  session_ids: string[];
  pr_links: PrLink[];
  repo: string | null;
  model: string | null;
  status: string;
  related: RelatedLink | null;
  created_at: string;
}

export interface JournalEntry {
  id: string;
  day: string;
  day_end: string | null;
  title: string;
  contribution: string;
  outcomes: Outcome[];
  evidence_level: number;
  session_ids: string[];
  pr_links: PrLink[];
  repo: string | null;
  model: string | null;
  approved_at: string;
  edited: boolean;
}

export interface EvalRun {
  id: string;
  day: string;
  kind: string;
  started_at: string;
  finished_at: string | null;
  status: string;
  model: string | null;
  cost_usd: number | null;
  num_turns: number | null;
  duration_ms: number | null;
  session_count: number;
  candidate_count: number;
  error: string | null;
}

export interface Settings {
  zread_time: string;
  scan_interval_min: number;
  retain_prompts: boolean;
  excluded_repos: string[];
  claude_path: string | null;
  retention_days: number;
  cost_limit_enabled: boolean;
}

export interface Overview {
  pending: number;
  journal_count: number;
  session_count: number;
  evaluating: boolean;
  last_scan_at: string | null;
  last_zread_day: string | null;
  zread_time: string;
  today: string;
  model: string;
  claude_found: boolean;
  metered: boolean;
}

export const api = {
  overview: () => invoke<Overview>("overview"),
  scanNow: () => invoke<number>("scan_now"),
  runXread: () => invoke<void>("run_xread"),
  candidates: (status: string) => invoke<Candidate[]>("candidates", { status }),
  updateCandidate: (id: string, title: string, contribution: string, outcomes: Outcome[]) =>
    invoke<void>("update_candidate", { id, title, contribution, outcomes }),
  approve: (id: string, edited: boolean) => invoke<void>("approve_candidate", { id, edited }),
  discard: (id: string) => invoke<void>("discard_candidate", { id }),
  restore: (id: string) => invoke<void>("restore_candidate", { id }),
  merge: (ids: string[]) => invoke<string>("merge_candidates", { ids }),
  dismissRelated: (id: string) => invoke<void>("dismiss_related", { id }),
  journal: (from: string, to: string, query?: string) =>
    invoke<JournalEntry[]>("journal", { from, to, query: query ?? null }),
  confirmImpact: (id: string, note: string) => invoke<void>("confirm_impact", { id, note }),
  deleteJournalEntry: (id: string) => invoke<void>("delete_journal_entry", { id }),
  exportMarkdown: (from: string, to: string) => invoke<string>("export_markdown", { from, to }),
  writeFile: (path: string, content: string) => invoke<void>("write_file", { path, content }),
  getSettings: () => invoke<Settings>("get_settings"),
  setSettings: (settings: Settings) => invoke<void>("set_settings", { settings }),
  evalRuns: () => invoke<EvalRun[]>("eval_runs"),
  setPinned: (pinned: boolean) => invoke<void>("set_pinned", { pinned }),
  hideWindow: () => invoke<void>("hide_window"),
  deleteAllData: () => invoke<void>("delete_all_data"),
};

export const LEVEL_LABELS = [
  "Work observed",
  "Change produced",
  "Locally verified",
  "Committed",
  "Impact confirmed",
];

export function levelLabel(level: number): string {
  return LEVEL_LABELS[Math.min(Math.max(level, 1), 5) - 1];
}
