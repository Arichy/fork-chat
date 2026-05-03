//! Actor-based pub/sub hub for per-turn SSE live events.
//!
//! This module implements the ephemeral (in-memory) delivery layer for the
//! "Snapshot + Live SSE" architecture. The `TurnStreamHub` owns a single
//! actor task that manages per-turn broadcast channels. The lifecycle loop
//! publishes events through the hub, and SSE handlers subscribe to receive them.
//!
//! # Why an actor instead of `DashMap` / `Mutex<HashMap>`?
//!
//! The single-actor pattern (one `mpsc` channel + message loop) has a key
//! advantage: all mutable state lives in one place (the actor's stack) and is
//! accessed sequentially. This eliminates the need for external locks, avoids
//! deadlocks, and makes the lifecycle of broadcast channels deterministic.
//!
//! The trade-off is that every subscribe/publish goes through an async channel,
//! adding a small amount of latency. For SSE delivery this is negligible.
//!
//! # Channel lifecycle
//!
//! Per-turn broadcast channels are created lazily on first subscribe and cleaned
//! up when one of two conditions is met:
//! 1. **No subscribers remain**: the broadcast sender's `receiver_count()` is 0,
//!    meaning all SSE connections for this turn have disconnected
//! 2. **A terminal event is published**: `turn_completed` or `turn_failed` means
//!    no further events will be produced for this turn
//!
//! This ensures channels don't leak memory for completed turns while still
//! allowing new subscribers to connect to an in-progress turn.

use std::collections::HashMap;

use serde_json::Value as JsonValue;
use tokio::sync::{broadcast, mpsc, oneshot};
use uuid::Uuid;

use crate::turn_runtime::stream_event;

/// Buffer size for the command channel between `TurnStreamHub` and its actor.
///
/// This is the mpsc channel that carries Subscribe and Publish commands.
/// 1024 is generous -- in practice, the lifecycle loop publishes events
/// sequentially (one at a time per turn) and subscriptions are infrequent
/// (one per SSE connection). A larger buffer prevents backpressure if the
/// actor is momentarily slow (e.g. during channel cleanup).
const STREAM_COMMAND_BUFFER: usize = 1024;

/// Buffer capacity for each per-turn broadcast channel.
///
/// Each turn gets its own `tokio::sync::broadcast` channel with this capacity.
///
/// # Why 256?
///
/// A single multi-round turn can produce many events. Consider a turn with
/// 10 rounds, each containing 2 tool calls:
/// - 1 turn_started
/// - 10 round_started
/// - 10 assistant_entry_appended
/// - 10 tool_calls
/// - 10 tool_result_appended
/// - 1 turn_completed
///
/// That is ~42 events. 256 provides a comfortable margin for complex turns
/// while keeping per-turn memory usage reasonable (each event is a small
/// JSON payload + metadata, so ~256 * ~1KB = ~256KB per active turn).
///
/// If a subscriber is slow and the buffer fills, the `broadcast` channel
/// sends a `Lagged` error, which the SSE handler handles gracefully by
/// continuing (the subscriber will get the next snapshot on reconnect).
const TURN_CHANNEL_BUFFER: usize = 256;

/// One live stream event sent to every active subscriber of a turn.
///
/// This is the unit of delivery over the in-memory broadcast channel. Each
/// event carries the monotonic sequence number (matching the persisted
/// `runtime_state.stream_seq`), the event type name, and a JSON payload.
///
/// The `seq` field is what enables the SSE handler to implement the
/// snapshot/live boundary: after emitting a snapshot, the handler only
/// forwards events with `seq > baseline_seq`.
#[derive(Debug, Clone)]
pub struct TurnStreamEvent {
    /// Monotonic sequence number from `runtime_state.stream_seq`.
    ///
    /// This matches the `stream_seq` that was persisted in the DB at the time
    /// the event was published. It is used by the SSE handler to skip events
    /// that are already reflected in the snapshot.
    pub seq: u64,
    /// Stable event name (e.g. `assistant_entry_appended`, `tool_calls`).
    ///
    /// Becomes the SSE `event:` field in the HTTP response.
    pub event: String,
    /// Event payload content -- the event-specific JSON data.
    ///
    /// Becomes the SSE `data:` field in the HTTP response.
    pub payload: JsonValue,
}

