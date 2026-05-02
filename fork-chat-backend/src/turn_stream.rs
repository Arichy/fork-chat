use std::collections::HashMap;

use serde_json::Value as JsonValue;
use tokio::sync::{broadcast, mpsc, oneshot};
use uuid::Uuid;

use crate::turn_runtime::stream_event;

const STREAM_COMMAND_BUFFER: usize = 1024;
const TURN_CHANNEL_BUFFER: usize = 256;

/// One live stream event sent to every active subscriber of a turn.
#[derive(Debug, Clone)]
pub struct TurnStreamEvent {
    /// Monotonic sequence number persisted in `runtime_state`.
    pub seq: u64,
    /// Stable event name (for example `assistant_entry_appended`).
    pub event: String,
    /// Event payload content.
    pub payload: JsonValue,
}

/// Commands handled by the turn-stream actor loop.
enum TurnStreamCommand {
    Subscribe {
        turn_id: Uuid,
        reply: oneshot::Sender<broadcast::Receiver<TurnStreamEvent>>,
    },
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
#[derive(Clone)]
pub struct TurnStreamHub {
    tx: mpsc::Sender<TurnStreamCommand>,
}

impl TurnStreamHub {
    /// Create the hub and spawn its actor loop.
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel(STREAM_COMMAND_BUFFER);
        tokio::spawn(async move {
            run_turn_stream_actor(rx).await;
        });
        Self { tx }
    }

    /// Subscribe to one turn's live event stream.
    pub async fn subscribe(&self, turn_id: Uuid) -> broadcast::Receiver<TurnStreamEvent> {
        let (reply_tx, reply_rx) = oneshot::channel();
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
        match reply_rx.await {
            Ok(rx) => rx,
            Err(_) => {
                let (_tx, rx) = broadcast::channel(TURN_CHANNEL_BUFFER);
                rx
            }
        }
    }

    /// Publish one event to all current subscribers of a turn.
    pub async fn publish(&self, turn_id: Uuid, event: TurnStreamEvent) {
        let _ = self
            .tx
            .send(TurnStreamCommand::Publish { turn_id, event })
            .await;
    }
}

/// Actor loop that owns all per-turn broadcast channels.
async fn run_turn_stream_actor(mut rx: mpsc::Receiver<TurnStreamCommand>) {
    let mut channels: HashMap<Uuid, broadcast::Sender<TurnStreamEvent>> = HashMap::new();
    while let Some(cmd) = rx.recv().await {
        match cmd {
            TurnStreamCommand::Subscribe { turn_id, reply } => {
                let sender = get_or_create_sender(&mut channels, turn_id);
                let _ = reply.send(sender.subscribe());
            }
            TurnStreamCommand::Publish { turn_id, event } => {
                let is_terminal_event = matches!(
                    event.event.as_str(),
                    stream_event::TURN_COMPLETED | stream_event::TURN_FAILED
                );
                if let Some(sender) = channels.get(&turn_id).cloned() {
                    let _ = sender.send(event);
                }
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
}

/// Returns an existing per-turn sender or allocates one lazily.
fn get_or_create_sender(
    channels: &mut HashMap<Uuid, broadcast::Sender<TurnStreamEvent>>,
    turn_id: Uuid,
) -> broadcast::Sender<TurnStreamEvent> {
    if let Some(sender) = channels.get(&turn_id) {
        return sender.clone();
    }
    let (sender, _rx) = broadcast::channel(TURN_CHANNEL_BUFFER);
    channels.insert(turn_id, sender.clone());
    sender
}

impl Default for TurnStreamHub {
    fn default() -> Self {
        Self::new()
    }
}
