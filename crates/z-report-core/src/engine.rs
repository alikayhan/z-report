use crate::{
    calendar, evaluator, gitfacts, ingest,
    lifecycle::{EvaluatorGuard, Lifecycle},
    models::*,
    pipeline,
    store::Store,
};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    io::Read,
    os::fd::AsRawFd,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};

pub use crate::store::DATABASE_VERSION;

pub const EVAL_WINDOW_DAYS: i64 = 15;
pub const PROTOCOL: u32 = 1;

pub struct Engine {
    pub store: Mutex<Store>,
    pub lifecycle: Lifecycle,
    pub availability: Mutex<evaluator::Availability>,
}

impl Engine {
    pub fn new(store: Store) -> Self {
        Self {
            store: Mutex::new(store),
            lifecycle: Lifecycle::default(),
            availability: Mutex::new(evaluator::Availability::default()),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ScanOutcome {
    pub updated: u32,
    pub complete: bool,
    pub diagnostics: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReadCycle {
    pub id: String,
    pub origin: String,
    pub status: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub completed_days: u32,
    pub total_days: u32,
    pub candidate_count: u32,
    pub message: String,
    pub scan: Option<ScanOutcome>,
}

pub fn today() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}
pub fn new_id() -> String {
    let mut bytes = [0u8; 16];
    File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .expect("OS randomness unavailable");
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn lock(store: &Store, name: &str) -> Result<File> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(store.path.with_extension(format!("{name}.lock")))?;
    anyhow::ensure!(
        unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0,
        "busy: another process is using this store"
    );
    Ok(file)
}

pub fn scan(engine: &Engine) -> Result<ScanOutcome> {
    let (_scan_lock, settings, known) = {
        let s = engine.store.lock().unwrap();
        (lock(&s, "scan")?, s.settings(), s.session_content_hashes()?)
    };
    let (files, mut diagnostics) = ingest::discover_checked();
    let stale_before = chrono::Utc::now().timestamp() - 90 * 86_400;
    let mut updated = 0;
    for file in &files {
        if (file.mtime as i64) < stale_before {
            continue;
        }
        if known.get(&file.session_id) == Some(&file.content_hash) {
            continue;
        }
        match ingest::parse_transcript(file, settings.retain_prompts) {
            Ok(mut facts) => {
                gitfacts::correlate(&mut facts);
                if settings.excludes(&facts) {
                    continue;
                }
                let Some(day) = ingest::session_day(&facts) else {
                    if facts.has_substance() {
                        diagnostics.push(format!(
                            "{}: evidence has no valid timestamp",
                            file.path.display()
                        ));
                    }
                    continue;
                };
                if engine.store.lock().unwrap().upsert_session_if_changed(
                    &facts,
                    &day,
                    &file.content_hash,
                )? {
                    updated += 1;
                }
            }
            Err(e) => diagnostics.push(format!("{}: {e}", file.path.display())),
        }
    }
    let complete = diagnostics.is_empty();
    let store = engine.store.lock().unwrap();
    if complete {
        if settings.retention_days > 0 {
            store.prune_candidates_older_than(&calendar::offset(
                &today(),
                -(settings.retention_days as i64),
            ))?;
        }
        store.prune_sessions_older_than(&calendar::offset(&today(), -97))?;
        // Discovery failures never remove evidence that temporarily became unreadable.
        store.delete_sessions_missing_from(
            &files
                .iter()
                .map(|f| f.session_id.clone())
                .collect::<Vec<_>>(),
        )?;
        store.kv_set("last_scan_at", &chrono::Utc::now().to_rfc3339())?;
    }
    Ok(ScanOutcome {
        updated,
        complete,
        diagnostics,
    })
}

struct Monitor {
    done: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Drop for Monitor {
    fn drop(&mut self) {
        self.done.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

pub fn run_read(
    engine: &Engine,
    id: &str,
    leased: bool,
    notify: &dyn Fn(&ReadCycle),
) -> Result<ReadCycle> {
    let mut busy = match engine.lifecycle.begin_evaluation() {
        Some(guard) => guard,
        None => {
            engine
                .store
                .lock()
                .unwrap()
                .finish_read(id, "busy", "A read or update is active")?;
            anyhow::bail!("busy: a read or update is active");
        }
    };
    let store = engine.store.lock().unwrap();
    let shared_lock = match lock(&store, "read") {
        Ok(lock) => lock,
        Err(e) => {
            store.finish_read(id, "busy", &e.to_string())?;
            return Err(e);
        }
    };
    busy.attach_lock(shared_lock);
    let cycle = store.read_cycle(Some(id))?.context("Read not found")?;
    if cycle.origin == "auto" && !store.auto_due(chrono::Utc::now(), false)? {
        store.finish_read(id, "skipped", "Recent work is already up to date")?;
        return Ok(store.read_cycle(Some(id))?.unwrap());
    }
    store.activate_read(id)?;
    let path = store.path.clone();
    drop(store);
    let cancel = busy.cancel_token();
    let done = Arc::new(AtomicBool::new(false));
    let done_thread = done.clone();
    let owned_id = id.to_string();
    let monitor = Monitor {
        done,
        thread: Some(std::thread::spawn(move || {
            let Ok(store) = Store::open(path) else {
                cancel.store(true, Ordering::SeqCst);
                return;
            };
            while !done_thread.load(Ordering::SeqCst) {
                if store.read_cancelled(&owned_id, leased).unwrap_or(true) {
                    cancel.store(true, Ordering::SeqCst);
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        })),
    };
    let result = evaluate(engine, id, &cycle.origin, &busy, notify);
    let cancelled = busy.cancelled();
    drop(monitor);
    let store = engine.store.lock().unwrap();
    match result {
        Ok(()) if !cancelled => store.finish_read(id, "completed", "Read complete")?,
        Ok(()) => store.finish_read(
            id,
            "cancelled",
            "Read cancelled; unfinished evidence remains pending",
        )?,
        Err(e) => store.finish_read(
            id,
            if cancelled { "cancelled" } else { "failed" },
            &e.to_string(),
        )?,
    }
    let cycle = store.read_cycle(Some(id))?.unwrap();
    notify(&cycle);
    Ok(cycle)
}

fn evaluate(
    engine: &Engine,
    id: &str,
    origin: &str,
    busy: &EvaluatorGuard,
    notify: &dyn Fn(&ReadCycle),
) -> Result<()> {
    anyhow::ensure!(!busy.cancelled(), "Read cancelled");
    let outcome = scan(engine)?;
    let store = engine.store.lock().unwrap();
    let end = today();
    let start = calendar::offset(&end, -(EVAL_WINDOW_DAYS - 1));
    let settings = store.settings();
    let mut days: BTreeMap<String, Vec<(SessionFacts, String)>> = BTreeMap::new();
    for (day, facts, hash) in store.pending_snapshots(&start, &end)? {
        if settings.excludes(&facts) {
            continue;
        }
        days.entry(day).or_default().push((facts, hash));
    }
    store.read_progress(id, days.len(), &outcome)?;
    notify(&store.read_cycle(Some(id))?.unwrap());
    drop(store);
    for (day, selected) in days {
        anyhow::ensure!(!busy.cancelled(), "Read cancelled");
        let versions: Vec<_> = selected
            .iter()
            .map(|(s, h)| (s.session_id.clone(), h.clone()))
            .collect();
        let sessions: Vec<_> = selected
            .into_iter()
            .map(|(s, _)| s)
            .filter(SessionFacts::has_substance)
            .collect();
        let mut run = EvalRun {
            id: new_id(),
            day: day.clone(),
            kind: origin.into(),
            started_at: chrono::Utc::now().to_rfc3339(),
            finished_at: None,
            status: "running".into(),
            model: None,
            cost_usd: None,
            num_turns: None,
            duration_ms: None,
            session_count: sessions.len() as i64,
            candidate_count: 0,
            error: None,
        };
        let candidates = if sessions.is_empty() {
            Vec::new()
        } else {
            engine.store.lock().unwrap().insert_cycle_run(id, &run)?;
            match evaluator::evaluate_day(&settings, busy, &day, &sessions) {
                Ok(result) => {
                    let candidates = pipeline::build_candidates(
                        &day,
                        &sessions,
                        result.achievements,
                        result.model.clone(),
                    );
                    run.status = "ok".into();
                    run.model = result.model;
                    run.error = result.note;
                    run.cost_usd = result.cost_usd;
                    run.num_turns = result.num_turns;
                    run.duration_ms = result.duration_ms;
                    run.candidate_count = candidates.len() as i64;
                    run.finished_at = Some(chrono::Utc::now().to_rfc3339());
                    candidates
                }
                Err(e) => {
                    run.status = "error".into();
                    run.error = Some(e.to_string());
                    run.finished_at = Some(chrono::Utc::now().to_rfc3339());
                    engine.store.lock().unwrap().insert_cycle_run(id, &run)?;
                    return Err(e);
                }
            }
        };
        anyhow::ensure!(!busy.cancelled(), "Read cancelled");
        let store = engine.store.lock().unwrap();
        store.replace_day(
            id,
            &day,
            &versions,
            &candidates,
            (!sessions.is_empty()).then_some(&run),
        )?;
        pipeline::link_related(&store)?;
        notify(&store.read_cycle(Some(id))?.unwrap());
    }
    anyhow::ensure!(
        outcome.complete,
        "Some evidence could not be scanned: {}",
        outcome.diagnostics.join("; ")
    );
    Ok(())
}

pub fn rewrite_merged(
    engine: &Engine,
    stitched: &Candidate,
    parts: &[(String, String)],
    notify: &dyn Fn(),
) {
    let Some(mut busy) = engine.lifecycle.begin_rewrite() else {
        return;
    };
    let settings = {
        let store = engine.store.lock().unwrap();
        let Ok(lock) = lock(&store, "read") else {
            return;
        };
        busy.attach_lock(lock);
        store.settings()
    };
    if let Ok((title, contribution)) = evaluator::rewrite_merged(&settings, &busy, parts) {
        let store = engine.store.lock().unwrap();
        let _ = store.edit_candidate(
            &stitched.id,
            Some(stitched.revision),
            &title,
            &contribution,
            &stitched.outcomes,
        );
        let _ = pipeline::link_related(&store);
    }
    notify();
}
