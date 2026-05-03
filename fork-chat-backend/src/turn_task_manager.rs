//! Process-local cancellation plane for in-flight turn execution tasks.
//!
//! When a turn is running, it may have multiple concurrent async branches:
//! streaming LLM responses, executing tool calls, reading files, running bash
//! commands. If the user cancels the turn (e.g. by sending a new message that
//! triggers a retry), all of those branches need to stop cooperatively.
//!
//! This module provides [`TurnTaskManager`] -- an actor-based registry that
//! maps `turn_id` to a [`CancellationToken`] shared by all branches of a turn's
//! execution. The actor pattern serializes all state mutations so no locks are
//! needed.
//!
//! ## Key design decisions
//!
//! - **Actor model**: All mutable state lives in a single `tokio::spawn`ed
//!   task. Clients send commands via `mpsc` channels. This eliminates locks
//!   and makes the state machine easy to reason about.
//!
//! - **Generation guard**: Each registration gets a unique `generation` UUID.
//!   When `finish()` is called, the entry is only removed if the generation
//!   still matches. This prevents a stale task from removing a newer task's
//!   registration -- a critical race condition when turns are retried.
//!
//! - **Auto-cancellation on re-register**: Calling `register()` for a turn
//!   that already has an active registration cancels the old token first.
//!   This ensures that when a turn is retried, the previous attempt's branches
//!   stop before the new attempt starts.

use std::collections::HashMap;

use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// Buffer size for the actor's command channel. 1024 is generous -- in
/// practice there are at most a handful of concurrent turns, each issuing
/// register/cancel/finish exactly once. The large buffer prevents any
/// realistic risk of backpressure.
const TASK_COMMAND_BUFFER: usize = 1024;

/// Registration metadata returned when a turn loop starts executing.
///
/// This type bundles two pieces of state that are both needed by the turn
/// loop and by the `finish()` call when the loop exits:
///
/// - `token`: The cancellation token that all async branches inside the turn
///   loop must check. When the user cancels the turn or the turn is retried,
///   this token is cancelled, causing all `tokio::select! { _ = token.cancelled() }`
///   branches to fire.
///
/// - `generation`: A unique UUID that acts as a monotonic version counter.
///   When `finish()` is called at loop exit, it passes this generation back.
///   The actor only removes the registration if the generation matches the
///   current entry. This prevents a stale (old) task from removing a newer
///   task's registration after the newer task has already started.
#[derive(Debug, Clone)]
pub struct TurnTaskRegistration {
    /// Cancellation token shared by all async branches inside one turn loop.
    /// Clone this freely -- cancellation is signaled to all clones at once.
    pub token: CancellationToken,
    /// Opaque generation UUID used to prevent stale task cleanup races.
    /// Passed back to [`TurnTaskManager::finish`] to guard against removing
    /// a newer task's registration.
    pub generation: Uuid,
}

/// Internal tracking entry for a running turn task, stored in the actor's
/// `running` HashMap.
#[derive(Debug, Clone)]
struct RunningTurnTask {
    /// Cancellation token for this task. Cancelled when the user cancels the
    /// turn or when a new task for the same `turn_id` is registered.
    token: CancellationToken,
    /// Generation marker for the "latest task for this turn_id". Only the
    /// task whose generation matches the stored value is allowed to clean
    /// up the registration via `finish()`.
    generation: Uuid,
}

/// Commands sent to the task-manager actor via its `mpsc` channel.
///
/// Each command that needs a response includes a `oneshot::Sender` for
/// reply. This avoids the need for a correlation ID system.
enum TurnTaskCommand {
    /// Register a new task for a turn. Returns the fresh registration
    /// (token + generation) via the oneshot reply channel.
    Register {
        turn_id: Uuid,
        reply: oneshot::Sender<TurnTaskRegistration>,
    },
    /// Cancel the currently registered task for a turn (if any). Returns
    /// `true` via the reply channel if a task was found and cancelled.
    Cancel {
        turn_id: Uuid,
        reply: oneshot::Sender<bool>,
    },
    /// Remove a task registration when the turn loop exits. The `generation`
    /// must match the stored generation or the removal is skipped (stale
    /// task protection). No reply is needed -- fire and forget.
    Finish { turn_id: Uuid, generation: Uuid },
}

