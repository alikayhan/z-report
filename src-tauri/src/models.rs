use serde::{Deserialize, Serialize};

pub const LEVEL_LABELS: [&str; 5] = [
    "Work observed",
    "Change produced",
    "Locally verified",
    "Committed",
    "Impact confirmed",
];

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionFacts {
    pub session_id: String,
    pub file_path: String,
    pub cwd: Option<String>,
    pub git_branch: Option<String>,
    pub repo_root: Option<String>,
    pub first_ts: Option<String>,
    pub last_ts: Option<String>,
    pub cli_version: Option<String>,
    pub prompts: Vec<String>,
    pub final_response: Option<String>,
    pub files_changed: Vec<FileChange>,
    pub commands: Vec<CommandFact>,
    pub commits: Vec<CommitFact>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileChange {
    pub path: String,
    pub tool: String,
    pub count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandFact {
    pub id: String,
    pub command: String,
    pub ok: bool,
    pub kind: String,
    pub ts: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommitFact {
    pub sha: String,
    pub subject: String,
    pub ts: String,
    pub files: u32,
    pub insertions: u32,
    pub deletions: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Outcome {
    pub claim: String,
    pub evidence_level: u8,
    pub evidence_refs: Vec<String>,
    #[serde(default)]
    pub verified: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Candidate {
    pub id: String,
    pub day: String,
    pub title: String,
    pub contribution: String,
    pub outcomes: Vec<Outcome>,
    pub uncertainties: Vec<String>,
    pub confidence: f64,
    pub evidence_level: u8,
    pub session_ids: Vec<String>,
    pub repo: Option<String>,
    pub model: Option<String>,
    pub status: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JournalEntry {
    pub id: String,
    pub day: String,
    pub title: String,
    pub contribution: String,
    pub outcomes: Vec<Outcome>,
    pub evidence_level: u8,
    pub session_ids: Vec<String>,
    pub repo: Option<String>,
    pub model: Option<String>,
    pub approved_at: String,
    pub edited: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalRun {
    pub id: String,
    pub day: String,
    pub kind: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub status: String,
    pub model: Option<String>,
    pub cost_usd: Option<f64>,
    pub num_turns: Option<i64>,
    pub duration_ms: Option<i64>,
    pub session_count: i64,
    pub candidate_count: i64,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub zread_time: String,
    pub scan_interval_min: u32,
    pub retain_prompts: bool,
    pub excluded_repos: Vec<String>,
    pub max_budget_usd: f64,
    pub claude_path: Option<String>,
    pub retention_days: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            zread_time: "18:00".into(),
            scan_interval_min: 30,
            retain_prompts: true,
            excluded_repos: Vec::new(),
            max_budget_usd: 3.0,
            claude_path: None,
            retention_days: 90,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Achievement {
    pub title: String,
    pub contribution: String,
    pub outcomes: Vec<Outcome>,
    #[serde(default)]
    pub uncertainties: Vec<String>,
    pub confidence: f64,
    pub session_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvaluatorOutput {
    pub achievements: Vec<Achievement>,
}