/// Commands handled by the turn-stream actor loop.
///
/// The hub sends these commands through an mpsc channel to the single actor
/// task, which processes them sequentially. This eliminates the need for
/// external locking on the per-turn channel map.
enum TurnStreamCommand {
    /// Subscribe to a turn's live event stream.
    ///
    /// The actor creates (or reuses) a broadcast sender for the given turn_id
    /// and returns a new broadcast receiver through the oneshot reply channel.
    Subscribe {
        turn_id: Uuid,
        reply: oneshot::Sender<broadcast::Receiver<TurnStreamEvent>>,
    },
    /// Publish one event to all current subscribers of a turn.
    ///
    /// The actor sends the event through the turn's broadcast sender. If the
    /// turn has no subscribers or the event is terminal, the channel is cleaned up.
    Publish {
        turn_id: Uuid,
        event: TurnStreamEvent,
    },
}

/// Process-local pub/sub hub for per-turn SSE updates.
///
/// Internally this uses a single actor task (channel + message loop) so all
/// shared mutable state lives in one place instead of being protected by
/// external locks.
///
/// # Usage
///
/// - `TurnStreamHub::new()` creates the hub and spawns its actor task
/// - `hub.subscribe(turn_id)` returns a broadcast receiver for that turn's events
/// - `hub.publish(turn_id, event)` sends an event to all current subscribers
///
/// The hub is cheaply cloneable (it wraps an `mpsc::Sender`). Typically one
/// instance is stored in `AppState` and shared across all handlers and
/// lifecycle tasks.
#[derive(Clone)]
pub struct TurnStreamHub {
    /// Sender half of the command channel to the actor task.
    ///
    /// All method calls on `TurnStreamHub` send commands through this channel.
    /// The actor task owns the receiver and processes commands one at a time.
    tx: mpsc::Sender<TurnStreamCommand>,
}

impl TurnStreamHub {
    /// Create the hub and spawn its actor loop.
    ///
    /// This spawns a background tokio task that owns all per-turn broadcast
    /// channels. The task runs until the `TurnStreamHub` and all its clones
    /// are dropped (which closes the mpsc channel, causing the actor to exit).
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel(STREAM_COMMAND_BUFFER);
        tokio::spawn(async move {
            run_turn_stream_actor(rx).await;
        });
        Self { tx }
    }

    /// Subscribe to one turn's live event stream.
    ///
    /// Returns a `broadcast::Receiver` that yields `TurnStreamEvent`s for the
    /// given turn. If the actor task has crashed (should not happen in normal
    /// operation), returns a fresh unused receiver instead of panicking.
    ///
    /// # Error handling
    ///
    /// If the command cannot be sent (actor dropped) or the oneshot reply
    /// fails (actor crashed between receive and reply), we return an empty
    /// broadcast receiver. The SSE handler will simply not receive any live
    /// events, which is safe -- the subscriber still gets the snapshot.
    pub async fn subscribe(&self, turn_id: Uuid) -> broadcast::Receiver<TurnStreamEvent> {
        let (reply_tx, reply_rx) = oneshot::channel();
        // Send the subscribe command to the actor. If the actor is gone,
        // return an empty receiver so the SSE handler can still emit a snapshot.
        if self
            .tx
            .send(TurnStreamCommand::Subscribe {
                turn_id,
                reply: reply_tx,
            })
            .await
            .is_err()
        {
            let (_tx, rx) = broadcast::channel(TURN_CHANNEL_BUFFER);
            return rx;
        }
        // Wait for the actor to send back the broadcast receiver.
        // If the actor crashes between receiving the command and replying,
        // fall back to an empty receiver.
        match reply_rx.await {
            Ok(rx) => rx,
            Err(_) => {
                let (_tx, rx) = broadcast::channel(TURN_CHANNEL_BUFFER);
                rx
            }
        }
    }

    /// Publish one event to all current subscribers of a turn.
    ///
    /// This is a fire-and-forget operation. Errors (actor gone, no subscribers)
    /// are silently ignored because:
    /// - If the actor is gone, there are no subscribers to deliver to
    /// - If there are no subscribers, the event is simply not needed
    ///
    /// The event may still be useful for triggering channel cleanup (e.g. a
    /// terminal event removes the channel from the map).
    pub async fn publish(&self, turn_id: Uuid, event: TurnStreamEvent) {
        let _ = self
            .tx
            .send(TurnStreamCommand::Publish { turn_id, event })
            .await;
    }
}

