//! Fully-on-chain (FoC) backend for DeForm.
//!
//! Where [`deform_quic`](../deform_quic) talks to a Web2 authoritative server over
//! QUIC, this backend treats a MagicBlock **ephemeral rollup** as the authority:
//!
//! - **reading state** — subscribe to the lobby account over WebSocket
//!   (`accountSubscribe`); each on-chain crank `tick` pushes a new `Lobby`.
//! - **committing inputs** — send a `set_inputs` transaction to the ER over HTTP,
//!   signed with the player's keypair.
//!
//! The client-side prediction/rollback loop is the same idea as the QUIC backend;
//! only the transport and the latency estimate differ (see [`backend`]). The game
//! engine still only sees a backend-agnostic [`DeformClient`].

use std::{
    collections::{BTreeMap, HashMap},
    fmt::Debug,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use deform_core::{
    DeformClient, DeformError, DeformResult, DeformSharedBackendState, DeformUserLogic, Pubkey,
    Smooth,
    accounts::{
        inputs::InputsAccount,
        lobby::{Lobby, LobbyState, ongoing::LobbyOngoing},
    },
    error::{UserFacingError, UserFacingResult},
    game_program_client::GameProgramClient,
};
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::{message::Instruction, signature::Keypair, signer::Signer};
use tokio::sync::{Notify, mpsc, oneshot};

mod client;
mod rtt;
mod ws;

use client::FocBackend;

// Exactly one latency strategy must be enabled (rtt-getslot is the default).
const _: () = assert!(
    cfg!(feature = "rtt-getslot") as u8
        + cfg!(feature = "rtt-ping") as u8
        + cfg!(feature = "rtt-inputs") as u8
        == 1,
    "deform_foc: enable exactly one latency feature (rtt-getslot, rtt-ping, or rtt-inputs); \
     to switch off the default rtt-getslot, disable default features",
);

/// Ties a game's [`DeformUserLogic`] to the [`GameProgramClient`] that builds its
/// on-chain instructions. The FoC analogue of `DeformQuicLogic`, minus the Web2
/// server concepts (auth, custom reliable messages).
pub trait DeformFocLogic: Clone + Sized + Debug + Send + Sync + 'static {
    type UserLogic: DeformUserLogic;
    type ProgramClient: GameProgramClient<Self::UserLogic>;
}

/// How often the latency probe samples RTT and the backend re-derives how far ahead
/// of the on-chain tick the local simulation should run.
pub const RTT_SAMPLE_INTERVAL_MS: u64 = 500;

