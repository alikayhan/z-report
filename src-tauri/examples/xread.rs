//! Manual end-to-end harness: evaluate one day's ingested sessions and print
//! the drafted achievements without writing to the review queue.
//! Usage: cargo run --example xread [YYYY-MM-DD]

use z_report_lib::evaluator;
use z_report_lib::gitfacts;
use z_report_lib::models::SessionFacts;
use z_report_lib::store::Store;

fn main() -> anyhow::Result<()> {
    let day = std::env::args()
        .nth(1)
        .unwrap_or_else(|| chrono::Local::now().format("%Y-%m-%d").to_string());
    let store = Store::open_default()?;
    let settings = store.settings();
    let sessions: Vec<SessionFacts> = store
        .pending_sessions(&day, &day)?
        .into_iter()
        .map(|(_, f)| f)
        .filter(SessionFacts::has_substance)
        .collect();
    eprintln!("evaluating {} substantial session(s) for {day}", sessions.len());
    if sessions.is_empty() {
        return Ok(());
    }

    let result = evaluator::evaluate_day(&settings, &day, &sessions)?;
    eprintln!(
        "served by {} — ${:.2}, {} turns, {:.0}s",
        result.model.as_deref().unwrap_or("?"),
        result.cost_usd.unwrap_or(0.0),
        result.num_turns.unwrap_or(0),
        result.duration_ms.unwrap_or(0) as f64 / 1000.0
    );

    for mut a in result.achievements {
        let cited: Vec<&SessionFacts> = sessions
            .iter()
            .filter(|s| a.session_ids.contains(&s.session_id))
            .collect();
        let mut uncertainties = a.uncertainties.clone();
        for o in a.outcomes.iter_mut() {
            gitfacts::verify_outcome(o, &cited, &mut uncertainties);
        }
        println!("\n## {} (conf {:.2})", a.title, a.confidence);
        println!("{}", a.contribution);
        for o in &a.outcomes {
            println!(
                "  [{}L{}] {}",
                if o.verified { "✓ " } else { "△ " },
                o.evidence_level,
                o.claim
            );
        }
        for u in &uncertainties {
            println!("  ? {u}");
        }
    }
    Ok(())
}
