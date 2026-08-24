use std::time::Duration;

use base64::Engine as _;
use deform_core::{DeformError, DeformResult, DeformUserLogic, Pubkey, accounts::DeformAccount};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

/// How often to send a keepalive ping while the subscribed account is otherwise idle.
const KEEPALIVE_SECS: u64 = 20;

/// Subscribe to `lobby_pda` and pump decoded [`LobbyState`]s into `state_tx`. Signals
/// `ready` once the subscription is confirmed (or fails). Runs until `terminate`
/// fires or the socket closes.
///
// TODO: implement retry logic, as sometimes websockets close for no reason
// TODO: better error handling
pub async fn ws_task<U: DeformUserLogic>(
    ws_url: String,
    lobby_pda: Pubkey,
    inputs_pda: Pubkey,
    account_update_tx: mpsc::UnboundedSender<DeformAccount<U>>,
    ready: oneshot::Sender<DeformResult>,
    cancellation_token: CancellationToken,
) {
    let lobby_stream = match tokio_tungstenite::connect_async(ws_url.as_str()).await {
        Ok((stream, _resp)) => stream,
        Err(e) => {
            let _ = ready.send(Err(DeformError::Connection(format!(
                "websocket connect to {ws_url}: {e}"
            ))));
            return;
        }
    };
    let inputs_stream = match tokio_tungstenite::connect_async(ws_url.as_str()).await {
        Ok((stream, _resp)) => stream,
        Err(e) => {
            let _ = ready.send(Err(DeformError::Connection(format!(
                "websocket connect to {ws_url}: {e}"
            ))));
            return;
        }
    };
    let (mut lobby_write, mut lobby_read) = lobby_stream.split();
    let (mut inputs_write, mut inputs_read) = inputs_stream.split();

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
    if let Err(e) = lobby_write.send(Message::Text(sub.into())).await {
        let _ = ready.send(Err(DeformError::Connection(format!(
            "websocket send subscribe: {e}"
        ))));
        return;
    }

    let sub = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "accountSubscribe",
        "params": [
            inputs_pda.to_string(),
            { "encoding": "base64", "commitment": "processed" }
        ]
    })
    .to_string();
    if let Err(e) = inputs_write.send(Message::Text(sub.into())).await {
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
                if let Err(e) = lobby_write.send(Message::Ping(Vec::new().into())).await {
                    warn!("foc ws keepalive ping failed: {e}");
                    break;
                }
                if let Err(e) = inputs_write.send(Message::Ping(Vec::new().into())).await {
                    warn!("foc ws keepalive ping failed: {e}");
                    break;
                }
            }

            lobby_update_msg = lobby_read.next() => {
                let Some(msg) = lobby_update_msg else { break };
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
                        match handle_text(&text) {
                            TextOutcome::Subscribed => {
                                if let Some(tx) = ready.take() {
                                    let _ = tx.send(Ok(()));
                                }
                            }
                            // FIX: this is repeated exactly for the inputs account
                            // need to have some sort of utility that receives N addresses and listens on them all cleanly
                            TextOutcome::State(bytes) => {
                                let account = match DeformAccount::<U>::from_bytes(&bytes) {
                                    Ok(account) => account,
                                    Err(e) => {
                                        warn!("foc ws: lobby decode failed: {e}");
                                        continue;
                                    }
                                };

                                if account_update_tx.send(account).is_err() {
                                    break;
                                }
                            }
                            TextOutcome::Ignored => {}
                        }
                    }
                    // Some servers ping us for keepalive; answer so we aren't dropped.
                    Message::Ping(payload) => {
                        if lobby_write.send(Message::Pong(payload)).await.is_err() {
                            break;
                        }
                    }
                    // Pong from our own keepalive ping; nothing to do.
                    Message::Pong(_) => {}
                    Message::Close(_) => break,
                    _ => {}
                }
            }

            inputs_update_msg = inputs_read.next() => {
                let Some(msg) = inputs_update_msg else { break };
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
                        match handle_text(&text) {
                            TextOutcome::Subscribed => {
                                if let Some(tx) = ready.take() {
                                    let _ = tx.send(Ok(()));
                                }
                            }
                            TextOutcome::State(bytes) => {
                                let account = match DeformAccount::<U>::from_bytes(&bytes) {
                                    Ok(account) => account,
                                    Err(e) => {
                                        warn!("foc ws: inputs decode failed: {e}");
                                        continue;
                                    }
                                };

                                if account_update_tx.send(account).is_err() {
                                    break;
                                }
                            }
                            TextOutcome::Ignored => {}
                        }
                    }
                    // Some servers ping us for keepalive; answer so we aren't dropped.
                    Message::Ping(payload) => {
                        if lobby_write.send(Message::Pong(payload)).await.is_err() {
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

enum TextOutcome {
    /// The `accountSubscribe` request was acknowledged.
    Subscribed,
    /// Account update
    State(Vec<u8>),
    /// Anything we don't care about (or couldn't decode).
    Ignored,
}

// FIX: stop using TextOutcome::Ignored for errors, just use a regular result
fn handle_text(text: &str) -> TextOutcome {
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

    TextOutcome::State(bytes)
}
