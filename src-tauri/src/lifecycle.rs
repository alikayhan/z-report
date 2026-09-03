use std::sync::{Arc, Mutex};

// One lock for updates and every evaluator CLI child process (evaluations and merge
// rewrites): an update must not install while any of them runs, and vice versa.
#[derive(Clone, Default)]
pub struct Lifecycle(Arc<Mutex<Busy>>);

#[derive(Default)]
struct Busy {
    evaluating: bool,
    evaluator_procs: u32,
    updating: bool,
}

pub struct EvaluatorGuard {
    shared: Arc<Mutex<Busy>>,
    evaluation: bool,
}

pub struct UpdateGuard(Arc<Mutex<Busy>>);

impl Lifecycle {
    pub fn begin_evaluation(&self) -> Option<EvaluatorGuard> {
        let mut busy = self.0.lock().unwrap();
        if busy.evaluating || busy.updating {
            return None;
        }
        busy.evaluating = true;
        busy.evaluator_procs += 1;
        Some(EvaluatorGuard {
            shared: self.0.clone(),
            evaluation: true,
        })
    }

    pub fn begin_rewrite(&self) -> Option<EvaluatorGuard> {
        self.begin_evaluator_process()
    }

    pub fn begin_probe(&self) -> Option<EvaluatorGuard> {
        self.begin_evaluator_process()
    }

    fn begin_evaluator_process(&self) -> Option<EvaluatorGuard> {
        let mut busy = self.0.lock().unwrap();
        if busy.updating {
            return None;
        }
        busy.evaluator_procs += 1;
        Some(EvaluatorGuard {
            shared: self.0.clone(),
            evaluation: false,
        })
    }

    pub fn begin_update(&self) -> Result<UpdateGuard, String> {
        let mut busy = self.0.lock().unwrap();
        if busy.updating {
            return Err("An update is already in progress.".into());
        }
        if busy.evaluator_procs > 0 {
            return Err(
                "An evaluation is running. The update can be installed when it finishes.".into(),
            );
        }
        busy.updating = true;
        Ok(UpdateGuard(self.0.clone()))
    }

    pub fn evaluating(&self) -> bool {
        self.0.lock().unwrap().evaluating
    }
}

impl Drop for EvaluatorGuard {
    fn drop(&mut self) {
        let mut busy = self.shared.lock().unwrap();
        busy.evaluator_procs -= 1;
        if self.evaluation {
            busy.evaluating = false;
        }
    }
}

impl Drop for UpdateGuard {
    fn drop(&mut self) {
        self.0.lock().unwrap().updating = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evaluation_is_exclusive_and_released_on_drop() {
        let l = Lifecycle::default();
        let guard = l.begin_evaluation().unwrap();
        assert!(l.evaluating());
        assert!(l.begin_evaluation().is_none());
        drop(guard);
        assert!(!l.evaluating());
        assert!(l.begin_evaluation().is_some());
    }

    #[test]
    fn update_waits_for_all_evaluator_processes() {
        let l = Lifecycle::default();
        let eval = l.begin_evaluation().unwrap();
        let rewrite = l.begin_rewrite().unwrap();
        assert!(l.begin_update().is_err());
        drop(eval);
        assert!(l.begin_update().is_err());
        drop(rewrite);
        assert!(l.begin_update().is_ok());
    }

    #[test]
    fn evaluator_processes_wait_for_update() {
        let l = Lifecycle::default();
        let update = l.begin_update().unwrap();
        assert!(l.begin_evaluation().is_none());
        assert!(l.begin_rewrite().is_none());
        assert!(l.begin_probe().is_none());
        assert!(l.begin_update().is_err());
        drop(update);
        assert!(l.begin_rewrite().is_some());
    }
}
