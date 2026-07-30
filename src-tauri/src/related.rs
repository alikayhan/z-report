use crate::models::*;
use std::collections::HashSet;

pub const RELATED_THRESHOLD: f64 = 0.20;
const WINDOW_DAYS: f64 = crate::pipeline::EVAL_WINDOW_DAYS as f64;

const TITLE_WEIGHT: f64 = 0.60;
const FILE_WEIGHT: f64 = 0.25;
const BRANCH_WEIGHT: f64 = 0.15;

/// Branches every session in a repo shares, so agreeing on one says nothing.
const SHARED_BRANCHES: &[&str] = &["main", "master", "develop", "trunk", "HEAD"];

const STOPWORDS: &[&str] = &[
    "the", "and", "for", "with", "from", "into", "that", "this", "its", "was", "were", "are",
    "then", "than", "over", "about", "after", "before", "when", "what", "why", "how", "all",
    "any", "not", "but", "via", "per", "out", "off", "top",
];

/// One side of a comparison: a candidate plus the facts of the sessions it cites.
#[derive(Debug, Clone, Default)]
pub struct MatchFacts {
    pub id: String,
    pub title: String,
    pub day: String,
    pub day_end: String,
    pub repo: Option<String>,
    pub session_ids: Vec<String>,
    pub session_titles: Vec<String>,
    pub files: HashSet<String>,
    pub branches: HashSet<String>,
}

impl MatchFacts {
    pub fn build(c: &Candidate, sessions: &[SessionFacts]) -> Self {
        let cited: Vec<&SessionFacts> = sessions
            .iter()
            .filter(|s| c.session_ids.contains(&s.session_id))
            .collect();
        Self {
            id: c.id.clone(),
            title: c.title.clone(),
            day: c.day.clone(),
            day_end: c.day_end().to_string(),
            repo: c.repo.clone(),
            session_ids: c.session_ids.clone(),
            session_titles: cited.iter().filter_map(|s| s.title.clone()).collect(),
            files: cited
                .iter()
                .flat_map(|s| s.files_changed.iter().map(|f| f.path.clone()))
                .collect(),
            branches: cited.iter().filter_map(|s| s.git_branch.clone()).collect(),
        }
    }

    fn repo_name(&self) -> Option<&str> {
        self.repo.as_deref()?.rsplit(['/', '\\']).next()
    }
}

fn normalize(word: &str) -> String {
    let w: String = word
        .chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect();
    // "safety checks" and "safety check" are the same thread.
    match w.strip_suffix('s') {
        Some(stem) if stem.len() >= 3 && !stem.ends_with('s') => stem.to_string(),
        _ => w,
    }
}

/// Content words of a side's session titles. The repository name is dropped:
/// every title in a repo tends to carry it, so it separates nothing.
fn title_tokens(m: &MatchFacts) -> HashSet<String> {
    let repo = m.repo_name().map(normalize);
    m.session_titles
        .iter()
        .flat_map(|t| t.split_whitespace())
        .map(normalize)
        .filter(|w| w.len() >= 3 && !STOPWORDS.contains(&w.as_str()))
        .filter(|w| Some(w) != repo.as_ref())
        .collect()
}

fn jaccard(a: &HashSet<String>, b: &HashSet<String>) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    a.intersection(b).count() as f64 / a.union(b).count() as f64
}

fn parse_day(day: &str) -> Option<chrono::NaiveDate> {
    chrono::NaiveDate::parse_from_str(day, "%Y-%m-%d").ok()
}

/// Days between two candidates' date ranges; 0 when they overlap.
fn gap_days(a: &MatchFacts, b: &MatchFacts) -> Option<i64> {
    let (a_start, a_end) = (parse_day(&a.day)?, parse_day(&a.day_end)?);
    let (b_start, b_end) = (parse_day(&b.day)?, parse_day(&b.day_end)?);
    Some(if a_end < b_start {
        (b_start - a_end).num_days()
    } else if b_end < a_start {
        (a_start - b_end).num_days()
    } else {
        0
    })
}

fn shares_feature_branch(a: &MatchFacts, b: &MatchFacts) -> bool {
    a.branches
        .intersection(&b.branches)
        .any(|br| !SHARED_BRANCHES.contains(&br.as_str()))
}

/// How strongly two candidates look like one piece of work carried across days.
/// Zero means no suggestion at all.
pub fn score(a: &MatchFacts, b: &MatchFacts) -> f64 {
    let (Some(ra), Some(rb)) = (&a.repo, &b.repo) else {
        return 0.0;
    };
    if ra != rb || a.day == b.day {
        return 0.0;
    }
    let Some(gap) = gap_days(a, b) else {
        return 0.0;
    };
    // The evaluator already clusters within a day, so a same-day pair is a
    // split it made on purpose. Only titles can open a match: the earlier half
    // of a continuation is usually a scoping session that changed no files.
    let title = jaccard(&title_tokens(a), &title_tokens(b));
    if title <= 0.0 {
        return 0.0;
    }
    let decay = 1.0 - (gap as f64 / WINDOW_DAYS);
    if decay <= 0.0 {
        return 0.0;
    }
    let files = jaccard(&a.files, &b.files);
    let branch = if shares_feature_branch(a, b) { 1.0 } else { 0.0 };
    (TITLE_WEIGHT * title + FILE_WEIGHT * files + BRANCH_WEIGHT * branch) * decay
}

/// Identifies a suggested pair by the work it covers rather than by candidate
/// id, so dismissing one survives the day being re-evaluated into fresh ids.
pub fn pair_key(a: &[String], b: &[String]) -> String {
    let norm = |ids: &[String]| {
        let mut v: Vec<&str> = ids.iter().map(String::as_str).collect();
        v.sort_unstable();
        v.dedup();
        v.join(",")
    };
    let (x, y) = (norm(a), norm(b));
    if x <= y {
        format!("{x}~{y}")
    } else {
        format!("{y}~{x}")
    }
}

