//! The WebSocket half of the FoC backend: `accountSubscribe` on the lobby PDA.
//! Decoded lobby states are forwarded to the simulation loop. Latency is measured
//! separately over HTTP (`getSlot`, see [`crate`]), so this task doesn't do RTT
//! ping/pong — it only keepalive-pings and answers server pings so the socket stays
//! open even while the lobby is idle.

use std::time::Duration;

use base64::Engine as _;
use deform_core::{
    DeformError, DeformResult, DeformUserLogic, Pubkey,
    accounts::{DeformAccount, lobby::LobbyState},
};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

/// How often to send a keepalive ping while the lobby account is otherwise idle.
const KEEPALIVE_SECS: u64 = 20;

/// Subscribe to `lobby_pda` and pump decoded [`LobbyState`]s into `state_tx`. Signals
/// `ready` once the subscription is confirmed (or fails). Runs until `terminate`
/// fires or the socket closes.
pub async fn ws_task<U: DeformUserLogic>(
    ws_url: String,
    lobby_pda: Pubkey,
    state_tx: mpsc::UnboundedSender<LobbyState<U>>,
    ready: oneshot::Sender<DeformResult>,
    cancellation_token: CancellationToken,
) {
    let stream = match tokio_tungstenite::connect_async(ws_url.as_str()).await {
        Ok((stream, _resp)) => stream,
        Err(e) => {
            let _ = ready.send(Err(DeformError::Connection(format!(
                "websocket connect to {ws_url}: {e}"
            ))));
            return;
        }
    };
    let (mut write, mut read) = stream.split();

    // accountSubscribe with the lowest-latency commitment; the ER is a single
    // validator so `processed` is effectively final.
    let sub = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "accountSubscribe",
        "params": [
            lobby_pda.to_string(),
            { "encoding": "base64", "commitment": "processed" }
        ]
    })
    .to_string();

    if let Err(e) = write.send(Message::Text(sub.into())).await {
        let _ = ready.send(Err(DeformError::Connection(format!(
            "websocket send subscribe: {e}"
        ))));
        return;
    }

    let mut ready = Some(ready);
    let mut keepalive = tokio::time::interval(Duration::from_secs(KEEPALIVE_SECS));
    keepalive.reset();

    loop {
        tokio::select! {
            _ = cancellation_token.cancelled() => break,

            _ = keepalive.tick() => {
                if let Err(e) = write.send(Message::Ping(Vec::new().into())).await {
                    warn!("foc ws keepalive ping failed: {e}");
                    break;
                }
            }

            msg = read.next() => {
                let Some(msg) = msg else { break };
                let msg = match msg {
                    Ok(m) => m,
                    Err(e) => {
                        if let Some(tx) = ready.take() {
                            let _ = tx.send(Err(DeformError::Connection(format!("websocket: {e}"))));
                        }
                        warn!("foc ws recv error: {e}");
                        break;
                    }
                };

                match msg {
                    Message::Text(text) => {
                        match handle_text::<U>(&text) {
                            TextOutcome::Subscribed => {
                                if let Some(tx) = ready.take() {
                                    let _ = tx.send(Ok(()));
                                }
                            }
                            TextOutcome::State(state) => {
                                // receiver gone => simulation loop ended; stop.
                                if state_tx.send(state).is_err() {
                                    break;
                                }
                            }
                            TextOutcome::Ignored => {}
                        }
                    }
                    // Some servers ping us for keepalive; answer so we aren't dropped.
                    Message::Ping(payload) => {
                        if write.send(Message::Pong(payload)).await.is_err() {
                            break;
                        }
                    }
                    // Pong from our own keepalive ping; nothing to do.
                    Message::Pong(_) => {}
                    Message::Close(_) => break,
                    _ => {}
                }
            }
        }
    }

    // If we never confirmed the subscription, report failure so setup doesn't hang.
    if let Some(tx) = ready.take() {
        let _ = tx.send(Err(DeformError::Connection(
            "websocket closed before subscription was confirmed".into(),
        )));
    }
    debug!("foc ws task exiting");
}

enum TextOutcome<U: DeformUserLogic> {
    /// The `accountSubscribe` request was acknowledged.
    Subscribed,
    /// An `accountNotification` we successfully decoded into a lobby state.
    State(LobbyState<U>),
    /// Anything we don't care about (or couldn't decode).
    Ignored,
}

// FIX: this is cursed, is it even being used??
fn handle_text<U: DeformUserLogic>(text: &str) -> TextOutcome<U> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
        return TextOutcome::Ignored;
    };

    // Subscription ack: {"jsonrpc":"2.0","result":<subId>,"id":1}
    if value.get("method").is_none() && value.get("result").and_then(|r| r.as_u64()).is_some() {
        return TextOutcome::Subscribed;
    }

    if value.get("method").and_then(|m| m.as_str()) != Some("accountNotification") {
        return TextOutcome::Ignored;
    }

    // params.result.value.data[0] is the base64-encoded account data.
    let Some(data_b64) = value
        .pointer("/params/result/value/data/0")
        .and_then(|d| d.as_str())
    else {
        return TextOutcome::Ignored;
    };

    let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(data_b64) else {
        warn!("foc ws: base64 decode of account data failed");
        return TextOutcome::Ignored;
    };

    match DeformAccount::<U>::from_bytes(&bytes) {
        Ok(DeformAccount::Lobby(lobby)) => TextOutcome::State(lobby.state),
        Ok(_) => TextOutcome::Ignored,
        Err(e) => {
            warn!("foc ws: lobby decode failed: {e}");
            TextOutcome::Ignored
        }
    }
}
