use std::collections::{HashMap, HashSet};

use deform_core::Pubkey;
use deform_core::{
    DeformClient, DeformGameState, DeformInputs, DeformResult, DeformUserLogic, lobby::LobbyData,
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

pub fn new_quic_client<T: DeformUserLogic>(
    server_addr: String,
    server_name: String,
    lobby_id: u64,
    player: Pubkey,
    players: HashSet<Pubkey>,
    sig: Signature,
    skip_cert_verify: bool,
) -> DeformResult<DeformClient<T>> {
    client::QuicBackend::<T>::init(
        server_addr,
        server_name,
        lobby_id,
        player,
        players,
        sig,
        skip_cert_verify,
    )
}
