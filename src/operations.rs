//! Cross-process exclusivity for consistency-sensitive library operations.

use std::future::Future;
use std::time::Duration;

use anyhow::{anyhow, Result};
use futures_util::FutureExt;
use tokio::task::JoinHandle;
use tracing::warn;

use crate::db::{Database, OperationRunRecord, LIBRARY_OPERATION_LOCK};

const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);

#[derive(Debug, Clone)]
pub struct OperationRequest {
    pub kind: &'static str,
    pub origin: &'static str,
    pub scope: Option<String>,
}

impl OperationRequest {
    pub fn new(kind: &'static str, origin: &'static str, scope: Option<String>) -> Self {
        Self {
            kind,
            origin,
            scope,
        }
    }
}

#[derive(Clone)]
pub struct OperationCoordinator {
    db: Database,
}

impl OperationCoordinator {
    pub fn new(db: Database) -> Self {
        Self { db }
    }

    pub async fn acquire(&self, request: OperationRequest) -> Result<OperationLease> {
        let run = self
            .db
            .try_acquire_operation(
                LIBRARY_OPERATION_LOCK,
                request.kind,
                request.origin,
                request.scope.as_deref(),
            )
            .await??;
        Ok(OperationLease::new(self.db.clone(), run))
    }

    pub async fn run<T, F>(&self, request: OperationRequest, future: F) -> Result<T>
    where
        F: Future<Output = Result<T>> + Send,
        T: Send,
    {
        let lease = self.acquire(request).await?;
        Self::run_acquired(lease, future).await
    }

    pub async fn run_acquired<T, F>(mut lease: OperationLease, future: F) -> Result<T>
    where
        F: Future<Output = Result<T>> + Send,
        T: Send,
    {
        match std::panic::AssertUnwindSafe(future).catch_unwind().await {
            Ok(Ok(value)) => {
                lease.succeed(None, None).await?;
                Ok(value)
            }
            Ok(Err(err)) => {
                let message = err.to_string();
                if let Err(finalize_err) = lease.fail(&message).await {
                    return Err(anyhow!(
                        "operation failed: {}; additionally could not record terminal state: {}",
                        message,
                        finalize_err
                    ));
                }
                Err(err)
            }
            Err(panic) => {
                let message = format!("operation panicked: {}", panic_message(panic));
                if let Err(finalize_err) = lease.fail(&message).await {
                    return Err(anyhow!(
                        "{}; additionally could not record terminal state: {}",
                        message,
                        finalize_err
                    ));
                }
                Err(anyhow!(message))
            }
        }
    }
}

pub struct OperationLease {
    db: Database,
    run: OperationRunRecord,
    heartbeat: Option<JoinHandle<()>>,
}

impl OperationLease {
    fn new(db: Database, run: OperationRunRecord) -> Self {
        let heartbeat_db = db.clone();
        let operation_id = run.id;
        let heartbeat = tokio::spawn(async move {
            let mut interval = tokio::time::interval(HEARTBEAT_INTERVAL);
            interval.tick().await;
            loop {
                interval.tick().await;
                match heartbeat_db.heartbeat_operation(operation_id).await {
                    Ok(true) => {}
                    Ok(false) => return,
                    Err(err) => warn!(operation_id, "Operation heartbeat failed: {}", err),
                }
            }
        });
        Self {
            db,
            run,
            heartbeat: Some(heartbeat),
        }
    }

    pub fn id(&self) -> i64 {
        self.run.id
    }

    async fn terminal(
        &mut self,
        status: &str,
        message: Option<&str>,
        result_json: Option<&str>,
    ) -> Result<()> {
        // Keep the heartbeat alive until the terminal write succeeds. If SQLite is
        // temporarily unavailable, dropping this lease retains a live heartbeat and
        // its best-effort interrupted fallback rather than stranding a fresh lock.
        let finished = self
            .db
            .finish_operation(self.run.id, status, message, result_json)
            .await?;
        if !finished {
            anyhow::bail!("operation {} is no longer running", self.run.id);
        }
        if let Some(heartbeat) = self.heartbeat.take() {
            heartbeat.abort();
            let _ = heartbeat.await;
        }
        Ok(())
    }

    pub async fn succeed(
        &mut self,
        message: Option<&str>,
        result_json: Option<&str>,
    ) -> Result<()> {
        self.terminal("succeeded", message, result_json).await
    }

    pub async fn fail(&mut self, message: &str) -> Result<()> {
        self.terminal("failed", Some(message), None).await
    }
}

impl Drop for OperationLease {
    fn drop(&mut self) {
        if let Some(heartbeat) = self.heartbeat.take() {
            heartbeat.abort();
            let db = self.db.clone();
            let operation_id = self.run.id;
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.spawn(async move {
                    if let Err(err) = db
                        .finish_operation(
                            operation_id,
                            "interrupted",
                            Some("Operation lease dropped before a terminal outcome"),
                            None,
                        )
                        .await
                    {
                        warn!(
                            operation_id,
                            "Could not record dropped operation lease: {}", err
                        );
                    }
                });
            }
        }
    }
}

fn panic_message(payload: Box<dyn std::any::Any + Send + 'static>) -> String {
    if let Some(message) = payload.downcast_ref::<&'static str>() {
        (*message).to_string()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "non-string panic payload".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn coordinator_records_error_and_panic_as_terminal_failures() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::new(dir.path().join("operations.db").to_str().unwrap())
            .await
            .unwrap();
        let coordinator = OperationCoordinator::new(db.clone());
        assert!(coordinator
            .run(OperationRequest::new("scan", "cli", None), async {
                Err::<(), _>(anyhow!("expected failure"))
            },)
            .await
            .is_err());
        assert!(coordinator
            .run(OperationRequest::new("repair_auto", "web", None), async {
                panic!("expected panic");
                #[allow(unreachable_code)]
                Ok::<(), anyhow::Error>(())
            },)
            .await
            .is_err());
        let history = db.list_operation_runs(10).await.unwrap();
        assert_eq!(history.len(), 2);
        assert!(history.iter().all(|run| run.status == "failed"));
    }

    #[tokio::test]
    async fn nested_domain_work_does_not_reacquire_the_top_level_operation() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::new(dir.path().join("operations.db").to_str().unwrap())
            .await
            .unwrap();
        let coordinator = OperationCoordinator::new(db.clone());
        coordinator
            .run(OperationRequest::new("scan", "cli", None), async {
                // Relink scans are domain helpers, so they inherit this operation
                // instead of trying to acquire the same persistent lock again.
                tokio::task::yield_now().await;
                Ok(())
            })
            .await
            .unwrap();
        assert_eq!(db.list_operation_runs(10).await.unwrap().len(), 1);
    }
}
