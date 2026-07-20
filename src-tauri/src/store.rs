use crate::models::*;
use anyhow::{anyhow, Result};
use rusqlite::{params, Connection};
use std::path::{Path, PathBuf};

pub struct Store {
    conn: Connection,
}

pub fn data_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("com.zreport.app")
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
             CREATE INDEX IF NOT EXISTS idx_journal_day ON journal(day);",
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

    pub fn upsert_session(&self, facts: &SessionFacts, day: &str, content_hash: &str) -> Result<()> {
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
                serde_json::to_string(facts)?,
                chrono::Local::now().to_rfc3339()
            ],
        )?;
        Ok(())
    }

    pub fn mark_sessions_evaluated(&self, ids: &[String]) -> Result<()> {
        for id in ids {
            self.conn.execute(
                "UPDATE sessions SET evaluated_hash=content_hash WHERE id=?1",
                params![id],
            )?;
        }
        Ok(())
    }

    /// Sessions whose content changed since last evaluation, grouped by day.
    pub fn pending_sessions(&self, up_to_day: &str, min_day: &str) -> Result<Vec<(String, SessionFacts)>> {
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
            "INSERT INTO candidates(id,day,title,contribution,outcomes,uncertainties,confidence,
               evidence_level,session_ids,repo,model,status,created_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
            params![
                c.id,
                c.day,
                c.title,
                c.contribution,
                serde_json::to_string(&c.outcomes)?,
                serde_json::to_string(&c.uncertainties)?,
                c.confidence,
                c.evidence_level,
                serde_json::to_string(&c.session_ids)?,
                c.repo,
                c.model,
                c.status,
                c.created_at
            ],
        )?;
        Ok(())
    }

    fn row_to_candidate(r: &rusqlite::Row) -> rusqlite::Result<Candidate> {
        Ok(Candidate {
            id: r.get(0)?,
            day: r.get(1)?,
            title: r.get(2)?,
            contribution: r.get(3)?,
            outcomes: serde_json::from_str(&r.get::<_, String>(4)?).unwrap_or_default(),
            uncertainties: serde_json::from_str(&r.get::<_, String>(5)?).unwrap_or_default(),
            confidence: r.get(6)?,
            evidence_level: r.get::<_, i64>(7)? as u8,
            session_ids: serde_json::from_str(&r.get::<_, String>(8)?).unwrap_or_default(),
            repo: r.get(9)?,
            model: r.get(10)?,
            status: r.get(11)?,
            created_at: r.get(12)?,
        })
    }

    const CANDIDATE_COLS: &'static str = "id,day,title,contribution,outcomes,uncertainties,confidence,evidence_level,session_ids,repo,model,status,created_at";

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

    pub fn set_candidate_status(&self, id: &str, status: &str, merged_into: Option<&str>) -> Result<()> {
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

    pub fn insert_journal(&self, e: &JournalEntry) -> Result<()> {
        self.conn.execute(
            "INSERT INTO journal(id,day,title,contribution,outcomes,evidence_level,session_ids,repo,model,approved_at,edited)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
            params![
                e.id,
                e.day,
                e.title,
                e.contribution,
                serde_json::to_string(&e.outcomes)?,
                e.evidence_level,
                serde_json::to_string(&e.session_ids)?,
                e.repo,
                e.model,
                e.approved_at,
                e.edited as i64
            ],
        )?;
        Ok(())
    }

    pub fn journal_range(&self, from: &str, to: &str, query: Option<&str>) -> Result<Vec<JournalEntry>> {
        let mut sql = String::from(
            "SELECT id,day,title,contribution,outcomes,evidence_level,session_ids,repo,model,approved_at,edited
             FROM journal WHERE day>=?1 AND day<=?2",
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
                title: r.get(2)?,
                contribution: r.get(3)?,
                outcomes: serde_json::from_str(&r.get::<_, String>(4)?).unwrap_or_default(),
                evidence_level: r.get::<_, i64>(5)? as u8,
                session_ids: serde_json::from_str(&r.get::<_, String>(6)?).unwrap_or_default(),
                repo: r.get(7)?,
                model: r.get(8)?,
                approved_at: r.get(9)?,
                edited: r.get::<_, i64>(10)? != 0,
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

    pub fn counts(&self) -> Result<(i64, i64, i64)> {
        let pending: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM candidates WHERE status='pending'",
            [],
            |r| r.get(0),
        )?;
        let journal: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM journal", [], |r| r.get(0))?;
        let sessions: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))?;
        Ok((pending, journal, sessions))
    }

    pub fn prune_older_than(&self, min_day: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM sessions WHERE day < ?1",
            params![min_day],
        )?;
        self.conn.execute(
            "DELETE FROM candidates WHERE day < ?1 AND status != 'approved'",
            params![min_day],
        )?;
        Ok(())
    }

    pub fn wipe_all(&self) -> Result<()> {
        self.conn.execute_batch(
            "DELETE FROM sessions; DELETE FROM candidates; DELETE FROM journal;
             DELETE FROM eval_runs; DELETE FROM kv;",
        )?;
        Ok(())
    }
}
