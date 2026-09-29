use super::*;
use crate::engine::{ReadCycle, ScanOutcome};

impl Store {
    pub fn recover_reads(&self) -> Result<()> {
        let Ok(_lock) = crate::engine::lock(self, "read") else {
            return Ok(());
        };
        self.transaction(|| {
            let now=chrono::Utc::now();
            let recovered=self.conn.execute("UPDATE read_cycles SET status='interrupted',finished_at=?1,message='Previous owner stopped; unfinished evidence remains pending' WHERE status='running' OR (status='queued' AND started_at<?2)",params![now.to_rfc3339(),(now-chrono::Duration::seconds(10)).to_rfc3339()])?;
            if recovered > 0 {
                self.kv_set("retry_after", &(now+chrono::Duration::minutes(30)).to_rfc3339())?;
                self.conn.execute("UPDATE eval_runs SET status='error',finished_at=?1,error='Read interrupted' WHERE status='running'", [now.to_rfc3339()])?;
            }
            Ok(())
        })
    }

    pub fn queue_read(&self, id: &str, origin: &str, owner: &str, now: &str) -> Result<()> {
        self.conn.execute("INSERT INTO read_cycles(id,origin,status,owner,started_at,heartbeat_at) VALUES(?1,?2,'queued',?3,?4,?4)", params![id,origin,owner,now])?;
        Ok(())
    }

    pub fn read_cycle(&self, id: Option<&str>) -> Result<Option<ReadCycle>> {
        let select = "SELECT id,origin,status,started_at,finished_at,completed_days,total_days,candidate_count,message,scan FROM read_cycles";
        let query = match id {
            Some(_) => format!("{select} WHERE id=?1"),
            None => format!("{select} ORDER BY CASE status WHEN 'running' THEN 0 WHEN 'queued' THEN 1 ELSE 2 END, started_at DESC LIMIT 1"),
        };
        let mut stmt = self.conn.prepare(&query)?;
        let mut rows = stmt.query(rusqlite::params_from_iter(id))?;
        let Some(r) = rows.next()? else {
            return Ok(None);
        };
        let scan: Option<String> = r.get(9)?;
        Ok(Some(ReadCycle {
            id: r.get(0)?,
            origin: r.get(1)?,
            status: r.get(2)?,
            started_at: r.get(3)?,
            finished_at: r.get(4)?,
            completed_days: r.get(5)?,
            total_days: r.get(6)?,
            candidate_count: r.get(7)?,
            message: r.get(8)?,
            scan: scan.map(|s| serde_json::from_str(&s)).transpose()?,
        }))
    }

    pub fn activate_read(&self, id: &str) -> Result<()> {
        self.transaction(|| {
            // Caller holds the OS lock, so any older running owner has stopped.
            self.conn.execute("UPDATE read_cycles SET status='interrupted', finished_at=?2, message='Previous owner stopped; unfinished evidence remains pending' WHERE status='running' AND id<>?1", params![id,chrono::Utc::now().to_rfc3339()])?;
            self.conn.execute("UPDATE eval_runs SET status='error',finished_at=?1,error='Read interrupted' WHERE status='running'", [chrono::Utc::now().to_rfc3339()])?;
            anyhow::ensure!(self.conn.execute("UPDATE read_cycles SET status='running',message='Scanning local evidence' WHERE id=?1 AND status='queued'", [id])? == 1,"Read is no longer queued");
            self.kv_set("last_read_attempt_at", &chrono::Utc::now().to_rfc3339())
        })
    }

    pub fn read_progress(&self, id: &str, days: usize, scan: &ScanOutcome) -> Result<()> {
        self.conn.execute(
            "UPDATE read_cycles SET total_days=?2,scan=?3,message='Preparing evidence' WHERE id=?1",
            params![id, days as i64, serde_json::to_string(scan)?],
        )?;
        Ok(())
    }

    pub fn finish_read(&self, id: &str, status: &str, message: &str) -> Result<()> {
        self.transaction(|| {
            let now = chrono::Utc::now();
            self.conn.execute(
                "UPDATE read_cycles SET status=?2,message=?3,finished_at=?4 WHERE id=?1",
                params![id, status, message, now.to_rfc3339()],
            )?;
            if matches!(status,"failed"|"cancelled"|"interrupted") {
                self.conn.execute("UPDATE eval_runs SET status='error',finished_at=?2,error=?3 WHERE cycle_id=?1 AND status='running'",params![id,now.to_rfc3339(),message])?;
            }
            if status == "completed" {
                self.kv_set("last_successful_read_at", &now.to_rfc3339())?;
                self.kv_set("read_initialized", "true")?;
                self.kv_delete("retry_after")?;
            } else if matches!(status, "failed" | "cancelled" | "interrupted") {
                self.kv_set(
                    "retry_after",
                    &(now + chrono::Duration::minutes(30)).to_rfc3339(),
                )?;
            }
            Ok(())
        })
    }

    pub fn heartbeat_read(&self, id: &str, owner: &str) -> Result<()> {
        let changed = self.conn.execute(
            "UPDATE read_cycles SET heartbeat_at=?3 WHERE id=?1 AND owner=?2 AND status IN ('queued','running')",
            params![id, owner, chrono::Utc::now().to_rfc3339()],
        )?;
        if changed == 0 {
            let matches: bool = self.conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM read_cycles WHERE id=?1 AND owner=?2)",
                params![id, owner],
                |r| r.get(0),
            )?;
            anyhow::ensure!(matches, "Read owner mismatch");
        }
        Ok(())
    }

    pub fn cancel_read(&self, id: &str) -> Result<()> {
        self.conn.execute("UPDATE read_cycles SET cancel_requested=1 WHERE id=?1 AND status IN ('queued','running')",[id])?;
        Ok(())
    }

    pub fn read_cancelled(&self, id: &str, leased: bool) -> Result<bool> {
        let (cancel, heartbeat): (bool, String) = self.conn.query_row(
            "SELECT cancel_requested,heartbeat_at FROM read_cycles WHERE id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        Ok(cancel
            || (leased
                && chrono::Utc::now()
                    .signed_duration_since(chrono::DateTime::parse_from_rfc3339(&heartbeat)?)
                    > chrono::Duration::seconds(15)))
    }

    pub fn auto_due(&self, now: chrono::DateTime<chrono::Utc>, throttle: bool) -> Result<bool> {
        self.transaction(|| {
            if !self.settings().auto_catchup
                || self.kv_get("read_initialized").as_deref() != Some("true")
            {
                return Ok(false);
            }
            let stamp = |key| {
                self.kv_get(key)
                    .and_then(|s| chrono::DateTime::parse_from_rfc3339(&s).ok())
                    .map(|s| s.with_timezone(&chrono::Utc))
            };
            if throttle {
                if stamp("last_auto_check_at")
                    .is_some_and(|s| now.signed_duration_since(s) < chrono::Duration::minutes(15))
                {
                    return Ok(false);
                }
                self.kv_set("last_auto_check_at", &now.to_rfc3339())?;
            }
            if stamp("retry_after").is_some_and(|s| s > now) {
                return Ok(false);
            }
            Ok(stamp("last_successful_read_at")
                .is_some_and(|s| now.signed_duration_since(s) >= chrono::Duration::hours(24)))
        })
    }
}
