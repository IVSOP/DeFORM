//! Network-latency probes. Exactly one is compiled in, selected by feature —
//! `rtt-getslot` (the default when none is set), `rtt-ping`, or `rtt-inputs`. Each
//! runs as its own task and publishes the latest RTT (micros) into the shared atomic
//! the sim loop samples. See [`crate`] for how each feeds the ticks-ahead target.

use std::sync::{Arc, atomic::AtomicU64};

use tokio_util::sync::CancellationToken;

/// Latency via an HTTP `getSlot` round-trip. Always works, and measures the same HTTP
/// path `set_inputs` commits take. Reports network RTT only (the ticks-ahead target
/// adds one slot of inclusion on top).
#[cfg(feature = "rtt-getslot")]
pub async fn getslot_task(
    rpc: Arc<solana_rpc_client::nonblocking::rpc_client::RpcClient>,
    rtt_micros: Arc<AtomicU64>,
    cancellation_token: CancellationToken,
) {
    use std::{
        sync::atomic::Ordering,
        time::{Duration, Instant},
    };

    use crate::RTT_SAMPLE_INTERVAL_MS;

    let mut ticker = tokio::time::interval(Duration::from_millis(RTT_SAMPLE_INTERVAL_MS));
    loop {
        tokio::select! {
            _ = ticker.tick() => {
                let t = Instant::now();
                match rpc.get_slot().await {
                    Ok(_) => rtt_micros.store(t.elapsed().as_micros() as u64, Ordering::Relaxed),
                    Err(e) => tracing::debug!("foc getSlot rtt probe failed: {e}"),
                }
            }
            _ = cancellation_token.cancelled() => break,
        }
    }
}

/// Latency via WebSocket control-frame ping/pong on a dedicated connection. Only
/// correct if the ER actually answers control-frame pings — verify first (see the
/// `websockets` probe in the crate docs). Reports network RTT only (the ticks-ahead
/// target adds one slot of inclusion on top).
#[cfg(feature = "rtt-ping")]
pub async fn ping_task(
    ws_url: String,
    rtt_micros: Arc<AtomicU64>,
    cancellation_token: CancellationToken,
) {
    use std::{
        sync::atomic::Ordering,
        time::{Duration, Instant},
    };

    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;
    use tracing::warn;

    use crate::RTT_SAMPLE_INTERVAL_MS;

    let stream = match tokio_tungstenite::connect_async(ws_url.as_str()).await {
        Ok((s, _)) => s,
        Err(e) => {
            warn!("foc ping ws connect failed: {e}");
            return;
        }
    };
    let (mut write, mut read) = stream.split();
    let started = Instant::now();
    let mut ticker = tokio::time::interval(Duration::from_millis(RTT_SAMPLE_INTERVAL_MS));
    ticker.reset();

    loop {
        tokio::select! {
            _ = cancellation_token.cancelled() => break,
            _ = ticker.tick() => {
                let now = started.elapsed().as_micros() as u64;
                if write.send(Message::Ping(now.to_le_bytes().to_vec().into())).await.is_err() {
                    break;
                }
            }
            msg = read.next() => {
                let Some(Ok(msg)) = msg else { break };
                match msg {
                    Message::Pong(p) if p.len() == 8 => {
                        let sent = u64::from_le_bytes(p[..8].try_into().unwrap());
                        let now = started.elapsed().as_micros() as u64;
                        rtt_micros.store(now.saturating_sub(sent), Ordering::Relaxed);
                    }
                    Message::Ping(p) => {
                        let _ = write.send(Message::Pong(p)).await;
                    }
                    Message::Close(_) => break,
                    _ => {}
                }
            }
        }
    }
}

/// Latency via true end-to-end timing. When the sim commits inputs it records the
/// batch's max tick and the send instant in `commit_times`; this task subscribes to
/// the inputs account and, when a committed tick shows up there, reports the elapsed
/// time. That RTT already includes inclusion, so the ticks-ahead target does NOT add a
/// slot on top.
#[cfg(feature = "rtt-inputs")]
pub async fn inputs_rtt_task<U: deform_core::DeformUserLogic>(
    ws_url: String,
    inputs_pda: deform_core::Pubkey,
    commit_times: Arc<std::sync::Mutex<std::collections::BTreeMap<u64, std::time::Instant>>>,
    rtt_micros: Arc<AtomicU64>,
    cancellation_token: CancellationToken,
) {
    use std::sync::atomic::Ordering;

    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;
    use tracing::warn;

    let stream = match tokio_tungstenite::connect_async(ws_url.as_str()).await {
        Ok((s, _)) => s,
        Err(e) => {
            warn!("foc inputs-rtt ws connect failed: {e}");
            return;
        }
    };
    let (mut write, mut read) = stream.split();

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
    if let Err(e) = write.send(Message::Text(sub.into())).await {
        warn!("foc inputs-rtt subscribe failed: {e}");
        return;
    }

    loop {
        tokio::select! {
            _ = cancellation_token.cancelled() => break,
            msg = read.next() => {
                let Some(Ok(msg)) = msg else { break };
                match msg {
                    Message::Text(text) => {
                        let Some(account_max) = decode_max_tick::<U>(&text) else { continue };
                        if let Ok(mut times) = commit_times.lock() {
                            // The freshest commit whose batch-max has landed on-chain.
                            if let Some((&tick, &sent)) = times.range(..=account_max).next_back() {
                                rtt_micros.store(sent.elapsed().as_micros() as u64, Ordering::Relaxed);
                                // drop the measured entry and anything older
                                *times = times.split_off(&(tick + 1));
                            }
                        }
                    }
                    Message::Ping(p) => {
                        let _ = write.send(Message::Pong(p)).await;
                    }
                    Message::Close(_) => break,
                    _ => {}
                }
            }
        }
    }
}

/// Decode an `accountNotification` for the inputs account and return the max tick it
/// currently holds, if any.
#[cfg(feature = "rtt-inputs")]
fn decode_max_tick<U: deform_core::DeformUserLogic>(text: &str) -> Option<u64> {
    use base64::Engine as _;
    use deform_core::accounts::DeformAccount;

    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    if value.get("method")?.as_str()? != "accountNotification" {
        return None;
    }
    let data_b64 = value.pointer("/params/result/value/data/0")?.as_str()?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data_b64)
        .ok()?;
    match DeformAccount::<U>::from_bytes(&bytes).ok()? {
        DeformAccount::Inputs(acc) => acc.inputs.keys().max().copied(),
        _ => None,
    }
}
