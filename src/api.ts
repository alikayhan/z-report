import { invoke as tauriInvoke } from "@tauri-apps/api/core";

export const inTauri = "__TAURI_INTERNALS__" in window;

async function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  if (inTauri) return tauriInvoke<T>(cmd, args);
  const { mockInvoke } = await import("./mock");
  return mockInvoke(cmd, args) as Promise<T>;
}

export * from "./types";
import type { Candidate, EvalRun, ExportData, JournalEntry, Outcome, Overview, Settings } from "./types";

export const api = {
  overview: () => invoke<Overview>("overview"),
  runXread: () => invoke<void>("run_xread"),
  cancelXread: () => invoke<void>("cancel_xread"),
  candidates: (status: string) => invoke<Candidate[]>("candidates", { status }),
  updateCandidate: (id: string, title: string, contribution: string, outcomes: Outcome[], revision: number) =>
    invoke<void>("update_candidate", { id, title, contribution, outcomes, revision }),
  approve: (id: string, edited: boolean, revision: number) => invoke<void>("approve_candidate", { id, edited, revision }),
  discard: (id: string, revision: number) => invoke<void>("discard_candidate", { id, revision }),
  restore: (id: string, revision: number) => invoke<void>("restore_candidate", { id, revision }),
  merge: (ids: string[], revisions: number[]) => invoke<string>("merge_candidates", { ids, revisions }),
  dismissRelated: (id: string) => invoke<void>("dismiss_related", { id }),
  journal: (from: string, to: string, query?: string) =>
    invoke<JournalEntry[]>("journal", { from, to, query: query ?? null }),
  confirmImpact: (id: string, note: string) => invoke<void>("confirm_impact", { id, note }),
  deleteJournalEntry: (id: string) => invoke<void>("delete_journal_entry", { id }),
  exportData: (from: string, to: string) => invoke<ExportData>("export_data", { from, to }),
  writeFile: (path: string, content: string) => invoke<void>("write_file", { path, content }),
  getSettings: () => invoke<Settings>("get_settings"),
  setSettings: (settings: Settings) => invoke<void>("set_settings", { settings }),
  evalRuns: () => invoke<EvalRun[]>("eval_runs"),
  deleteAllData: () => invoke<void>("delete_all_data"),
  installUpdate: () => invoke<void>("install_update"),
  restartApp: () => invoke<void>("restart_app"),
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
