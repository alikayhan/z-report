use z_report_core::pipeline;
use z_report_lib::evaluator;
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
    eprintln!(
        "evaluating {} substantial session(s) for {day}",
        sessions.len()
    );
    if sessions.is_empty() {
        return Ok(());
    }

    let lifecycle = z_report_lib::lifecycle::Lifecycle::default();
    let mut claude = lifecycle.begin_evaluation().unwrap();
    claude.attach_lock(z_report_core::engine::lock(&store, "read")?);
    let result = evaluator::evaluate_day(&settings, &claude, &day, &sessions)?;
    eprintln!(
        "served by {} — ${:.2}, {} turns, {:.0}s",
        result.model.as_deref().unwrap_or("?"),
        result.cost_usd.unwrap_or(0.0),
        result.num_turns.unwrap_or(0),
        result.duration_ms.unwrap_or(0) as f64 / 1000.0
    );

    for a in pipeline::prepare_achievements(result.achievements, &sessions) {
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
        for u in &a.uncertainties {
            println!("  ? {u}");
        }
    }
    Ok(())
}