/// Tracks currently running turn-loop tasks by `turn_id`.
///
/// This manager provides a process-local cancellation plane:
/// - `register` creates a new cancellation token for a turn and cancels any
///   previous token for the same turn (for retry scenarios).
/// - `cancel` requests cooperative cancellation for the current task, if any.
/// - `finish` removes task state only when generation still matches, so an old
///   task cannot remove a newer task's registration.
///
/// The manager is cheaply cloneable (it's just an `mpsc::Sender`) and can be
/// shared freely across the application. All mutable state lives in the
/// actor task spawned during construction.
#[derive(Clone)]
pub struct TurnTaskManager {
    /// Channel to send commands to the background actor task.
    tx: mpsc::Sender<TurnTaskCommand>,
}

impl TurnTaskManager {
    /// Builds an empty task manager and spawns the background actor task.
    ///
    /// The actor runs for the lifetime of the application, processing commands
    /// from the `mpsc` channel. If all `Sender` handles are dropped, the actor
    /// exits cleanly (the `recv()` returns `None`).
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel(TASK_COMMAND_BUFFER);

        // Spawn the actor as a background task. It serializes all mutations
        // to the `running` HashMap, eliminating the need for locks.
        tokio::spawn(async move {
            run_turn_task_actor(rx).await;
        });

