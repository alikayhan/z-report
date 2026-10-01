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

export type Agent = "claude" | "codex";

const AGENT_LABELS: Record<Agent, string> = {
  claude: "Claude Code",
  codex: "Codex",
};

export function agentsLabel(agents: Agent[]): string {
  return agents.map((agent) => AGENT_LABELS[agent]).join(" + ");
}

export interface SessionFacts {
  session_id: string;
  agent: Agent;
  title: string | null;
  cli_version: string | null;
  first_ts: string | null;
  last_ts: string | null;
  git_branch: string | null;
  commands: unknown[];
  commits: unknown[];
  files_changed: unknown[];
  pr_links: PrLink[];
}

export interface EvaluatorInfo {
  agent: Agent;
  model: string;
  effort: string;
  found: boolean;
}

export interface RelatedLink {
  kind: "continuation" | "journaled";
  target_id: string;
  target_title: string;
  target_day: string;
  pair_key: string;
}

export interface Candidate {
  revision: number;
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
  agents: Agent[];
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
  agents: Agent[];
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
  auto_catchup: boolean;
}

export interface UpdateInfo {
  version: string;
}

export interface Overview {
  pending: number;
  session_count: number;
  evaluating: boolean;
  last_scan_at: string | null;
  zread_time: string;
  today: string;
  evaluators: EvaluatorInfo[];
  metered: boolean;
  app_version: string;
  update: UpdateInfo | null;
  update_ready: boolean;
}

export interface ExportData {
  markdown: string;
  entries: JournalEntry[];
}
