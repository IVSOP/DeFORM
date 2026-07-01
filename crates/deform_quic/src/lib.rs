use std::collections::{HashMap, HashSet};
use std::fmt::Debug;

use deform_core::Pubkey;
use deform_core::error::UserFacingError;
use deform_core::{
    DeformClient, DeformError, DeformInputs, DeformResult, DeformUserLogic, accounts::lobby::Lobby,
};
use solana_signature::Signature;
use wincode::config::DefaultConfig;
use wincode::{SchemaRead, SchemaWrite};

mod client;
pub mod server;

pub const ALPN_PROTOCOL: &[u8] = b"deform/1";

wincode::pod_wrapper! {
    unsafe struct PodPubkey(Pubkey);
    unsafe struct PodSignature(Signature);
}

/// Trait that defines what data types the server, as well as the logic functions/callbacks.
// has to be debug for printing reliable messages (wtf)
pub trait DeformQuicLogic: Clone + Sized + Debug + Send + Sync + 'static {
    type CustomReliableMessage: for<'de> SchemaRead<'de, DefaultConfig, Dst = Self::CustomReliableMessage>
        + SchemaWrite<DefaultConfig, Src = Self::CustomReliableMessage>
        + Clone
        + Debug
        + Send
        + Sync;

    type Auth: for<'de> SchemaRead<'de, DefaultConfig, Dst = Self::Auth>
        + SchemaWrite<DefaultConfig, Src = Self::Auth>
        + Clone
        + Debug
        + Send
        + Sync;

    // this looks messy at first but is actually the cleaner solution (that I know of)
    // I wanted to avoid user having to specify <Q, U> and also <Q<U>> looks very messy in the structs below
    // and this makes it so each server logic has a specific user logic associated
    type UserLogic: DeformUserLogic;

    // https://github.com/rust-lang/rust/issues/29661
    // type Result<T = ()> = Result<T, Self::Error>;

    /// Function that is wired in when the client connects to the server.
    ///
    /// - Err -> send an error message to the client and close the connection
    /// - Ok(()) -> ReliableMessage::Authorized will be sent.
    // TODO: use different error types??
    fn authorize_connection(
        identification: &UserIdentification<Self>,
    ) -> Result<(), <Self::UserLogic as DeformUserLogic>::Error>;
}

// TODO: user might want custom information here. make this an associated type instead?
#[derive(Clone, SchemaRead, SchemaWrite, Debug)]
pub struct UserIdentification<Q: DeformQuicLogic> {
    pub user: Pubkey,
    pub lobby_id: u64,
    pub auth: Q::Auth,
}

/// Messages on the reliable control stream (the bi-directional stream
/// opened during auth and kept open for the lifetime of the connection).
#[derive(Clone, SchemaRead, SchemaWrite, Debug)]
pub enum ReliableMessage<Q: DeformQuicLogic> {
    Identification(UserIdentification<Q>),
    Authorized,
    Finish,
    Custom(Q::CustomReliableMessage),
    Error(UserFacingError<Q::UserLogic>),
}

#[derive(Clone, SchemaRead, SchemaWrite)]
pub enum UnreliableServerInstruction<I: DeformInputs> {
    BatchSetInputs(HashMap<u64, I>),
}

// FIX: change to Bytes that quinn uses?? or at least change to Arc??
#[repr(transparent)]
#[derive(Clone, Debug, SchemaRead, SchemaWrite)]
pub struct SerializedUnreliableServerResponse(pub Vec<u8>);

#[derive(Clone, SchemaRead, SchemaWrite)]
pub struct UnreliableServerResponse<T: DeformUserLogic> {
    pub lobby_info: Lobby<T>,
}

const MAX_CONTROL_MSG_SIZE: usize = 4096;

impl<Q: DeformQuicLogic> ReliableMessage<Q> {
    pub async fn write(&self, send: &mut quinn::SendStream) -> DeformResult {
        let data = wincode::serialize(self)
            .map_err(|e| DeformError::Serialize(format!("control message: {e:?}")))?;
        send.write_all(&(data.len() as u32).to_le_bytes())
            .await
            .map_err(|e| DeformError::Connection(e.to_string()))?;
        send.write_all(data.as_slice())
            .await
            .map_err(|e| DeformError::Connection(e.to_string()))?;
        Ok(())
    }

    pub async fn read(recv: &mut quinn::RecvStream) -> DeformResult<Self> {
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
}

pub fn new_quic_client<Q: DeformQuicLogic>(
    server_addr: String,
    server_name: String,
    lobby_id: u64,
    player: Pubkey,
    players: HashSet<Pubkey>,
    skip_cert_verify: bool,
    visual_tick_micros: u64,
    auth: Q::Auth,
) -> DeformResult<DeformClient<Q::UserLogic>> {
    client::QuicBackend::<Q>::init(
        server_addr,
        server_name,
        lobby_id,
        player,
        players,
        skip_cert_verify,
        visual_tick_micros,
        auth,
    )
}
