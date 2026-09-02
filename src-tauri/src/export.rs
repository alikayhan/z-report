use crate::calendar;
use crate::models::*;
use std::collections::BTreeMap;

fn short_day(day: &str) -> String {
    calendar::format(day, "%b %-d")
}

fn pretty_day(day: &str) -> String {
    calendar::format(day, "%A, %B %-d, %Y")
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
            let span = if e.day_end() != e.day {
                format!("{} – {} · ", short_day(&e.day), short_day(e.day_end()))
            } else {
                String::new()
            };
            md.push_str(&format!("_{}{}_\n\n", span, level_label(e.evidence_level)));
            md.push_str(&format!("{}\n", e.contribution.trim()));
            if !e.outcomes.is_empty() {
                md.push('\n');
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
            if !e.pr_links.is_empty() {
                let links: Vec<String> = e
                    .pr_links
                    .iter()
                    .map(|pr| format!("[{}#{}]({})", pr.repository, pr.number, pr.url))
                    .collect();
                md.push_str(&format!("\n{}\n", links.join(" · ")));
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
            day_end: None,
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
            agents: vec![],
            pr_links: vec![PrLink {
                number: 5159,
                url: "https://github.com/acme/widgets/pull/5159".into(),
                repository: "acme/widgets".into(),
                ts: None,
            }],
            repo: None,
            model: Some("claude-opus-5".into()),
            approved_at: "2026-07-20T18:05:00+02:00".into(),
            edited: false,
        }];
        let spanning = JournalEntry {
            id: "j2".into(),
            day: "2026-07-18".into(),
            day_end: Some("2026-07-20".into()),
            title: "Added contribution checks".into(),
            contribution: "Scoped then shipped the scanner.".into(),
            outcomes: vec![],
            evidence_level: 4,
            session_ids: vec!["s2".into()],
            agents: vec![],
            pr_links: vec![],
            repo: None,
            model: None,
            approved_at: "2026-07-20T18:06:00+02:00".into(),
            edited: false,
        };
        let entries = [entries, vec![spanning]].concat();
        let md = to_markdown(&entries, "2026-07-14", "2026-07-20");
        assert!(md.contains("_Jul 18 – Jul 20 · Committed_"));
        assert!(
            md.contains("_Locally verified_"),
            "single-day entries keep a bare label"
        );
        assert!(md.contains("# Z Report — 2026-07-14 to 2026-07-20"));
        assert!(md.contains("### Fixed flaky auth test"));
        assert!(md.contains("✓ Auth test suite passes"));
        assert!(md.contains("Locally verified"));
        assert!(md.contains("[acme/widgets#5159](https://github.com/acme/widgets/pull/5159)"));
    }
}
