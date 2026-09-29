use super::*;
use crate::pipeline;

impl Store {
    pub(crate) fn transaction<T>(&self, f: impl FnOnce() -> Result<T>) -> Result<T> {
        let tx = rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        let value = f()?;
        tx.commit()?;
        Ok(value)
    }

    fn check_revision(candidate: &Candidate, expected: Option<i64>) -> Result<()> {
        anyhow::ensure!(
            expected.is_none_or(|r| r == candidate.revision),
            "conflict: this achievement changed; refresh and try again"
        );
        Ok(())
    }

    pub fn edit_candidate(
        &self,
        id: &str,
        revision: Option<i64>,
        title: &str,
        contribution: &str,
        outcomes: &[Outcome],
    ) -> Result<()> {
        anyhow::ensure!(
            !title.trim().is_empty()
                && title.len() <= 500
                && contribution.len() <= 20_000
                && outcomes.len() <= 100,
            "Invalid achievement text"
        );
        self.transaction(|| {
            let current = self.candidate(id)?;
            Self::check_revision(&current, revision)?;
            anyhow::ensure!(current.status == "pending", "conflict: only pending achievements can be edited");
            let outcomes: Vec<Outcome> = outcomes.iter().map(|outcome| {
                let mut value = outcome.clone();
                if !current.outcomes.iter().any(|old| old.claim == value.claim && old.evidence_refs == value.evidence_refs && old.evidence_level == value.evidence_level) {
                    value.verified = false;
                    value.evidence_level = 1;
                } else {
                    value.verified = current.outcomes.iter().any(|old| old.claim == value.claim && old.evidence_refs == value.evidence_refs && old.verified);
                }
                value
            }).collect();
            self.conn.execute("UPDATE candidates SET title=?2, contribution=?3, outcomes=?4, evidence_level=?5, protected=1, revision=revision+1 WHERE id=?1",
                params![id, title.trim(), contribution.trim(), serde_json::to_string(&outcomes)?, outcomes.iter().map(|o| o.evidence_level).max().unwrap_or(1)])?;
            Ok(())
        })
    }

    pub fn approve(&self, id: &str, revision: Option<i64>, edited: bool) -> Result<String> {
        self.transaction(|| {
            let c = self.candidate(id)?;
            if c.status == "approved" {
                return Ok(format!("j-{id}"));
            }
            Self::check_revision(&c, revision)?;
            anyhow::ensure!(
                c.status == "pending",
                "conflict: only pending achievements can be approved"
            );
            let entry = JournalEntry::from_candidate(
                &c,
                chrono::Utc::now().to_rfc3339(),
                edited || c.revision > 0,
            );
            self.insert_journal(&entry)?;
            self.set_candidate_status(id, "approved", None)?;
            Ok(entry.id)
        })
    }

    pub fn transition(&self, id: &str, revision: Option<i64>, status: &str) -> Result<()> {
        self.transaction(|| {
            let c = self.candidate(id)?;
            if c.status == status {
                return Ok(());
            }
            Self::check_revision(&c, revision)?;
            anyhow::ensure!(
                matches!(
                    (c.status.as_str(), status),
                    ("pending", "discarded") | ("discarded", "pending")
                ),
                "conflict: achievement cannot make that transition"
            );
            self.set_candidate_status(id, status, None)
        })
    }

    pub fn merge(&self, ids: &[String], revisions: Option<&[i64]>) -> Result<Candidate> {
        let distinct: std::collections::HashSet<_> = ids.iter().collect();
        anyhow::ensure!(
            ids.len() >= 2 && ids.len() <= 100 && distinct.len() == ids.len(),
            "Select at least two different achievements"
        );
        anyhow::ensure!(
            revisions.is_none_or(|r| r.len() == ids.len()),
            "Invalid merge revisions"
        );
        self.transaction(|| {
            let parts = ids
                .iter()
                .enumerate()
                .map(|(i, id)| {
                    let candidate = self.candidate(id)?;
                    Self::check_revision(&candidate, revisions.map(|r| r[i]))?;
                    Ok(candidate)
                })
                .collect::<Result<Vec<_>>>()?;
            anyhow::ensure!(
                parts.iter().all(|c| c.status == "pending"),
                "conflict: only pending achievements can be merged"
            );
            let mut merged = pipeline::merge_into_one(&parts);
            merged.id = crate::engine::new_id();
            self.insert_candidate(&merged)?;
            self.conn.execute(
                "UPDATE candidates SET protected=1 WHERE id=?1",
                [&merged.id],
            )?;
            for id in ids {
                self.set_candidate_status(id, "merged", Some(&merged.id))?;
            }
            Ok(merged)
        })
    }

    pub fn insert_cycle_run(&self, cycle: &str, run: &EvalRun) -> Result<()> {
        self.transaction(|| self.write_cycle_run(cycle, run))
    }

    fn write_cycle_run(&self, cycle: &str, run: &EvalRun) -> Result<()> {
        self.insert_eval_run(run)?;
        self.conn.execute(
            "UPDATE eval_runs SET cycle_id=?2 WHERE id=?1",
            params![run.id, cycle],
        )?;
        Ok(())
    }

    pub fn replace_day(
        &self,
        cycle: &str,
        day: &str,
        versions: &[(String, String)],
        candidates: &[Candidate],
        run: Option<&EvalRun>,
    ) -> Result<()> {
        self.transaction(|| {
            self.delete_pending_for_sessions(day, &versions.iter().map(|(id,_)| id.clone()).collect::<Vec<_>>())?;
            for c in candidates { self.insert_candidate(c)?; }
            self.mark_snapshots(versions)?;
            let mut evidence = self.conn.prepare("INSERT OR REPLACE INTO read_evidence(cycle_id, session_id, content_hash) VALUES(?1,?2,?3)")?;
            for (id, hash) in versions {
                evidence.execute(params![cycle,id,hash])?;
            }
            if let Some(run) = run {
                self.write_cycle_run(cycle, run)?;
            }
            self.conn.execute("UPDATE read_cycles SET completed_days=completed_days+1, candidate_count=candidate_count+?2, message=?3 WHERE id=?1",
                params![cycle, candidates.len() as i64, format!("Evaluated {day}")])?;
            Ok(())
        })
    }

    pub fn mark_snapshots(&self, versions: &[(String, String)]) -> Result<()> {
        let mut update = self
            .conn
            .prepare("UPDATE sessions SET evaluated_hash=?2 WHERE id=?1")?;
        for (id, hash) in versions {
            update.execute(params![id, hash])?;
        }
        Ok(())
    }

    pub fn pending_snapshots(
        &self,
        from: &str,
        to: &str,
    ) -> Result<Vec<(String, SessionFacts, String)>> {
        let mut stmt = self.conn.prepare("SELECT day,facts,content_hash FROM sessions WHERE day>=?1 AND day<=?2 AND content_hash<>evaluated_hash ORDER BY day,id")?;
        let rows = stmt.query_map(params![from, to], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        rows.map(|r| {
            let (day, json, hash) = r?;
            Ok((day, serde_json::from_str(&json)?, hash))
        })
        .collect()
    }
}
