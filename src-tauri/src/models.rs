use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::hash::Hash;

pub const LEVEL_LABELS: [&str; 5] = [
    "Work observed",
    "Change produced",
    "Locally verified",
    "Committed",
    "Impact confirmed",
];

pub fn level_label(level: u8) -> &'static str {
    LEVEL_LABELS
        .get(level.saturating_sub(1) as usize)
        .copied()
        .unwrap_or(LEVEL_LABELS[0])
}

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
    #[serde(default)]
    pub title: Option<String>,
    pub prompts: Vec<String>,
    pub final_response: Option<String>,
    pub files_changed: Vec<FileChange>,
    pub commands: Vec<CommandFact>,
    pub commits: Vec<CommitFact>,
    #[serde(default)]
    pub pr_links: Vec<PrLink>,
    #[serde(default)]
    pub external_actions: Vec<ExternalAction>,
}

impl SessionFacts {
    pub fn has_substance(&self) -> bool {
        !self.prompts.is_empty()
            || !self.files_changed.is_empty()
            || !self.commands.is_empty()
            || !self.commits.is_empty()
            || !self.pr_links.is_empty()
            || self.external_changes().next().is_some()
    }

    pub fn external_changes(&self) -> impl Iterator<Item = &ExternalAction> {
        self.external_actions.iter().filter(|a| a.mutating)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileChange {
    pub path: String,
    pub tool: String,
    pub count: u32,
    #[serde(default)]
    pub via_delegate: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandFact {
    pub id: String,
    pub command: String,
    pub ok: bool,
    pub kind: String,
    pub ts: Option<String>,
    #[serde(default)]
    pub via_delegate: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExternalAction {
    pub id: String,
    pub server: String,
    pub tool: String,
    pub ok: bool,
    pub mutating: bool,
    pub ts: Option<String>,
    #[serde(default)]
    pub via_delegate: bool,
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PrLink {
    pub number: u64,
    pub url: String,
    pub repository: String,
    pub ts: Option<String>,
}

impl PrLink {
    const REF_PREFIX: &'static str = "pr:";

    pub fn evidence_ref(&self) -> String {
        format!("{}{}#{}", Self::REF_PREFIX, self.repository, self.number)
    }

    pub fn parse_evidence_ref(value: &str) -> Option<(&str, u64)> {
        let (repository, number) = value.strip_prefix(Self::REF_PREFIX)?.rsplit_once('#')?;
        Some((repository, number.parse().ok()?))
    }

    pub fn has_canonical_url(&self) -> bool {
        let mut parts = self.repository.split('/');
        let (Some(owner), Some(repo), None) = (parts.next(), parts.next(), parts.next()) else {
            return false;
        };
        let safe = |part: &str| {
            !part.is_empty()
                && part
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        };
        safe(owner)
            && safe(repo)
            && self.number > 0
            && self.url == format!("https://github.com/{}/{}/pull/{}", owner, repo, self.number)
    }
}

pub(crate) fn unique_by<T, K>(
    items: impl IntoIterator<Item = T>,
    mut key: impl FnMut(&T) -> K,
) -> Vec<T>
where
    K: Eq + Hash,
{
    let mut seen = HashSet::new();
    items
        .into_iter()
        .filter(|item| seen.insert(key(item)))
        .collect()
}

pub fn unique_pr_links<'a>(links: impl IntoIterator<Item = &'a PrLink>) -> Vec<PrLink> {
    unique_by(links.into_iter().cloned(), |pr| pr.url.clone())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Outcome {
    pub claim: String,
    pub evidence_level: u8,
    pub evidence_refs: Vec<String>,
    #[serde(default)]
    pub verified: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RelatedKind {
    Continuation,
    Journaled,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RelatedLink {
    pub kind: RelatedKind,
    pub target_id: String,
    pub target_title: String,
    pub target_day: String,
    #[serde(default)]
    pub pair_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Candidate {
    pub id: String,
    pub day: String,
    #[serde(default)]
    pub day_end: Option<String>,
    pub title: String,
    pub contribution: String,
    pub outcomes: Vec<Outcome>,
    pub uncertainties: Vec<String>,
    pub confidence: f64,
    pub evidence_level: u8,
    pub session_ids: Vec<String>,
    #[serde(default)]
    pub pr_links: Vec<PrLink>,
    pub repo: Option<String>,
    pub model: Option<String>,
    pub status: String,
    #[serde(default)]
    pub related: Option<RelatedLink>,
    pub created_at: String,
}

impl Candidate {
    pub fn day_end(&self) -> &str {
        self.day_end.as_deref().unwrap_or(&self.day)
    }
}

impl JournalEntry {
    pub fn day_end(&self) -> &str {
        self.day_end.as_deref().unwrap_or(&self.day)
    }

    pub fn from_candidate(candidate: &Candidate, approved_at: String, edited: bool) -> Self {
        Self {
            id: format!("j-{}", candidate.id),
            day: candidate.day.clone(),
            day_end: candidate.day_end.clone(),
            title: candidate.title.clone(),
            contribution: candidate.contribution.clone(),
            outcomes: candidate.outcomes.clone(),
            evidence_level: candidate.evidence_level,
            session_ids: candidate.session_ids.clone(),
            pr_links: candidate.pr_links.clone(),
            repo: candidate.repo.clone(),
            model: candidate.model.clone(),
            approved_at,
            edited,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JournalEntry {
    pub id: String,
    pub day: String,
    #[serde(default)]
    pub day_end: Option<String>,
    pub title: String,
    pub contribution: String,
    pub outcomes: Vec<Outcome>,
    pub evidence_level: u8,
    pub session_ids: Vec<String>,
    #[serde(default)]
    pub pr_links: Vec<PrLink>,
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
    pub claude_path: Option<String>,
    pub retention_days: u32,
    pub cost_limit_enabled: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            zread_time: "18:00".into(),
            scan_interval_min: 30,
            retain_prompts: true,
            excluded_repos: Vec::new(),
            claude_path: None,
            retention_days: 90,
            cost_limit_enabled: true,
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
