use crate::models::*;
use std::collections::BTreeMap;

fn level_label(level: u8) -> &'static str {
    LEVEL_LABELS
        .get(level.saturating_sub(1) as usize)
        .copied()
        .unwrap_or("Work observed")
}

fn pretty_day(day: &str) -> String {
    chrono::NaiveDate::parse_from_str(day, "%Y-%m-%d")
        .map(|d| d.format("%A, %B %-d, %Y").to_string())
        .unwrap_or_else(|_| day.to_string())
}

pub fn to_markdown(entries: &[JournalEntry], from: &str, to: &str) -> String {
    let mut by_day: BTreeMap<&str, Vec<&JournalEntry>> = BTreeMap::new();
    for e in entries {
        by_day.entry(e.day.as_str()).or_default().push(e);
    }
    let mut md = String::new();
    if from == to {
        md.push_str(&format!("# Z Report — {}\n", pretty_day(from)));
    } else {
        md.push_str(&format!("# Z Report — {from} to {to}\n"));
    }
    if entries.is_empty() {
        md.push_str("\n_No approved achievements in this range._\n");
        return md;
    }
    for (day, list) in by_day.iter() {
        if from != to {
            md.push_str(&format!("\n## {}\n", pretty_day(day)));
        }
        for e in list {
            md.push_str(&format!("\n### {}\n", e.title.trim()));
            md.push_str(&format!("_{}_\n\n", level_label(e.evidence_level)));
            md.push_str(&format!("{}\n", e.contribution.trim()));
            if !e.outcomes.is_empty() {
                md.push_str("\n");
                for o in &e.outcomes {
                    let marker = if o.verified { "✓" } else { "•" };
                    md.push_str(&format!(
                        "- {} {} _({})_\n",
                        marker,
                        o.claim.trim(),
                        level_label(o.evidence_level)
                    ));
                }
            }
        }
    }
    md
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_range_export() {
        let entries = vec![JournalEntry {
            id: "j1".into(),
            day: "2026-07-20".into(),
            title: "Fixed flaky auth test".into(),
            contribution: "Tracked down a race in token refresh.".into(),
            outcomes: vec![Outcome {
                claim: "Auth test suite passes".into(),
                evidence_level: 3,
                evidence_refs: vec!["cmd:s1:0".into()],
                verified: true,
            }],
            evidence_level: 3,
            session_ids: vec!["s1".into()],
            repo: None,
            model: Some("claude-opus-4-8".into()),
            approved_at: "2026-07-20T18:05:00+02:00".into(),
            edited: false,
        }];
        let md = to_markdown(&entries, "2026-07-14", "2026-07-20");
        assert!(md.contains("# Z Report — 2026-07-14 to 2026-07-20"));
        assert!(md.contains("### Fixed flaky auth test"));
        assert!(md.contains("✓ Auth test suite passes"));
        assert!(md.contains("Locally verified"));
    }
}
