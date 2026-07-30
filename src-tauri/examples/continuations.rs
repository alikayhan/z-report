//! Read-only threshold harness. Usage: cargo run --example continuations [path/to/zreport.db]

use std::collections::HashMap;
use z_report_lib::ingest;
use z_report_lib::models::SessionFacts;
use z_report_lib::related::{self, MatchFacts, RELATED_THRESHOLD};
use z_report_lib::store::Store;

fn main() -> anyhow::Result<()> {
    let store = match std::env::args().nth(1) {
        Some(path) => Store::open(path)?,
        None => Store::open_default()?,
    };
    let pending = store.candidates_by_status("pending")?;
    eprintln!("{} pending candidate(s)", pending.len());

    // Facts stored before titles were captured have none, so re-parse.
    let paths: HashMap<String, _> = ingest::discover()
        .into_iter()
        .map(|f| (f.session_id, f.path))
        .collect();
    let mut sessions: Vec<SessionFacts> = Vec::new();
    for id in pending.iter().flat_map(|c| &c.session_ids) {
        let Some(path) = paths.get(id) else { continue };
        if let Ok(facts) = ingest::parse_transcript(path, id, true) {
            sessions.push(facts);
        }
    }
    eprintln!("{} transcript(s) still on disk\n", sessions.len());

    let sides: Vec<MatchFacts> = pending
        .iter()
        .map(|c| MatchFacts::build(c, &sessions))
        .collect();

    let mut scored: Vec<(f64, &MatchFacts, &MatchFacts)> = Vec::new();
    for (i, a) in sides.iter().enumerate() {
        for b in sides.iter().skip(i + 1) {
            let s = related::score(a, b);
            if s > 0.0 {
                scored.push((s, a, b));
            }
        }
    }
    scored.sort_by(|x, y| y.0.partial_cmp(&x.0).unwrap());

    if scored.is_empty() {
        eprintln!("no pair scored above zero");
    }
    for (s, a, b) in &scored {
        println!(
            "{:.3} {}  {} [{}]\n            {} [{}]",
            s,
            if *s >= RELATED_THRESHOLD { "SUGGEST" } else { "below  " },
            a.title,
            a.day,
            b.title,
            b.day
        );
    }
    Ok(())
}
