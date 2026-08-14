//! Supervision for background tasks whose failure invalidates service readiness.

use std::future::Future;
use std::time::Duration;

use anyhow::{Result, anyhow};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::{error, warn};

/// A set of critical tasks tied to one service cancellation token.
///
/// A task returning or panicking before shutdown cancels the service. During a
/// normal shutdown every task gets a bounded drain window before it is aborted.
pub struct CriticalTaskSupervisor {
    tasks: JoinSet<(&'static str, Result<()>)>,
    drain_timeout: Duration,
}

impl CriticalTaskSupervisor {
    #[must_use]
    pub fn new(drain_timeout: Duration) -> Self {
        Self {
            tasks: JoinSet::new(),
            drain_timeout,
        }
    }

    pub fn spawn<F>(&mut self, name: &'static str, task: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        self.tasks.spawn(async move {
            task.await;
            (name, Ok(()))
        });
    }

    /// Adds a critical task that can report a typed failure without panicking.
    ///
    /// This is useful for a coordinator that owns nested tasks: a nested join
    /// failure during normal drain must remain visible to the service root.
    pub fn spawn_result<F>(&mut self, name: &'static str, task: F)
    where
        F: Future<Output = Result<()>> + Send + 'static,
    {
        self.tasks.spawn(async move { (name, task.await) });
    }

    /// Runs until shutdown or the first unexpected critical-task exit.
    pub async fn run(mut self, shutdown: CancellationToken) -> Result<()> {
        let failure = tokio::select! {
            biased;
            _ = shutdown.cancelled() => None,
            joined = self.tasks.join_next() => {
                if shutdown.is_cancelled() {
                    None
                } else {
                    match joined {
                        Some(Ok((name, Ok(())))) => {
                            error!(task = name, "Critical background task exited unexpectedly");
                            Some(anyhow!("critical background task '{name}' exited unexpectedly"))
                        },
                        Some(Ok((name, Err(error)))) => {
                            error!(task = name, %error, "Critical background task failed");
                            Some(anyhow!("critical background task '{name}' failed: {error:#}"))
                        },
                        Some(Err(error)) => {
                            error!(%error, "Critical background task failed");
                            Some(anyhow!("critical background task failed: {error}"))
                        },
                        None => {
                            Some(anyhow!("all critical background tasks exited unexpectedly"))
                        },
                    }
                }
            }
        };

        if failure.is_some() {
            shutdown.cancel();
        }

        let mut drain_failure = None;
        let drain = async {
            while let Some(joined) = self.tasks.join_next().await {
                match joined {
                    Ok((name, Err(error))) => {
                        warn!(task = name, %error, "Background task failed while service was draining");
                        if drain_failure.is_none() {
                            drain_failure = Some(anyhow!(
                                "background task '{name}' failed while service was draining: {error:#}"
                            ));
                        }
                    },
                    Err(error) if !error.is_cancelled() => {
                        warn!(%error, "Background task failed while service was draining");
                        if drain_failure.is_none() {
                            drain_failure = Some(anyhow!(
                                "background task failed while service was draining: {error}"
                            ));
                        }
                    },
                    _ => {},
                }
            }
        };
        let drain_timed_out = tokio::time::timeout(self.drain_timeout, drain)
            .await
            .is_err();
        if drain_timed_out {
            warn!(
                timeout_ms = self.drain_timeout.as_millis(),
                "Background tasks exceeded their shutdown deadline; aborting them"
            );
            self.tasks.abort_all();
            while self.tasks.join_next().await.is_some() {}
        }

        match (failure, drain_failure, drain_timed_out) {
            (Some(error), _, _) | (None, Some(error), _) => Err(error),
            (None, None, true) => Err(anyhow!(
                "background tasks exceeded the {} ms shutdown deadline",
                self.drain_timeout.as_millis()
            )),
            (None, None, false) => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn unexpected_exit_cancels_the_service() {
        let shutdown = CancellationToken::new();
        let mut supervisor = CriticalTaskSupervisor::new(Duration::from_millis(100));
        supervisor.spawn("finished", async {});

        let result = supervisor.run(shutdown.clone()).await;

        assert!(result.is_err());
        assert!(shutdown.is_cancelled());
    }

    #[tokio::test]
    async fn normal_shutdown_drains_cooperative_tasks() {
        let shutdown = CancellationToken::new();
        let mut supervisor = CriticalTaskSupervisor::new(Duration::from_millis(100));
        let task_shutdown = shutdown.clone();
        supervisor.spawn("cooperative", async move {
            task_shutdown.cancelled().await;
        });
        shutdown.cancel();

        supervisor
            .run(shutdown)
            .await
            .expect("normal shutdown succeeds");
    }

    #[tokio::test]
    async fn panic_during_normal_shutdown_is_reported() {
        let shutdown = CancellationToken::new();
        let mut supervisor = CriticalTaskSupervisor::new(Duration::from_millis(100));
        let task_shutdown = shutdown.clone();
        supervisor.spawn("panic-on-drain", async move {
            task_shutdown.cancelled().await;
            panic!("simulated drain panic");
        });
        shutdown.cancel();

        let error = supervisor
            .run(shutdown)
            .await
            .expect_err("a task panic during drain is not a clean shutdown");

        assert!(
            error
                .to_string()
                .contains("failed while service was draining")
        );
    }

    #[tokio::test]
    async fn typed_failure_during_normal_shutdown_is_reported() {
        let shutdown = CancellationToken::new();
        let mut supervisor = CriticalTaskSupervisor::new(Duration::from_millis(100));
        let task_shutdown = shutdown.clone();
        supervisor.spawn_result("error-on-drain", async move {
            task_shutdown.cancelled().await;
            Err(anyhow!("simulated nested task failure"))
        });
        shutdown.cancel();

        let error = supervisor
            .run(shutdown)
            .await
            .expect_err("a typed task error during drain is not a clean shutdown");

        assert!(error.to_string().contains("simulated nested task failure"));
    }

    #[tokio::test]
    async fn shutdown_deadline_is_reported_instead_of_silently_succeeding() {
        let shutdown = CancellationToken::new();
        let mut supervisor = CriticalTaskSupervisor::new(Duration::from_millis(20));
        supervisor.spawn("stuck", std::future::pending::<()>());
        shutdown.cancel();

        let error = supervisor
            .run(shutdown)
            .await
            .expect_err("aborting a stuck critical task is not a clean shutdown");

        assert!(error.to_string().contains("shutdown deadline"));
    }
}