/// Actor loop that owns all per-turn broadcast channels.
///
/// This function runs as a spawned tokio task. It receives commands from
/// `TurnStreamHub` via the mpsc channel and processes them sequentially.
///
/// # State management
///
/// The `channels` HashMap is the only mutable state, and it lives entirely
/// on this function's stack. No external code can access it directly, which
/// means no locks are needed and no deadlocks are possible.
///
/// # Channel cleanup
///
/// Channels are removed from the map when:
/// 1. A publish finds zero subscribers for that turn (`receiver_count() == 0`)
/// 2. A terminal event is published (`turn_completed` or `turn_failed`)
///
/// Both conditions are checked after every publish. This means channels for
/// in-progress turns with active SSE connections stay alive, while channels
/// for completed turns or turns with no viewers are promptly cleaned up.
async fn run_turn_stream_actor(mut rx: mpsc::Receiver<TurnStreamCommand>) {
    // Map from turn_id to broadcast sender. Each sender fans out to all
    // active SSE subscribers for that turn. Channels are lazily created
    // on first subscribe and cleaned up on terminal event or zero subscribers.
    let mut channels: HashMap<Uuid, broadcast::Sender<TurnStreamEvent>> = HashMap::new();
    while let Some(cmd) = rx.recv().await {
        match cmd {
            TurnStreamCommand::Subscribe { turn_id, reply } => {
                // Get or create a broadcast sender for this turn, then
                // return a new receiver from it. The receiver will start
                // receiving events from this point forward.
                let sender = get_or_create_sender(&mut channels, turn_id);
                // Ignore error: if the oneshot reply fails, the caller
                // already gave up (e.g. the SSE connection closed between
                // subscribe and reply).
                let _ = reply.send(sender.subscribe());
            }
            TurnStreamCommand::Publish { turn_id, event } => {
                // Check if this is a terminal event before publishing.
                // Terminal events trigger cleanup regardless of subscriber count
                // because no more events will be produced for this turn.
                let is_terminal_event = matches!(
                    event.event.as_str(),
                    stream_event::TURN_COMPLETED | stream_event::TURN_FAILED
                );
                // Send the event to all current subscribers. The `send` on a
                // broadcast channel delivers to all active receivers. If no
                // receivers exist, the send is a no-op.
                if let Some(sender) = channels.get(&turn_id).cloned() {
                    // Ignore send result: `Err` just means no receivers,
                    // which is fine -- we'll clean up below.
                    let _ = sender.send(event);
                }
                // Clean up the channel if there are no more subscribers or
                // if a terminal event was just published. This prevents memory
                // leaks from accumulating channels for completed turns.
                if channels
                    .get(&turn_id)
                    .is_some_and(|sender| sender.receiver_count() == 0)
                    || is_terminal_event
                {
                    channels.remove(&turn_id);
                }
            }
        }
    }
    // When the mpsc sender is dropped (all TurnStreamHub clones gone),
    // rx.recv() returns None and the actor exits. All remaining broadcast
    // channels are dropped, which closes any remaining receivers.
}

/// Returns an existing per-turn sender or allocates one lazily.
///
/// This is called from inside the actor loop, so it has exclusive access to
/// the channel map. No locking is needed.
fn get_or_create_sender(
    channels: &mut HashMap<Uuid, broadcast::Sender<TurnStreamEvent>>,
    turn_id: Uuid,
) -> broadcast::Sender<TurnStreamEvent> {
    if let Some(sender) = channels.get(&turn_id) {
        // Channel already exists -- clone the sender (broadcast senders are
        // cheaply cloneable, they share the same underlying buffer).
        return sender.clone();
    }
    // First subscriber for this turn -- allocate a new broadcast channel.
    // The `_rx` is immediately dropped because the actor only needs senders;
    // receivers are created on demand via `sender.subscribe()`.
    let (sender, _rx) = broadcast::channel(TURN_CHANNEL_BUFFER);
    channels.insert(turn_id, sender.clone());
    sender
}

impl Default for TurnStreamHub {
    fn default() -> Self {
        Self::new()
    }
}
