use crate::models::*;
use anyhow::{anyhow, Result};
use rusqlite::{params, Connection};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub struct Store {
    conn: Connection,
}

pub fn data_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("com.zreport.app")
}

fn add_column_if_missing(conn: &Connection, table: &str, column: &str, decl: &str) -> Result<()> {
    let exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info(?1) WHERE name=?2)",
        params![table, column],
        |r| r.get(0),
    )?;
    if !exists {
        conn.execute(
            &format!("ALTER TABLE {table} ADD COLUMN {column} {decl}"),
            [],
        )?;
    }
    Ok(())
}

impl Store {
    pub fn open_default() -> Result<Self> {
        let dir = data_dir();
        std::fs::create_dir_all(&dir)?;
        Self::open(dir.join("zreport.db"))
    }

    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA foreign_keys=ON;
             CREATE TABLE IF NOT EXISTS sessions (
               id TEXT PRIMARY KEY,
               file_path TEXT NOT NULL,
               day TEXT,
               content_hash TEXT NOT NULL DEFAULT '',
               evaluated_hash TEXT NOT NULL DEFAULT '',
               facts TEXT NOT NULL DEFAULT '{}',
               updated_at TEXT
             );
             CREATE TABLE IF NOT EXISTS candidates (
               id TEXT PRIMARY KEY,
               day TEXT NOT NULL,
               title TEXT NOT NULL,
               contribution TEXT NOT NULL,
               outcomes TEXT NOT NULL DEFAULT '[]',
               uncertainties TEXT NOT NULL DEFAULT '[]',
               confidence REAL NOT NULL DEFAULT 0,
               evidence_level INTEGER NOT NULL DEFAULT 1,
               session_ids TEXT NOT NULL DEFAULT '[]',
               pr_links TEXT NOT NULL DEFAULT '[]',
               repo TEXT,
               model TEXT,
               status TEXT NOT NULL DEFAULT 'pending',
               merged_into TEXT,
               created_at TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS journal (
               id TEXT PRIMARY KEY,
               day TEXT NOT NULL,
               title TEXT NOT NULL,
               contribution TEXT NOT NULL,
               outcomes TEXT NOT NULL DEFAULT '[]',
               evidence_level INTEGER NOT NULL DEFAULT 1,
               session_ids TEXT NOT NULL DEFAULT '[]',
               pr_links TEXT NOT NULL DEFAULT '[]',
               repo TEXT,
               model TEXT,
               approved_at TEXT NOT NULL,
               edited INTEGER NOT NULL DEFAULT 0
             );
             CREATE TABLE IF NOT EXISTS eval_runs (
               id TEXT PRIMARY KEY,
               day TEXT NOT NULL,
               kind TEXT NOT NULL,
               started_at TEXT NOT NULL,
               finished_at TEXT,
               status TEXT NOT NULL,
               model TEXT,
               cost_usd REAL,
               num_turns INTEGER,
               duration_ms INTEGER,
               session_count INTEGER NOT NULL DEFAULT 0,
               candidate_count INTEGER NOT NULL DEFAULT 0,
               error TEXT
             );
             CREATE TABLE IF NOT EXISTS kv (
               key TEXT PRIMARY KEY,
               value TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_candidates_day ON candidates(day);
             CREATE INDEX IF NOT EXISTS idx_journal_day ON journal(day);
             CREATE INDEX IF NOT EXISTS idx_sessions_day ON sessions(day);",
        )?;
        for table in ["candidates", "journal"] {
            add_column_if_missing(&conn, table, "pr_links", "TEXT NOT NULL DEFAULT '[]'")?;
            add_column_if_missing(&conn, table, "day_end", "TEXT")?;
        }
        add_column_if_missing(&conn, "candidates", "related", "TEXT")?;
        let candidate_index_columns: i64 = conn.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('candidates')
             WHERE name IN ('status', 'day', 'created_at')",
            [],
            |row| row.get(0),
        )?;
        if candidate_index_columns == 3 {
            conn.execute(
                "CREATE INDEX IF NOT EXISTS idx_candidates_status_day_created
                 ON candidates(status, day DESC, created_at DESC)",
                [],
            )?;
        }
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS dismissed_links (
               pair_key TEXT PRIMARY KEY,
               dismissed_at TEXT NOT NULL
             );",
        )?;
        Ok(Self { conn })
    }

    pub fn kv_get(&self, key: &str) -> Option<String> {
        self.conn
            .query_row("SELECT value FROM kv WHERE key=?1", params![key], |r| {
                r.get(0)
            })
            .ok()
    }

    pub fn kv_set(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO kv(key,value) VALUES(?1,?2)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn settings(&self) -> Settings {
        self.kv_get("settings")
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save_settings(&self, s: &Settings) -> Result<()> {
        self.kv_set("settings", &serde_json::to_string(s)?)
    }

    pub fn session_hash(&self, id: &str) -> Option<(String, String)> {
        self.conn
            .query_row(
                "SELECT content_hash, evaluated_hash FROM sessions WHERE id=?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .ok()
    }

    pub fn session_content_hashes(&self) -> Result<HashMap<String, String>> {
        let mut stmt = self.conn.prepare("SELECT id, content_hash FROM sessions")?;
        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect::<rusqlite::Result<_>>().map_err(Into::into)
    }

    pub fn upsert_session(
        &self,
        facts: &SessionFacts,
        day: &str,
        content_hash: &str,
    ) -> Result<()> {
        self.upsert_session_json(facts, day, content_hash, &serde_json::to_string(facts)?)
    }

    fn upsert_session_json(
        &self,
        facts: &SessionFacts,
        day: &str,
        content_hash: &str,
        json: &str,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO sessions(id, file_path, day, content_hash, facts, updated_at)
             VALUES(?1,?2,?3,?4,?5,?6)
             ON CONFLICT(id) DO UPDATE SET
               file_path=excluded.file_path, day=excluded.day,
               content_hash=excluded.content_hash, facts=excluded.facts,
               updated_at=excluded.updated_at",
            params![
                facts.session_id,
                facts.file_path,
                day,
                content_hash,
                json,
                chrono::Local::now().to_rfc3339()
            ],
        )?;
        Ok(())
    }

    pub fn upsert_session_if_changed(
        &self,
        facts: &SessionFacts,
        day: &str,
        content_hash: &str,
    ) -> Result<bool> {
        let json = serde_json::to_string(facts)?;
        let prior: Option<String> = self
            .conn
            .query_row(
                "SELECT facts FROM sessions WHERE id=?1",
                params![facts.session_id],
                |r| r.get(0),
            )
            .ok();
        if prior.as_deref() == Some(json.as_str()) {
            // CASE compares pre-update hashes so metadata-only changes preserve queue state.
            self.conn.execute(
                "UPDATE sessions SET content_hash=?2, updated_at=?3,
                   evaluated_hash=CASE WHEN evaluated_hash=content_hash THEN ?2 ELSE evaluated_hash END
                 WHERE id=?1",
                params![
                    facts.session_id,
                    content_hash,
                    chrono::Local::now().to_rfc3339()
                ],
            )?;
            return Ok(false);
        }
        self.upsert_session_json(facts, day, content_hash, &json)?;
        Ok(true)
    }

    pub fn mark_sessions_evaluated(&self, ids: &[String]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let placeholders = vec!["?"; ids.len()].join(",");
        let sql =
            format!("UPDATE sessions SET evaluated_hash=content_hash WHERE id IN ({placeholders})");
        self.conn
            .execute(&sql, rusqlite::params_from_iter(ids.iter()))?;
        Ok(())
    }

    pub fn pending_sessions(
        &self,
        up_to_day: &str,
        min_day: &str,
    ) -> Result<Vec<(String, SessionFacts)>> {
        let mut stmt = self.conn.prepare(
            "SELECT day, facts FROM sessions
             WHERE content_hash != evaluated_hash AND day <= ?1 AND day >= ?2
             ORDER BY day ASC",
        )?;
        let rows = stmt.query_map(params![up_to_day, min_day], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (day, facts) = row?;
            if let Ok(f) = serde_json::from_str::<SessionFacts>(&facts) {
                out.push((day, f));
            }
        }
        Ok(out)
    }

    pub fn insert_candidate(&self, c: &Candidate) -> Result<()> {
        self.conn.execute(
            "INSERT INTO candidates(id,day,day_end,title,contribution,outcomes,uncertainties,confidence,
               evidence_level,session_ids,pr_links,repo,model,status,related,created_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)",
            params![
                c.id,
                c.day,
                c.day_end,
                c.title,
                c.contribution,
                serde_json::to_string(&c.outcomes)?,
                serde_json::to_string(&c.uncertainties)?,
                c.confidence,
                c.evidence_level,
                serde_json::to_string(&c.session_ids)?,
                serde_json::to_string(&c.pr_links)?,
                c.repo,
                c.model,
                c.status,
                c.related.as_ref().map(serde_json::to_string).transpose()?,
                c.created_at
            ],
        )?;
        Ok(())
    }

    fn row_to_candidate(r: &rusqlite::Row) -> rusqlite::Result<Candidate> {
        Ok(Candidate {
            id: r.get(0)?,
            day: r.get(1)?,
            day_end: r.get(2)?,
            title: r.get(3)?,
            contribution: r.get(4)?,
            outcomes: serde_json::from_str(&r.get::<_, String>(5)?).unwrap_or_default(),
            uncertainties: serde_json::from_str(&r.get::<_, String>(6)?).unwrap_or_default(),
            confidence: r.get(7)?,
            evidence_level: r.get::<_, i64>(8)? as u8,
            session_ids: serde_json::from_str(&r.get::<_, String>(9)?).unwrap_or_default(),
            pr_links: serde_json::from_str(&r.get::<_, String>(10)?).unwrap_or_default(),
            repo: r.get(11)?,
            model: r.get(12)?,
            status: r.get(13)?,
            related: r
                .get::<_, Option<String>>(14)?
                .and_then(|s| serde_json::from_str(&s).ok()),
            created_at: r.get(15)?,
        })
    }

    const CANDIDATE_COLS: &'static str = "id,day,day_end,title,contribution,outcomes,uncertainties,confidence,evidence_level,session_ids,pr_links,repo,model,status,related,created_at";

    pub fn candidates_by_status(&self, status: &str) -> Result<Vec<Candidate>> {
        let sql = format!(
            "SELECT {} FROM candidates WHERE status=?1 ORDER BY day DESC, created_at DESC",
            Self::CANDIDATE_COLS
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params![status], Self::row_to_candidate)?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    pub fn candidate(&self, id: &str) -> Result<Candidate> {
        let sql = format!(
            "SELECT {} FROM candidates WHERE id=?1",
            Self::CANDIDATE_COLS
        );
        self.conn
            .query_row(&sql, params![id], Self::row_to_candidate)
            .map_err(|e| anyhow!("candidate not found: {e}"))
    }

    pub fn update_candidate_fields(
        &self,
        id: &str,
        title: &str,
        contribution: &str,
        outcomes: &[Outcome],
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE candidates SET title=?2, contribution=?3, outcomes=?4 WHERE id=?1",
            params![id, title, contribution, serde_json::to_string(outcomes)?],
        )?;
        Ok(())
    }

    pub fn set_candidate_status(
        &self,
        id: &str,
        status: &str,
        merged_into: Option<&str>,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE candidates SET status=?2, merged_into=?3 WHERE id=?1",
            params![id, status, merged_into],
        )?;
        Ok(())
    }

    pub fn delete_pending_for_sessions(&self, day: &str, session_ids: &[String]) -> Result<()> {
        let sql = format!(
            "SELECT {} FROM candidates WHERE day=?1 AND status='pending'",
            Self::CANDIDATE_COLS
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows: Vec<Candidate> = stmt
            .query_map(params![day], Self::row_to_candidate)?
            .filter_map(|r| r.ok())
            .collect();
        for c in rows {
            if c.session_ids.iter().any(|s| session_ids.contains(s)) {
                self.conn
                    .execute("DELETE FROM candidates WHERE id=?1", params![c.id])?;
            }
        }
        Ok(())
    }

    pub fn set_candidate_related(&self, id: &str, link: Option<&RelatedLink>) -> Result<()> {
        self.conn.execute(
            "UPDATE candidates SET related=?2 WHERE id=?1",
            params![id, link.map(serde_json::to_string).transpose()?],
        )?;
        Ok(())
    }

    pub fn sessions_by_ids(&self, ids: &[String]) -> Result<Vec<SessionFacts>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = vec!["?"; ids.len()].join(",");
        let sql = format!("SELECT facts FROM sessions WHERE id IN ({placeholders})");
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(ids.iter()), |r| {
            r.get::<_, String>(0)
        })?;
        Ok(rows
            .filter_map(|r| r.ok())
            .filter_map(|f| serde_json::from_str(&f).ok())
            .collect())
    }

    pub fn journal_since(&self, min_day: &str) -> Result<Vec<JournalEntry>> {
        self.journal_range(min_day, "9999-12-31", None)
    }

    pub fn dismiss_link(&self, pair_key: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO dismissed_links(pair_key, dismissed_at) VALUES(?1,?2)
             ON CONFLICT(pair_key) DO NOTHING",
            params![pair_key, chrono::Local::now().to_rfc3339()],
        )?;
        Ok(())
    }

    pub fn dismissed_links(&self) -> Result<std::collections::HashSet<String>> {
        let mut stmt = self.conn.prepare("SELECT pair_key FROM dismissed_links")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    pub fn insert_journal(&self, e: &JournalEntry) -> Result<()> {
        self.conn.execute(
            "INSERT INTO journal(id,day,day_end,title,contribution,outcomes,evidence_level,session_ids,pr_links,repo,model,approved_at,edited)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
            params![
                e.id,
                e.day,
                e.day_end,
                e.title,
                e.contribution,
                serde_json::to_string(&e.outcomes)?,
                e.evidence_level,
                serde_json::to_string(&e.session_ids)?,
                serde_json::to_string(&e.pr_links)?,
                e.repo,
                e.model,
                e.approved_at,
                e.edited as i64
            ],
        )?;
        Ok(())
    }

    pub fn journal_range(
        &self,
        from: &str,
        to: &str,
        query: Option<&str>,
    ) -> Result<Vec<JournalEntry>> {
        let mut sql = String::from(
            "SELECT id,day,day_end,title,contribution,outcomes,evidence_level,session_ids,pr_links,repo,model,approved_at,edited
             FROM journal WHERE day<=?2 AND coalesce(day_end,day)>=?1",
        );
        if query.is_some() {
            sql.push_str(" AND (title LIKE ?3 OR contribution LIKE ?3)");
        }
        sql.push_str(" ORDER BY day DESC, approved_at DESC");
        let mut stmt = self.conn.prepare(&sql)?;
        let map = |r: &rusqlite::Row| -> rusqlite::Result<JournalEntry> {
            Ok(JournalEntry {
                id: r.get(0)?,
                day: r.get(1)?,
                day_end: r.get(2)?,
                title: r.get(3)?,
                contribution: r.get(4)?,
                outcomes: serde_json::from_str(&r.get::<_, String>(5)?).unwrap_or_default(),
                evidence_level: r.get::<_, i64>(6)? as u8,
                session_ids: serde_json::from_str(&r.get::<_, String>(7)?).unwrap_or_default(),
                pr_links: serde_json::from_str(&r.get::<_, String>(8)?).unwrap_or_default(),
                repo: r.get(9)?,
                model: r.get(10)?,
                approved_at: r.get(11)?,
                edited: r.get::<_, i64>(12)? != 0,
            })
        };
        let rows: Vec<JournalEntry> = if let Some(q) = query {
            let like = format!("%{}%", q);
            stmt.query_map(params![from, to, like], map)?
                .filter_map(|r| r.ok())
                .collect()
        } else {
            stmt.query_map(params![from, to], map)?
                .filter_map(|r| r.ok())
                .collect()
        };
        Ok(rows)
    }

    pub fn confirm_impact(&self, id: &str, note: &str) -> Result<()> {
        let outcomes_json: String = self.conn.query_row(
            "SELECT outcomes FROM journal WHERE id=?1",
            params![id],
            |r| r.get(0),
        )?;
        let mut outcomes: Vec<Outcome> = serde_json::from_str(&outcomes_json).unwrap_or_default();
        outcomes.push(Outcome {
            claim: note.to_string(),
            evidence_level: 5,
            evidence_refs: vec!["user:confirmed".into()],
            verified: true,
        });
        self.conn.execute(
            "UPDATE journal SET outcomes=?2, evidence_level=5 WHERE id=?1",
            params![id, serde_json::to_string(&outcomes)?],
        )?;
        Ok(())
    }

    pub fn delete_journal_entry(&self, id: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM journal WHERE id=?1", params![id])?;
        Ok(())
    }

    pub fn insert_eval_run(&self, run: &EvalRun) -> Result<()> {
        self.conn.execute(
            "INSERT INTO eval_runs(id,day,kind,started_at,finished_at,status,model,cost_usd,num_turns,duration_ms,session_count,candidate_count,error)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)
             ON CONFLICT(id) DO UPDATE SET finished_at=excluded.finished_at, status=excluded.status,
               model=excluded.model, cost_usd=excluded.cost_usd, num_turns=excluded.num_turns,
               duration_ms=excluded.duration_ms, session_count=excluded.session_count,
               candidate_count=excluded.candidate_count, error=excluded.error",
            params![
                run.id, run.day, run.kind, run.started_at, run.finished_at, run.status,
                run.model, run.cost_usd, run.num_turns, run.duration_ms,
                run.session_count, run.candidate_count, run.error
            ],
        )?;
        Ok(())
    }

    pub fn recent_eval_runs(&self, limit: u32) -> Result<Vec<EvalRun>> {
        let mut stmt = self.conn.prepare(
            "SELECT id,day,kind,started_at,finished_at,status,model,cost_usd,num_turns,duration_ms,session_count,candidate_count,error
             FROM eval_runs ORDER BY started_at DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit], |r| {
            Ok(EvalRun {
                id: r.get(0)?,
                day: r.get(1)?,
                kind: r.get(2)?,
                started_at: r.get(3)?,
                finished_at: r.get(4)?,
                status: r.get(5)?,
                model: r.get(6)?,
                cost_usd: r.get(7)?,
                num_turns: r.get(8)?,
                duration_ms: r.get(9)?,
                session_count: r.get(10)?,
                candidate_count: r.get(11)?,
                error: r.get(12)?,
            })
        })?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    pub fn overview_counts(&self) -> Result<(i64, i64)> {
        let pending: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM candidates WHERE status='pending'",
            [],
            |r| r.get(0),
        )?;
        let sessions: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))?;
        Ok((pending, sessions))
    }

    pub fn prune_candidates_older_than(&self, min_day: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM candidates WHERE day < ?1 AND status != 'approved'",
            params![min_day],
        )?;
        Ok(())
    }

    // Call only with the same horizon skipped by scanning, or old transcripts will churn.
    pub fn prune_sessions_older_than(&self, min_day: &str) -> Result<usize> {
        Ok(self
            .conn
            .execute("DELETE FROM sessions WHERE day < ?1", params![min_day])?)
    }

    pub fn delete_sessions_missing_from(&self, live_ids: &[String]) -> Result<usize> {
        // Empty discovery is ambiguous and must never erase all stored evidence.
        if live_ids.is_empty() {
            return Ok(0);
        }
        let placeholders = vec!["?"; live_ids.len()].join(",");
        let sql = format!("DELETE FROM sessions WHERE id NOT IN ({placeholders})");
        Ok(self
            .conn
            .execute(&sql, rusqlite::params_from_iter(live_ids.iter()))?)
    }

    pub fn wipe_all(&self) -> Result<()> {
        self.conn.execute_batch(
            "DELETE FROM sessions; DELETE FROM candidates; DELETE FROM journal;
             DELETE FROM eval_runs; DELETE FROM kv; DELETE FROM dismissed_links;",
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(id: &str, day: &str, status: &str) -> Candidate {
        Candidate {
            id: id.into(),
            day: day.into(),
            day_end: None,
            title: "t".into(),
            contribution: "c".into(),
            outcomes: vec![],
            uncertainties: vec![],
            confidence: 0.5,
            evidence_level: 1,
            session_ids: vec!["s1".into()],
            pr_links: vec![],
            repo: None,
            model: None,
            status: status.into(),
            related: None,
            created_at: "2026-06-01T09:00:00+02:00".into(),
        }
    }

    #[test]
    fn retention_prunes_stale_candidates_and_spares_evidence() {
        let store = Store::open(":memory:").unwrap();
        let facts = SessionFacts {
            session_id: "s1".into(),
            ..Default::default()
        };
        store.upsert_session(&facts, "2026-06-01", "h1").unwrap();
        store
            .insert_candidate(&candidate("c-stale", "2026-06-01", "pending"))
            .unwrap();
        store
            .insert_candidate(&candidate("c-approved", "2026-06-01", "approved"))
            .unwrap();
        store
            .insert_candidate(&candidate("c-recent", "2026-07-20", "pending"))
            .unwrap();

        store.prune_candidates_older_than("2026-07-03").unwrap();

        assert!(store.session_hash("s1").is_some());

        let pending: Vec<String> = store
            .candidates_by_status("pending")
            .unwrap()
            .into_iter()
            .map(|c| c.id)
            .collect();
        assert_eq!(pending, vec!["c-recent".to_string()]);
        assert_eq!(store.candidates_by_status("approved").unwrap().len(), 1);
    }

    #[test]
    fn candidate_pr_links_round_trip() {
        let store = Store::open(":memory:").unwrap();
        let mut value = candidate("c-pr", "2026-07-20", "pending");
        value.pr_links.push(PrLink {
            number: 5159,
            url: "https://github.com/acme/widgets/pull/5159".into(),
            repository: "acme/widgets".into(),
            ts: Some("2026-07-20T10:05:00Z".into()),
        });

        store.insert_candidate(&value).unwrap();
        let loaded = store.candidate("c-pr").unwrap();

        assert_eq!(loaded.pr_links, value.pr_links);
    }

    #[test]
    fn journal_pr_links_survive_approval() {
        let store = Store::open(":memory:").unwrap();
        let pr = PrLink {
            number: 5159,
            url: "https://github.com/acme/widgets/pull/5159".into(),
            repository: "acme/widgets".into(),
            ts: Some("2026-07-20T10:05:00Z".into()),
        };
        let entry = JournalEntry {
            id: "j-pr".into(),
            day: "2026-07-20".into(),
            day_end: None,
            title: "t".into(),
            contribution: "c".into(),
            outcomes: vec![],
            evidence_level: 4,
            session_ids: vec!["s1".into()],
            pr_links: vec![pr.clone()],
            repo: None,
            model: None,
            approved_at: "2026-07-20T18:05:00+02:00".into(),
            edited: false,
        };

        store.insert_journal(&entry).unwrap();
        let loaded = store
            .journal_range("2026-07-20", "2026-07-20", None)
            .unwrap();

        assert_eq!(loaded[0].pr_links, vec![pr]);
    }

    #[test]
    fn a_spanning_entry_survives_a_range_that_clips_it() {
        let store = Store::open(":memory:").unwrap();
        let entry = |id: &str, day: &str, day_end: Option<&str>| JournalEntry {
            id: id.into(),
            day: day.into(),
            day_end: day_end.map(String::from),
            title: id.into(),
            contribution: "c".into(),
            outcomes: vec![],
            evidence_level: 2,
            session_ids: vec![],
            pr_links: vec![],
            repo: None,
            model: None,
            approved_at: "2026-07-20T18:00:00+02:00".into(),
            edited: false,
        };
        store
            .insert_journal(&entry("spans", "2026-07-19", Some("2026-07-20")))
            .unwrap();
        store
            .insert_journal(&entry("single", "2026-07-17", None))
            .unwrap();

        let ids = |from: &str, to: &str| -> Vec<String> {
            store
                .journal_range(from, to, None)
                .unwrap()
                .into_iter()
                .map(|e| e.id)
                .collect()
        };
        assert_eq!(
            ids("2026-07-20", "2026-07-26"),
            vec!["spans"],
            "clipped at the start"
        );
        assert_eq!(
            ids("2026-07-13", "2026-07-19"),
            vec!["spans", "single"],
            "clipped at the end"
        );
        assert!(ids("2026-07-21", "2026-07-26").is_empty());
    }

    #[test]
    fn related_links_round_trip_and_clear() {
        let store = Store::open(":memory:").unwrap();
        store
            .insert_candidate(&candidate("c1", "2026-07-20", "pending"))
            .unwrap();
        let link = RelatedLink {
            kind: RelatedKind::Continuation,
            target_id: "c0".into(),
            target_day: "2026-07-19".into(),
            target_title: "Earlier half".into(),
            pair_key: "s-a~s-b".into(),
        };

        store.set_candidate_related("c1", Some(&link)).unwrap();
        assert_eq!(store.candidate("c1").unwrap().related, Some(link));

        store.set_candidate_related("c1", None).unwrap();
        assert_eq!(store.candidate("c1").unwrap().related, None);
    }

    #[test]
    fn dismissals_are_idempotent() {
        let store = Store::open(":memory:").unwrap();
        store.dismiss_link("a~b").unwrap();
        store.dismiss_link("a~b").unwrap();
        assert_eq!(store.dismissed_links().unwrap().len(), 1);
    }

    #[test]
    fn adds_pr_links_column_to_existing_tables() {
        let path = std::env::temp_dir().join(format!(
            "zreport-legacy-{}-{}.db",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE candidates (
                    id TEXT PRIMARY KEY,
                    day TEXT NOT NULL
                 );
                 CREATE TABLE journal (
                    id TEXT PRIMARY KEY,
                    day TEXT NOT NULL
                 );",
            )
            .unwrap();
        }

        let store = Store::open(&path).unwrap();
        for (table, column) in [
            ("candidates", "pr_links"),
            ("candidates", "day_end"),
            ("candidates", "related"),
            ("journal", "pr_links"),
            ("journal", "day_end"),
        ] {
            let count: i64 = store
                .conn
                .query_row(
                    "SELECT COUNT(*) FROM pragma_table_info(?1) WHERE name=?2",
                    params![table, column],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(count, 1, "{table} missing {column}");
        }

        drop(store);
        std::fs::remove_file(path).unwrap();
    }

    fn session(id: &str) -> SessionFacts {
        SessionFacts {
            session_id: id.into(),
            ..Default::default()
        }
    }

    #[test]
    fn touching_a_transcript_does_not_re_queue_evaluated_work() {
        let store = Store::open(":memory:").unwrap();
        let window = || {
            store
                .pending_sessions("2026-07-31", "2026-07-01")
                .unwrap()
                .len()
        };
        let facts = session("s1");

        assert!(store
            .upsert_session_if_changed(&facts, "2026-07-20", "100:1")
            .unwrap());
        store.mark_sessions_evaluated(&["s1".to_string()]).unwrap();
        assert_eq!(window(), 0);

        assert!(!store
            .upsert_session_if_changed(&facts, "2026-07-20", "100:2")
            .unwrap());
        assert_eq!(window(), 0, "an mtime bump alone must not re-queue");

        let mut resumed = facts.clone();
        resumed.prompts.push("and one more thing".into());
        assert!(store
            .upsert_session_if_changed(&resumed, "2026-07-20", "200:3")
            .unwrap());
        assert_eq!(window(), 1, "genuinely new work must re-queue");
    }

    #[test]
    fn touching_an_unevaluated_transcript_leaves_it_queued() {
        let store = Store::open(":memory:").unwrap();
        let facts = session("s1");
        store
            .upsert_session_if_changed(&facts, "2026-07-20", "100:1")
            .unwrap();

        assert!(!store
            .upsert_session_if_changed(&facts, "2026-07-20", "100:2")
            .unwrap());

        let pending = store.pending_sessions("2026-07-31", "2026-07-01").unwrap();
        assert_eq!(pending.len(), 1, "never evaluated, so it stays pending");
    }

    #[test]
    fn evidence_horizon_prunes_stale_sessions() {
        let store = Store::open(":memory:").unwrap();
        store
            .upsert_session(&session("old"), "2026-01-01", "h")
            .unwrap();
        store
            .upsert_session(&session("recent"), "2026-07-20", "h")
            .unwrap();

        assert_eq!(store.prune_sessions_older_than("2026-04-01").unwrap(), 1);
        assert!(store.session_hash("old").is_none());
        assert!(store.session_hash("recent").is_some());
    }

    #[test]
    fn orphan_cleanup_spares_everything_when_discovery_returns_nothing() {
        let store = Store::open(":memory:").unwrap();
        store
            .upsert_session(&session("live"), "2026-07-20", "h")
            .unwrap();
        store
            .upsert_session(&session("orphan"), "2026-07-21", "h")
            .unwrap();

        assert_eq!(store.delete_sessions_missing_from(&[]).unwrap(), 0);
        assert!(store.session_hash("live").is_some());
        assert!(store.session_hash("orphan").is_some());

        let live = vec!["live".to_string()];
        assert_eq!(store.delete_sessions_missing_from(&live).unwrap(), 1);
        assert!(store.session_hash("live").is_some());
        assert!(store.session_hash("orphan").is_none());
    }
}
