use std::collections::HashMap;

use deform_core::Pubkey;
use deform_core::{
    DeformClient, DeformGameState, DeformInputs, DeformResult, DeformUserLogic, lobby::Lobby,
};
use solana_sdk::signature::Signature;
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
    NewState(Lobby<I, G>),
}

pub fn new_quic_client<T: DeformUserLogic>(
    rpc_url: String,
    server_addr: String,
    server_name: String,
    lobby_id: u64,
    player: Pubkey,
    game_program: Pubkey,
    sig: Signature,
    skip_cert_verify: bool,
) -> DeformResult<DeformClient<T>> {
    client::QuicBackend::<T>::init(
        rpc_url,
        server_addr,
        server_name,
        lobby_id,
        player,
        game_program,
        sig,
        skip_cert_verify,
    )
}
