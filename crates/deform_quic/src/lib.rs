use std::collections::{HashMap, HashSet};
use std::fmt::{Debug, Display};

use deform_core::Pubkey;
use deform_core::{
    DeformClient, DeformError, DeformInputs, DeformResult, DeformUserLogic, lobby::LobbyData,
};
use solana_signature::Signature;
use wincode::config::DefaultConfig;
use wincode::{SchemaRead, SchemaWrite};

mod client;
mod server;

pub const ALPN_PROTOCOL: &[u8] = b"deform/1";

wincode::pod_wrapper! {
    unsafe struct PodPubkey(Pubkey);
    unsafe struct PodSignature(Signature);
}

/// Trait that defines what data types the server, as well as the logic functions/callbacks.
// has to be debug for printing reliable messages (wtf)
pub trait DeformQuicLogic: Debug + Send + 'static {
    type CustomReliableMessage: for<'de> SchemaRead<'de, DefaultConfig, Dst = Self::CustomReliableMessage>
        + SchemaWrite<DefaultConfig, Src = Self::CustomReliableMessage>
        + Clone
        + Debug
        + Send;

    type Auth: for<'de> SchemaRead<'de, DefaultConfig, Dst = Self::Auth>
        + SchemaWrite<DefaultConfig, Src = Self::Auth>
        + Clone
        + Debug
        + Send;

    type Error: for<'de> SchemaRead<'de, DefaultConfig, Dst = Self::Error>
        + SchemaWrite<DefaultConfig, Src = Self::Error>
        + Clone
        + Debug
        + Display
        + Send;

    // https://github.com/rust-lang/rust/issues/29661
    // type Result<T = ()> = Result<T, Self::Error>;

    /// Function that is wired in when the client connects to the server.
    /// Meaning of different return types:
    /// Err -> send an error message to the client and close the connection
    /// None -> ignore this message and wait for the next one
    /// Some(msg) -> send `msg` to the client and approve the client
    fn authorize_connection<D: DeformQuicLogic>(
        identification: UserIdentification<D>,
    ) -> Result<Option<ReliableMessage<D>>, Self::Error>;
}

// TODO: user might want custom information here. make this an associated type instead?
#[derive(Clone, SchemaRead, SchemaWrite, Debug)]
pub struct UserIdentification<D: DeformQuicLogic> {
    pub pubkey: Pubkey,
    pub lobby_id: u64,
    pub auth: D::Auth,
}

/// Messages on the reliable control stream (the bi-directional stream
/// opened during auth and kept open for the lifetime of the connection).
#[derive(Clone, SchemaRead, SchemaWrite, Debug)]
pub enum ReliableMessage<D: DeformQuicLogic> {
    Identification(UserIdentification<D>),
    Authorized,
    Finish,
    Custom(D::CustomReliableMessage),
    Error(D::Error),
}

#[derive(Clone, SchemaRead, SchemaWrite)]
pub enum UnreliableServerInstruction<I: DeformInputs> {
    BatchSetInputs(HashMap<u64, I>),
}

#[repr(transparent)]
#[derive(Clone, Debug, SchemaRead, SchemaWrite)]
pub struct SerializedUnreliableServerResponse(pub Vec<u8>);

#[derive(Clone, SchemaRead, SchemaWrite)]
pub struct UnreliableServerResponse<T: DeformUserLogic> {
    pub lobby_info: LobbyData<T>,
}

const MAX_CONTROL_MSG_SIZE: usize = 4096;

impl<D: DeformQuicLogic> ReliableMessage<D> {
    pub async fn write(send: &mut quinn::SendStream, msg: &Self) -> DeformResult {
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

pub fn new_quic_client<T: DeformUserLogic, D: DeformQuicLogic>(
    server_addr: String,
    server_name: String,
    lobby_id: u64,
    player: Pubkey,
    players: HashSet<Pubkey>,
    skip_cert_verify: bool,
    visual_tick_micros: u64,
    auth: D::Auth,
) -> DeformResult<DeformClient<T>> {
    client::QuicBackend::<T, D>::init(
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