/// Best scoring partner for `subject` above the threshold. Only earlier work
/// qualifies, so a pair is suggested once — on the card that continues it.
pub fn best_match<'a>(subject: &MatchFacts, others: &'a [MatchFacts]) -> Option<(&'a MatchFacts, f64)> {
    others
        .iter()
        .filter(|o| o.id != subject.id && o.day < subject.day)
        .map(|o| (o, score(subject, o)))
        .filter(|(_, s)| *s >= RELATED_THRESHOLD)
        .max_by(|x, y| x.1.partial_cmp(&y.1).unwrap_or(std::cmp::Ordering::Equal))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn side(id: &str, day: &str, repo: &str, titles: &[&str]) -> MatchFacts {
        MatchFacts {
            id: id.into(),
            title: id.into(),
            day: day.into(),
            day_end: day.into(),
            repo: Some(repo.into()),
            session_ids: vec![format!("sess-{id}")],
            session_titles: titles.iter().map(|t| t.to_string()).collect(),
            branches: HashSet::from(["main".to_string()]),
            ..Default::default()
        }
    }

    /// The pair these weights exist to catch: a scoping session that changed no
    /// files, followed the next day by the session that built the thing.
    fn scoping() -> MatchFacts {
        side(
            "a",
            "2026-07-19",
            "/Users/x/Desktop/gymondo/synapse",
            &["Review Synapse prompt injection safety checks"],
        )
    }

    fn building() -> MatchFacts {
        let mut m = side(
            "b",
            "2026-07-20",
            "/Users/x/Desktop/gymondo/synapse",
            &["Check JIRA ticket for prompt injection safety checks"],
        );
        m.files = HashSet::from([".github/CODEOWNERS".to_string()]);
        m
    }

    #[test]
    fn links_a_scoping_session_to_the_build_that_followed() {
        assert!(score(&scoping(), &building()) >= RELATED_THRESHOLD);
    }

    #[test]
    fn scoring_is_symmetric() {
        assert_eq!(score(&scoping(), &building()), score(&building(), &scoping()));
    }

    #[test]
    fn unrelated_work_in_the_same_repo_stays_unlinked() {
        let analytics = side(
            "c",
            "2026-07-19",
            "/Users/x/Desktop/gymondo/synapse",
            &["Add analytics to plugin usage tracking"],
        );
        assert_eq!(score(&analytics, &building()), 0.0);
    }

    /// A rename-everything session overlaps heavily on files with every other
    /// card in its repo; without the title gate that alone would suggest merges.
    #[test]
    fn file_overlap_alone_never_suggests_a_merge() {
        let mut rename = side("d", "2026-07-18", "/r/inferometer", &["Explain project in simple terms"]);
        let mut mock = side("e", "2026-07-19", "/r/inferometer", &["Design Inferometer mock with agent split"]);
        let shared: HashSet<String> = (0..20).map(|i| format!("/r/inferometer/src/{i}.ts")).collect();
        rename.files = shared.clone();
        mock.files = shared;
        assert_eq!(score(&rename, &mock), 0.0);
    }

    #[test]
    fn a_session_with_no_title_cannot_match() {
        let mut pruned = building();
        pruned.session_titles.clear();
        assert_eq!(score(&scoping(), &pruned), 0.0);
    }

    #[test]
    fn different_repositories_never_match() {
        let mut elsewhere = building();
        elsewhere.repo = Some("/Users/x/Desktop/other".into());
        assert_eq!(score(&scoping(), &elsewhere), 0.0);
        let mut unknown = building();
        unknown.repo = None;
        assert_eq!(score(&scoping(), &unknown), 0.0);
    }

    #[test]
    fn same_day_cards_are_left_to_the_evaluators_clustering() {
        let mut same = building();
        same.day = scoping().day.clone();
        same.day_end = same.day.clone();
        assert_eq!(score(&scoping(), &same), 0.0);
    }

    #[test]
    fn a_shared_feature_branch_lifts_the_score_and_main_does_not() {
        let base = score(&scoping(), &building());
        let (mut a, mut b) = (scoping(), building());
        a.branches.insert("tp-479-prompt-safety".into());
        b.branches.insert("tp-479-prompt-safety".into());
        assert!(score(&a, &b) > base);
    }

    #[test]
    fn distance_past_the_window_decays_to_nothing() {
        let mut far = building();
        far.day = "2026-09-30".into();
        far.day_end = far.day.clone();
        assert_eq!(score(&scoping(), &far), 0.0);
    }

    #[test]
    fn best_match_picks_the_strongest_earlier_partner() {
        let others = vec![
            side("c", "2026-07-19", "/Users/x/Desktop/gymondo/synapse", &["Add analytics to plugin usage tracking"]),
            scoping(),
        ];
        let (winner, _) = best_match(&building(), &others).unwrap();
        assert_eq!(winner.id, "a");
    }

    #[test]
    fn only_the_later_card_carries_the_suggestion() {
        let sides = vec![scoping(), building()];
        assert!(best_match(&scoping(), &sides).is_none());
        assert!(best_match(&building(), &sides).is_some());
    }

    #[test]
    fn pair_key_is_independent_of_order() {
        let (a, b) = (vec!["s2".to_string(), "s1".into()], vec!["s3".to_string()]);
        assert_eq!(pair_key(&a, &b), pair_key(&b, &a));
        assert_ne!(pair_key(&a, &b), pair_key(&a, &a));
    }
}
