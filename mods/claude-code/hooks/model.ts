import type { ProcessRunInit, ProcessRunResult, Timer } from "claude-code";
import type {
  Candidate,
  JournalEntry,
  Settings,
  ExportData,
  EvalRun,
} from "../../../src/types";
export type { Candidate, Settings };
export function atLeast(version: string, minimum: string): boolean {
  const parse = (v: string) => v.split(".").map((n) => Number.parseInt(n, 10));
  const a = parse(version),
    b = parse(minimum);
  if (a.some(Number.isNaN) || a.length === 0) return false;
  for (let i = 0; i < Math.max(a.length, b.length); i++) {
    const x = a[i] ?? 0,
      y = b[i] ?? 0;
    if (x !== y) return x > y;
  }
  return true;
}

// The terminal Select refuses more than 64 options; the picker pages at that size.
export const PICKER_LIMIT = 64;

export type Host = {
  root: string;
  run: (argv: string[], init: ProcessRunInit) => Promise<ProcessRunResult>;
  dataOverride: () => Promise<string | undefined>;
  redraw: () => void;
  log: (text: string) => void;
  every: (ms: number, fn: () => void) => Timer;
};
type Read = {
  id: string;
  status: string;
  message: string;
  completed_days: number;
  total_days: number;
  candidate_count: number;
  scan: { diagnostics: string[] } | null;
};
type Overview = {
  today: string;
  initialized: boolean;
  last_successful_read_at: string | null;
  read: Read | null;
  settings: Settings;
  evaluators: { agent: string; found: boolean; model: string }[];
};
export const active = (read: Read | null) =>
  !!read && ["queued", "running"].includes(read.status);
