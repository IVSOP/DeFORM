use std::collections::{HashMap, HashSet};

use deform_core::Pubkey;
use deform_core::{
    DeformClient, DeformError, DeformGameState, DeformInputs, DeformResult, DeformUserLogic,
    lobby::LobbyData,
};
use solana_signature::Signature;
use wincode::{SchemaRead, SchemaWrite};

mod client;

pub const ALPN_PROTOCOL: &[u8] = b"deform/1";

wincode::pod_wrapper! {
    unsafe struct PodPubkey(Pubkey);
    unsafe struct PodSignature(Signature);
}

/// Messages on the reliable control stream (the bi-directional stream
/// opened during auth and kept open for the lifetime of the connection).
#[derive(Clone, SchemaRead, SchemaWrite, Debug)]
pub enum ControlMessage {
    // client → server
    /// Initial auth — client identifies itself and the lobby.
    Handshake {
        lobby_id: u64,
        #[wincode(with = "PodPubkey")]
        player_pubkey: Pubkey,
        #[wincode(with = "PodSignature")]
        sig: Signature,
    },
    // server → client
    /// Auth succeeded — server confirms the handshake.
    AuthOk,
    /// The game has ended — client should wrap up.
    Finish,
    /// An error occurred — contains a human-readable description.
    Error(String),
}

#[derive(Clone, SchemaRead, SchemaWrite)]
pub enum ServerUnreliableInstruction<I: DeformInputs> {
    BatchSetInputs(HashMap<u64, I>),
}

#[derive(Clone, SchemaRead, SchemaWrite)]
pub enum ServerResponse<I: DeformInputs, G: DeformGameState> {
    Error(String),
    // TODO: might be wasteful but it better mimics the fully on-chain behaviour
    NewState(LobbyData<I, G>),
}

const MAX_CONTROL_MSG_SIZE: usize = 4096;

pub async fn write_control(send: &mut quinn::SendStream, msg: &ControlMessage) -> DeformResult {
    let data = wincode::serialize(msg)
        .map_err(|e| DeformError::Serialize(format!("control message: {e:?}")))?;
    send.write_all(&(data.len() as u32).to_le_bytes())
        .await
        .map_err(|e| DeformError::Connection(e.to_string()))?;
    send.write_all(data.as_slice())
        .await
        .map_err(|e| DeformError::Connection(e.to_string()))?;
    Ok(())
}

pub async fn read_control(recv: &mut quinn::RecvStream) -> DeformResult<ControlMessage> {
    let mut len_buf = [0u8; 4];
    recv.read_exact(&mut len_buf)
        .await
        .map_err(|e| DeformError::Connection(e.to_string()))?;
    let len = u32::from_le_bytes(len_buf) as usize;
    if len > MAX_CONTROL_MSG_SIZE {
        return Err(DeformError::Protocol(format!(
            "control message too large: {len} bytes",
        )));
    }
    let mut buf = vec![0u8; len];
    recv.read_exact(&mut buf)
        .await
        .map_err(|e| DeformError::Connection(e.to_string()))?;
    wincode::deserialize(&buf)
        .map_err(|e| DeformError::Deserialize(format!("control message: {e:?}")))
}

pub fn new_quic_client<T: DeformUserLogic>(
    server_addr: String,
    server_name: String,
    lobby_id: u64,
    player: Pubkey,
    players: HashSet<Pubkey>,
    sig: Signature,
    skip_cert_verify: bool,
    visual_tick_micros: u64,
) -> DeformResult<DeformClient<T>> {
    client::QuicBackend::<T>::init(
        server_addr,
        server_name,
        lobby_id,
        player,
        players,
        sig,
        skip_cert_verify,
        visual_tick_micros,
    )
}
