use std::collections::HashMap;

use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const TASK_COMMAND_BUFFER: usize = 1024;

/// Registration metadata for a running turn-loop task.
#[derive(Debug, Clone)]
pub struct TurnTaskRegistration {
    /// Cancellation token shared by all async branches inside one turn loop.
    pub token: CancellationToken,
    /// Opaque generation id used to prevent stale task cleanup races.
    pub generation: Uuid,
}

#[derive(Debug, Clone)]
struct RunningTurnTask {
    /// Cancellation token used by `cancel_turn_handler` and by loop branches.
    token: CancellationToken,
    /// Monotonic replacement marker for "latest task for this turn id".
    generation: Uuid,
}

/// Commands handled by the task-manager actor loop.
enum TurnTaskCommand {
    Register {
        turn_id: Uuid,
        reply: oneshot::Sender<TurnTaskRegistration>,
    },
    Cancel {
        turn_id: Uuid,
        reply: oneshot::Sender<bool>,
    },
    Finish {
        turn_id: Uuid,
        generation: Uuid,
    },
}

/// Tracks currently running turn-loop tasks by `turn_id`.
///
/// This manager provides a process-local cancellation plane:
/// - `register` creates a new token for a turn and cancels any previous token.
/// - `cancel` requests cooperative cancellation for the current task, if any.
/// - `finish` removes task state only when generation still matches, so an old
///   task cannot remove a newer task's registration.
#[derive(Clone)]
pub struct TurnTaskManager {
    tx: mpsc::Sender<TurnTaskCommand>,
}

impl TurnTaskManager {
    /// Build an empty task manager.
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel(TASK_COMMAND_BUFFER);
        tokio::spawn(async move {
            run_turn_task_actor(rx).await;
        });
        Self { tx }
    }

    /// Register a new active loop for `turn_id`.
    ///
    /// If a previous registration exists, its token is cancelled first so
    /// obsolete work can stop quickly.
    pub async fn register(&self, turn_id: Uuid) -> TurnTaskRegistration {
        let (reply_tx, reply_rx) = oneshot::channel();
        if self
            .tx
            .send(TurnTaskCommand::Register {
                turn_id,
                reply: reply_tx,
            })
            .await
            .is_ok()
            && let Ok(registration) = reply_rx.await
        {
            return registration;
        }

        TurnTaskRegistration {
            token: CancellationToken::new(),
            generation: Uuid::new_v4(),
        }
    }

    /// Cancel the currently registered task (if any) for `turn_id`.
    ///
    /// Returns `true` when a running task was found and cancellation was
    /// requested.
    pub async fn cancel(&self, turn_id: Uuid) -> bool {
        let (reply_tx, reply_rx) = oneshot::channel();
        if self
            .tx
            .send(TurnTaskCommand::Cancel {
                turn_id,
                reply: reply_tx,
            })
            .await
            .is_err()
        {
            return false;
        }
        reply_rx.await.unwrap_or(false)
    }

    /// Remove a task registration when a loop exits.
    ///
    /// The remove is generation-guarded to avoid removing a newer task that
    /// replaced this one while the old task was winding down.
    pub async fn finish(&self, turn_id: Uuid, generation: Uuid) {
        let _ = self
            .tx
            .send(TurnTaskCommand::Finish {
                turn_id,
                generation,
            })
            .await;
    }
}

/// Actor loop that serializes all mutable `running` state updates.
async fn run_turn_task_actor(mut rx: mpsc::Receiver<TurnTaskCommand>) {
    let mut running: HashMap<Uuid, RunningTurnTask> = HashMap::new();
    while let Some(cmd) = rx.recv().await {
        match cmd {
            TurnTaskCommand::Register { turn_id, reply } => {
                let registration = build_registration();
                let old = running.insert(
                    turn_id,
                    RunningTurnTask {
                        token: registration.token.clone(),
                        generation: registration.generation,
                    },
                );
                if let Some(old) = old {
                    old.token.cancel();
                }
                let _ = reply.send(registration);
            }
            TurnTaskCommand::Cancel { turn_id, reply } => {
                let found = if let Some(task) = running.get(&turn_id) {
                    task.token.cancel();
                    true
                } else {
                    false
                };
                let _ = reply.send(found);
            }
            TurnTaskCommand::Finish {
                turn_id,
                generation,
            } => {
                if running
                    .get(&turn_id)
                    .is_some_and(|entry| entry.generation == generation)
                {
                    running.remove(&turn_id);
                }
            }
        }
    }
}

/// Creates a fresh token+generation pair for one logical turn execution.
fn build_registration() -> TurnTaskRegistration {
    TurnTaskRegistration {
        token: CancellationToken::new(),
        generation: Uuid::new_v4(),
    }
}

impl Default for TurnTaskManager {
    fn default() -> Self {
        Self::new()
    }
}