export class Model {
  tab = "review";
  status = "pending";
  overview: Overview | null = null;
  read: Read | null = null;
  candidates: Candidate[] = [];
  selected = "";
  mergeIds: string[] = [];
  editing: Candidate | null = null;
  evidence = "";
  journal: JournalEntry[] = [];
  export: ExportData | null = null;
  settings: Settings | null = null;
  runs: EvalRun[] = [];
  from = "";
  to = "";
  query = "";
  savePath = "";
  error = "";
  notice = "";
  busy = false;
  private directory?: string;
  private owner = crypto.randomUUID();
  private owned = false;
  private timer?: Timer;
  private polling = false;
  private checking = false;
  private lastAutoCheck = 0;
  constructor(readonly host: Host) {}
  get candidate() {
    return (
      this.candidates.find((c) => c.id === this.selected) ?? this.candidates[0]
    );
  }
  get picker() {
    const index = Math.max(
      0,
      this.candidates.findIndex((c) => c.id === this.candidate?.id),
    );
    const start = index - (index % PICKER_LIMIT);
    return {
      items: this.candidates.slice(start, start + PICKER_LIMIT),
      start,
      total: this.candidates.length,
    };
  }
  pick(id: string | undefined) {
    if (!id) return;
    this.selected = id;
    this.editing = null;
    this.evidence = "";
    this.host.redraw();
  }
  async call<T>(action: string, fields: object = {}): Promise<T> {
    if (this.directory === undefined) {
      const result = await this.host.run(
        [`${this.host.root}/bin/z-report`, "info"],
        { timeoutMs: 10000 },
      );
      const info = JSON.parse(result.stdout);
      if (
        result.exitCode !== 0 ||
        info.protocol !== 1 ||
        !info.ok ||
        info.data?.engine !== "z-report" ||
        info.data?.database_version !== 1 ||
        typeof info.data?.tested_host !== "string"
      )
        throw new Error(
          "Incompatible Z Report engine. Reinstall the matching package.",
        );
      const host = await this.host.run(["claude", "--version"], {
        timeoutMs: 10000,
      });
      if (
        host.exitCode !== 0 ||
        !atLeast(
          host.stdout.trim().split(/\s+/)[0] ?? "",
          info.data.tested_host,
        )
      ) {
        throw new Error(
          `This Z Report package needs Claude Code ${info.data.tested_host} or newer. Update Claude Code or install a matching Z Report package.`,
        );
      }
      this.directory = (await this.host.dataOverride()) ?? "";
    }
    const args = this.directory
      ? ["--data-dir", this.directory, "rpc"]
      : ["rpc"];
    const result = await this.host.run(
      [`${this.host.root}/bin/z-report`, ...args],
      {
        stdin: JSON.stringify({ protocol: 1, action, ...fields }),
        timeoutMs: 10000,
      },
    );
    let response;
    try {
      response = JSON.parse(result.stdout);
    } catch {
      throw new Error(
        "Cannot read the engine response. Reinstall Z Report if this persists.",
      );
    }
    if (response.protocol !== 1)
      throw new Error("Incompatible Z Report protocol");
    if (!response.ok || result.exitCode !== 0)
      throw new Error(response.error?.message ?? "Engine request failed");
    return response.data as T;
  }
  async act(fn: () => Promise<void>) {
    if (this.busy) return;
    this.busy = true;
    this.error = "";
    this.notice = "";
    this.host.redraw();
    try {
      await fn();
    } catch (e) {
      this.error = e instanceof Error ? e.message : String(e);
    } finally {
      this.busy = false;
      this.host.redraw();
    }
  }
  async refresh() {
    this.overview = await this.call<Overview>("overview");
    this.read = this.overview.read;
    this.settings ??= { ...this.overview.settings };
    this.from ||= this.overview.today;
    this.to ||= this.overview.today;
    await this.loadTab(this.overview.settings);
    if (active(this.read)) this.watch();
  }
  async loadTab(settings?: Settings) {
    if (this.tab === "review") {
      this.candidates = await this.call("candidates", { status: this.status });
      this.mergeIds = this.mergeIds.filter((id) =>
        this.candidates.some((c) => c.id === id),
      );
    }
    if (this.tab === "journal")
      this.journal = await this.call("journal", {
        from: this.from,
        to: this.to,
        query: this.query,
      });
    if (this.tab === "export")
      this.export = await this.call("export", { from: this.from, to: this.to });
    if (this.tab === "settings") {
      [this.settings, this.runs] = await Promise.all([
        settings ? Promise.resolve(settings) : this.call<Settings>("settings"),
        this.call<EvalRun[]>("eval_runs"),
      ]);
    }
  }
  async start(automatic = false) {
    if (active(this.read)) return;
    const result = await this.call<{ read: Read; owned: boolean } | null>(
      "read_start",
      { owner: this.owner, automatic, interactive: true },
    );
    if (!result) return;
    this.read = result.read;
    this.owned = result.owned;
    this.watch();
  }
  async auto() {
    if (
      this.checking ||
      this.busy ||
      active(this.read) ||
      Date.now() - this.lastAutoCheck < 15 * 60 * 1000
    )
      return;
    this.lastAutoCheck = Date.now();
    this.checking = true;
    try {
      await this.start(true);
    } catch (e) {
      this.error = e instanceof Error ? e.message : String(e);
    } finally {
      this.checking = false;
      this.host.redraw();
    }
  }
  private watch() {
    if (!this.timer)
      this.timer = this.host.every(2000, () => {
        void this.poll();
      });
  }
  private async poll() {
    if (this.polling || !this.read) return;
    this.polling = true;
    try {
      this.read = await this.call(
        this.owned ? "read_heartbeat" : "read_status",
        { id: this.read.id, ...(this.owned ? { owner: this.owner } : {}) },
      );
      if (!active(this.read)) {
        this.timer?.cancel();
        this.timer = undefined;
        if (this.owned && this.read)
          this.host.log(
            `Z Report: ${this.read.status} · ${this.read.candidate_count} candidates. Open /z-report to review.`,
          );
        this.owned = false;
        await this.refresh();
      }
    } catch (e) {
      this.error = e instanceof Error ? e.message : String(e);
      this.timer?.cancel();
      this.timer = undefined;
    } finally {
      this.polling = false;
      this.host.redraw();
    }
  }
  async cancel() {
    if (this.read)
      this.read = await this.call("read_cancel", { id: this.read.id });
  }
  async mutate(action: string, candidate: Candidate) {
    await this.call(action, { id: candidate.id, revision: candidate.revision });
    this.editing = null;
    this.evidence = "";
    await this.refresh();
  }
  async saveEdit() {
    if (!this.editing) return;
    const c = this.editing;
    await this.call("edit_candidate", {
      id: c.id,
      revision: c.revision,
      title: c.title,
      contribution: c.contribution,
      outcomes: c.outcomes,
    });
    this.editing = null;
    await this.refresh();
  }
  async merge() {
    const selected = this.candidates.filter((c) =>
      this.mergeIds.includes(c.id),
    );
    const merged = await this.call<Candidate>("merge_candidates", {
      ids: selected.map((c) => c.id),
      revisions: selected.map((c) => c.revision),
    });
    this.selected = merged.id;
    this.mergeIds = [];
    await this.refresh();
  }
  async showEvidence(c: Candidate) {
    const evidence = await this.call<unknown[]>("evidence", {
      ids: c.session_ids,
    });
    this.evidence = JSON.stringify(evidence, null, 2);
  }
  period(days: number) {
    const end = this.overview?.today ?? this.to;
    const date = new Date(`${end}T12:00:00Z`);
    date.setUTCDate(date.getUTCDate() - days + 1);
    this.to = end;
    this.from = date.toISOString().slice(0, 10);
  }
}
