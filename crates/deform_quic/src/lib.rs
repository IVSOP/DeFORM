use std::{collections::HashMap, fmt::Debug, future::Future, time::Duration};

use deform_core::{
    DeformClient, DeformError, DeformInputs, DeformResult, DeformUserLogic, Pubkey,
    accounts::lobby::Lobby,
    error::{UserFacingError, UserFacingResult},
    game_program_client::GameProgramClient,
};
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::{
    message::{AccountMeta, Instruction, Message},
    signature::Keypair,
    signer::Signer,
    transaction::Transaction,
};
use solana_signature::Signature;
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};
use wincode::{SchemaRead, SchemaWrite, config::DefaultConfig};

mod client;
pub mod datagram;
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
    type ProgramClient: GameProgramClient<Self::UserLogic>;

    /// zstd level for datagram bodies
    const COMPRESSION: i32 = 10;

    /// Maximum time-dilation rate, as a fraction of the base tick rate.
    /// 0.10 means the client runs at most 10% faster to refill the server's input
    /// buffer. Slowing down is capped at half of this since being early only costs lag and not a missprediction
    const TIME_DILATION: f32 = 0.10;

    /// Extra inputs to keep queued on the server beyond the one it consumes each tick as margin for jitter
    const JITTER_SLACK: f32 = 1.5;

    /// Maximum number of incomplete messages that we buffer
    const MAX_MESSAGE_BUFFER: u8 = 32;

    /// Maximum number of fragments per incomplete message
    const MAX_FRAGMENTS: u8 = 64;

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

    /// Called after the game ends (when [`deform_core::DeformGameState::has_ended`] returns true) but before
    /// the match is removed
    ///
    /// The default implementation calls [`GameProgramClient::write_and_close_ix`] and sends the
    /// transaction in a retry loop of 10 attempts until it is confirmed.
    ///
    /// - Ok(()) -> the match is finalized and removed
    /// - Err -> the error is broadcast to clients, and the match is removed
    fn on_match_end(
        &self,
        lobby: &Lobby<Self::UserLogic>,
        rpc_client: &RpcClient,
        admin: &Keypair,
        program_client: &Self::ProgramClient,
    ) -> impl Future<Output = UserFacingResult<Self::UserLogic>> + Send {
        let admin_pubkey = admin.pubkey();

        let (lobby_pda, _) = Lobby::<Self::UserLogic>::find_program_address(
            lobby.metadata.id,
            &program_client.game_program(),
        );

        let ix = program_client.write_and_close_ix(
            admin_pubkey,
            lobby_pda,
            lobby.metadata.creator,
            lobby,
        );

        async move {
            let ix = ix.map_err(|e| UserFacingError::User(e))?;

            let sdk_ix = Instruction {
                program_id: Pubkey::new_from_array(ix.program_id.to_bytes()),
                accounts: ix
                    .accounts
                    .iter()
                    .map(|a| AccountMeta {
                        pubkey: Pubkey::new_from_array(a.pubkey.to_bytes()),
                        is_signer: a.is_signer,
                        is_writable: a.is_writable,
                    })
                    .collect(),
                data: ix.data,
            };

            let mut last_err = None;

            for attempt in 1..=10u32 {
                let blockhash = rpc_client.get_latest_blockhash().await.map_err(|e| {
                    DeformError::Rpc(format!("get blockhash (attempt {attempt}/10): {e}"))
                })?;

                let msg = Message::new(&[sdk_ix.clone()], Some(&admin_pubkey));
                let mut tx = Transaction::new_unsigned(msg);
                tx.sign(&[admin], blockhash);

                match rpc_client.send_and_confirm_transaction(&tx).await {
                    Ok(sig) => {
                        info!(%sig, "write_and_close confirmed");
                        return Ok(());
                    }
                    Err(e) => {
                        warn!(attempt, "write_and_close failed: {e}");
                        last_err = Some(e);
                        sleep(Duration::from_millis(400)).await;
                    }
                }
            }

            Err(UserFacingError::Deform(
                DeformError::Rpc(format!(
                    "write_and_close failed after 10 attempts: {}",
                    last_err.unwrap()
                ))
                .into(),
            ))
        }
    }
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
    Finish(Lobby<Q::UserLogic>),
    Custom(Q::CustomReliableMessage),
    Error(UserFacingError<Q::UserLogic>),
}

#[derive(Clone, SchemaRead, SchemaWrite)]
pub enum ServerInstruction<I: DeformInputs> {
    BatchSetInputs(HashMap<u64, I>),
}

/// Type actually sent over the wire. The entire packet is not compressed, only the state update.
/// This is because that update will be common to all players so this saves compressing multiple times.
#[derive(Clone, Debug, SchemaRead, SchemaWrite)]
pub struct StateUpdatePacket {
    // FIX: change to Bytes that quinn uses??
    /// A compressed [`LobbyState`]
    pub lobby_state: Compressed,
    /// Tells the user how many inputs are currently in its input buffer
    pub player_input_buffer_len: u8,
}

#[derive(Clone, Debug, SchemaRead, SchemaWrite)]
#[repr(transparent)]
pub struct Compressed(pub Vec<u8>);

impl Compressed {
    pub fn compress(bytes: &[u8], level: i32) -> DeformResult<Self> {
        let compressed = zstd::stream::encode_all(bytes, level)
            .map_err(|e| DeformError::Serialize(format!("compress datagram: {e}")))?;

        #[cfg(feature = "metrics")]
        deform_metrics::plot!(
            "compression_ratio",
            bytes.len() as f64 / compressed.len().max(1) as f64
        );

        // What actually has to fit the MTU, so this is the number to watch alongside
        // `datagram_fragments`.
        #[cfg(feature = "metrics")]
        deform_metrics::plot!("datagram_body_bytes", compressed.len() as f64);

        Ok(Compressed(compressed))
    }

    pub fn decompress(&self) -> DeformResult<Vec<u8>> {
        zstd::stream::decode_all(self.0.as_slice())
            .map_err(|e| DeformError::Deserialize(format!("decompress datagram: {e}")))
    }
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
    lobby: Lobby<Q::UserLogic>,
    player: Pubkey,
    skip_cert_verify: bool,
    visual_tick_micros: u64,
    auth: Q::Auth,
    cancellation_token: CancellationToken,
) -> UserFacingResult<Q::UserLogic, DeformClient<Q::UserLogic>> {
    client::QuicBackend::<Q>::init(
        server_addr,
        server_name,
        lobby,
        player,
        skip_cert_verify,
        visual_tick_micros,
        auth,
        cancellation_token,
    )
}