        Self { tx }
    }

    /// Register a new active loop for `turn_id`.
    ///
    /// **Cancellation of previous task**: If a previous registration exists
    /// for the same `turn_id`, its cancellation token is cancelled before the
    /// new registration is created. This is essential for **retry scenarios**:
    /// when the user sends a new message that retriggers a turn, the old
    /// loop's branches need to stop before the new loop starts. Without this,
    /// the old loop would continue running alongside the new one, causing
    /// conflicting writes to the database.
    ///
    /// Returns a fresh [`TurnTaskRegistration`] containing the new cancellation
    /// token and generation UUID. The caller should clone the token into every
    /// async branch of the turn loop.
    pub async fn register(&self, turn_id: Uuid) -> TurnTaskRegistration {
        let (reply_tx, reply_rx) = oneshot::channel();

        // Send the Register command and wait for the actor's reply.
        // If the actor has crashed (channel closed), fall through to return
        // a standalone registration so the caller doesn't panic.
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

        // Fallback: actor is dead (shouldn't happen in normal operation).
        // Return a standalone registration so the caller can still function,
        // though cancellation won't be coordinated.
        TurnTaskRegistration {
            token: CancellationToken::new(),
            generation: Uuid::new_v4(),
        }
    }

    /// Cancel the currently registered task (if any) for `turn_id`.
    ///
    /// This triggers **cooperative cancellation**: the token is cancelled,
    /// which causes all `tokio::select! { _ = token.cancelled() }` branches
    /// in the turn loop to fire. The loop branches are responsible for
    /// exiting promptly when they detect cancellation.
    ///
    /// Returns `true` when a running task was found and its token was
    /// cancelled. Returns `false` if no task was registered for this turn
    /// (e.g. the turn had already finished).
    pub async fn cancel(&self, turn_id: Uuid) -> bool {
        let (reply_tx, reply_rx) = oneshot::channel();

        // Send the Cancel command. If the actor is dead, report "not found".
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

        // Wait for the actor to tell us whether a task was found.
        reply_rx.await.unwrap_or(false)
    }

    /// Remove a task registration when a turn loop exits (normally or with
    /// an error).
    ///
    /// **Generation guard**: The removal only happens if the stored
    /// `generation` for this `turn_id` matches the `generation` argument.
    /// This is the critical race-condition prevention mechanism:
    ///
    /// ```text
    /// Timeline:
    ///   T1: Task A registers for turn X (generation=g1)
    ///   T2: User retries turn X
    ///   T3: Task B registers for turn X (generation=g2), Task A's token cancelled
    ///   T4: Task A finally exits and calls finish(turn_x, g1)
    ///   T5: Actor sees g1 != g2, skips removal -- Task B's registration is safe
    /// ```
    ///
    /// Without the generation guard, Task A's `finish()` at T4 would remove
    /// Task B's registration, leaving B running but untracked (and
    /// uncancellable).
    pub async fn finish(&self, turn_id: Uuid, generation: Uuid) {
        // Fire-and-forget: we don't need confirmation that the finish
        // succeeded. If the actor is dead, there's nothing to clean up.
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
///
/// Runs in a dedicated `tokio::spawn` task and processes commands one at a
/// time from the `mpsc` channel. Because all mutations happen in this single
/// task, no locking is required for the `running` HashMap.
///
/// The actor exits cleanly when all `Sender` handles are dropped (i.e. when
/// the `TurnTaskManager` is dropped), because `rx.recv()` returns `None`.
async fn run_turn_task_actor(mut rx: mpsc::Receiver<TurnTaskCommand>) {
    // Map from turn_id to the currently registered task.
    // At most one task per turn_id at any time.
    let mut running: HashMap<Uuid, RunningTurnTask> = HashMap::new();

    // Process commands sequentially. This is safe because the mpsc channel
    // guarantees FIFO ordering and we're the only reader.
    while let Some(cmd) = rx.recv().await {
        match cmd {
            TurnTaskCommand::Register { turn_id, reply } => {
                // Create a fresh registration with new token and generation.
                let registration = build_registration();

                // Insert the new task, replacing any existing registration
                // for the same turn_id. `insert` returns the old entry if
                // one existed.
                let old = running.insert(
                    turn_id,
                    RunningTurnTask {
                        token: registration.token.clone(),
                        generation: registration.generation,
                    },
                );

                // Cancel the old task's token so its branches stop running.
                // This is what makes retries work: the old loop's
                // `tokio::select!` branches detect cancellation and exit.
                if let Some(old) = old {
                    old.token.cancel();
                }

                // Send the registration back to the caller so it can
                // distribute the cancellation token to its branches.
                let _ = reply.send(registration);
            }

            TurnTaskCommand::Cancel { turn_id, reply } => {
                // Find the task and cancel its token, or report "not found".
                let found = if let Some(task) = running.get(&turn_id) {
                    // Signal cancellation. All branches holding a clone of
                    // this token will detect it via `token.cancelled()`.
                    task.token.cancel();
                    true
                } else {
                    // No task registered for this turn -- it may have already
                    // finished or was never started.
                    false
                };
                let _ = reply.send(found);
            }

            TurnTaskCommand::Finish {
                turn_id,
                generation,
            } => {
                // Only remove the registration if the generation matches.
                // This is the generation guard that prevents stale tasks
                // from removing newer tasks' registrations.
                //
                // Scenario we're preventing:
                //   1. Task A (gen=1) is running for turn X
                //   2. User retries -> Task B (gen=2) replaces Task A
                //   3. Task A exits and calls finish(turn_x, gen=1)
                //   4. gen=1 != stored gen=2 -> removal skipped
                //   5. Task B remains properly tracked
                if running
                    .get(&turn_id)
                    .is_some_and(|entry| entry.generation == generation)
                {
                    running.remove(&turn_id);
                }
                // If generation doesn't match, we silently skip the removal.
                // The newer task is still running and will call finish() with
                // its own generation when it exits.
            }
        }
    }
}

/// Creates a fresh token + generation pair for one logical turn execution.
///
/// Each call produces a unique combination, ensuring that every registration
/// is distinguishable from every other registration (even for the same
/// `turn_id` across retries).
fn build_registration() -> TurnTaskRegistration {
    TurnTaskRegistration {
        // New token -- not yet cancelled. All branches sharing this token
        // will be notified when it's cancelled.
        token: CancellationToken::new(),
        // Random UUID acts as a monotonic version for this specific
        // registration attempt. UUID v4 provides sufficient uniqueness.
        generation: Uuid::new_v4(),
    }
}

impl Default for TurnTaskManager {
    fn default() -> Self {
        Self::new()
    }
}