pub fn new_foc_client<F: DeformFocLogic>(
    rpc_url: String,
    ws_url: String,
    keypair: Arc<Keypair>,
    program_client: F::ProgramClient,
    lobby: Lobby<F::UserLogic>,
    visual_tick_micros: u64,
    slot_time_micros: u64,
) -> UserFacingResult<F::UserLogic, DeformClient<F::UserLogic>> {
    let player = Pubkey::new_from_array(keypair.pubkey().to_bytes());
    let game_program = program_client.game_program();
    let lobby_id = lobby.metadata.id;
    let (lobby_pda, _) = Lobby::<F::UserLogic>::find_program_address(lobby_id, &game_program);
    let (inputs_pda, _) =
        InputsAccount::<F::UserLogic>::find_program_address(lobby_id, &player, &game_program);

    // Same bootstrap as the QUIC backend: a NotStarted lobby is promoted to a fresh
    // Ongoing state at tick 0 so we have something to predict from until the first
    // on-chain update fast-forwards us.
    let (lobby, user_logic, starting_tick_info) = match &lobby.state {
        LobbyState::Finished(_) => Err(DeformError::InvalidState("Game already ended!".into()))?,
        LobbyState::NotStarted(not_started) => {
            let mut inputs = BTreeMap::new();
            for player in not_started.player_status.keys() {
                inputs.insert(
                    *player,
                    <F::UserLogic as DeformUserLogic>::Inputs::default(),
                );
            }

            let user_logic =
                <F::UserLogic as DeformUserLogic>::new_from_lobby(&lobby.metadata, not_started)
                    .map_err(UserFacingError::User)?;
            let game_state = <F::UserLogic as DeformUserLogic>::new_game_from_lobby(
                &lobby.metadata,
                not_started,
            )
            .map_err(UserFacingError::User)?;
            let tick_info = deform_core::TickInfo { game_state, inputs };

            (
                Lobby {
                    metadata: lobby.metadata.clone(),
                    state: LobbyState::Ongoing(LobbyOngoing {
                        slot: None,
                        tick: 0,
                        tick_info: tick_info.clone(),
                        user_logic: user_logic.clone(),
                    }),
                },
                user_logic,
                tick_info,
            )
        }
        LobbyState::Ongoing(state) => (
            lobby.clone(),
            state.user_logic.clone(),
            state.tick_info.clone(),
        ),
    };

    let terminate = Arc::new(Notify::new());
    let (set_inputs_sender, set_inputs_receiver) =
        mpsc::unbounded_channel::<<F::UserLogic as DeformUserLogic>::Inputs>();
    let backend_state = Arc::new(std::sync::Mutex::new(DeformSharedBackendState::<
        F::UserLogic,
    >::new_from_lobby(lobby.clone())?));
    let backend_dead = Arc::new(AtomicBool::new(false));

    let (setup_tx, setup_rx) = oneshot::channel::<DeformResult>();

    let terminate_clone = terminate.clone();
    let backend_state_clone = backend_state.clone();
    let backend_dead_clone = backend_dead.clone();

    let _rss_thread = thread::spawn(move || {
        let rt = match tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(e) => {
                let _ = setup_tx.send(Err(DeformError::Connection(format!(
                    "failed to build tokio runtime: {e}"
                ))));
                return;
            }
        };

        rt.block_on(async move {
            let rpc = Arc::new(RpcClient::new(rpc_url));

            // WebSocket: decoded lobby states flow in on `state_rx`. If the
            // subscription can't be established, fail setup.
            let (state_tx, state_rx) = mpsc::unbounded_channel::<LobbyState<F::UserLogic>>();
            let rtt_micros = Arc::new(AtomicU64::new(Duration::from_millis(50).as_micros() as u64));

            let (ws_ready_tx, ws_ready_rx) = oneshot::channel::<DeformResult>();
            tokio::spawn(ws::ws_task::<F::UserLogic>(
                ws_url.clone(),
                lobby_pda,
                state_tx,
                terminate_clone.clone(),
                ws_ready_tx,
            ));
            match ws_ready_rx.await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    let _ = setup_tx.send(Err(e));
                    return;
                }
                Err(_) => {
                    let _ = setup_tx.send(Err(DeformError::Connection(
                        "websocket task terminated before it was ready".into(),
                    )));
                    return;
                }
            }

            // --- latency probe: exactly one, selected by feature ---
            #[cfg(feature = "rtt-getslot")]
            tokio::spawn(rtt::getslot_task(
                rpc.clone(),
                rtt_micros.clone(),
                terminate_clone.clone(),
            ));

            #[cfg(feature = "rtt-ping")]
            tokio::spawn(rtt::ping_task(
                ws_url.clone(),
                rtt_micros.clone(),
                terminate_clone.clone(),
            ));

            // The end-to-end probe reads the sim's per-commit send times.
            #[cfg(feature = "rtt-inputs")]
            let commit_times = Arc::new(std::sync::Mutex::new(std::collections::BTreeMap::new()));
            #[cfg(feature = "rtt-inputs")]
            tokio::spawn(rtt::inputs_rtt_task::<F::UserLogic>(
                ws_url.clone(),
                inputs_pda,
                commit_times.clone(),
                rtt_micros.clone(),
                terminate_clone.clone(),
            ));

            // The commit task owns the RPC send path; the bounded channel backpressures
            // the sim loop if sends fall behind.
            // TODO: do not hardcode 64 here
            let (commit_tx, commit_rx) = mpsc::channel::<Instruction>(64);
            tokio::spawn(client::commit_task(
                rpc,
                keypair,
                commit_rx,
                slot_time_micros,
            ));

            let _ = setup_tx.send(Ok(()));

            let smoother = {
                let mut s = <F::UserLogic as DeformUserLogic>::Smoother::default();
                let decay_ratio = visual_tick_micros as f32
                    / <F::UserLogic as DeformUserLogic>::TICK_RATE_MICROS as f32;
                s.scale_decay(decay_ratio);
                s
            };

            let backend = FocBackend::<F> {
                local_tick: 0,
                info_per_tick: HashMap::new(),
                remote_lobby: lobby,
                inputs: HashMap::new(),

                player,
                lobby_pda,
                inputs_pda,
                lobby_id,

                program_client,
                commit_tx,
                #[cfg(feature = "rtt-inputs")]
                commit_times,

                state_rx,
                rtt_micros,

                terminate: terminate_clone.clone(),
                set_inputs_receiver,
                backend_state: backend_state_clone.clone(),
                user_logic,

                smoother,
                visual_tick_micros,
                slot_time_micros,
                last_sim_instant: Instant::now(),
                next_tick_deadline: tokio::time::Instant::now(),

                avg_rtt: Duration::from_millis(50),
                min_ticks_ahead: 4,
                max_ticks_ahead: 12,
            };

            if let Err(e) = backend.tick_loop(starting_tick_info).await
                && let Ok(mut shared) = backend_state_clone.lock()
            {
                shared.internal_error = Err(e);
            }
        });

        backend_dead_clone.store(true, Ordering::Relaxed);
    });

    setup_rx
        .blocking_recv()
        .map_err(|_| DeformError::Connection("setup thread terminated unexpectedly".into()))??;

    Ok(DeformClient {
        terminate,
        set_inputs_sender,
        backend_state,
        backend_dead,
    })
}
