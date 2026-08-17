use std::{
    collections::{BTreeMap, HashMap},
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use better_tokio_select::tokio_select;
use deform_core::{
    DeformClient, DeformError, DeformInputs, DeformResult, DeformSharedBackendState,
    DeformUserLogic, Pubkey, Smooth, TickInfo,
    accounts::lobby::{Lobby, LobbyFinished, LobbyState, ongoing::LobbyOngoing},
    error::{UserFacingError, UserFacingResult},
    game_program_client::GameProgramClient,
};
use glam::FloatExt;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::{
    message::{AccountMeta, Instruction, Message},
    signature::Keypair,
    signer::Signer,
    transaction::Transaction,
};
use tokio::{
    sync::mpsc,
    time::{Sleep, interval, sleep_until},
};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, warn};

use crate::{DeformFocLogic, RTT_SAMPLE_INTERVAL_MS};

pub struct FocBackend<F: DeformFocLogic> {
    pub local_tick: u64,
    pub info_per_tick: HashMap<u64, TickInfo<F::UserLogic>>,
    pub remote_lobby: Lobby<F::UserLogic>,
    /// Inputs from our own player, keyed by the tick they apply to.
    pub inputs: HashMap<u64, <F::UserLogic as DeformUserLogic>::Inputs>,

    pub player: Pubkey,
    pub lobby_pda: Pubkey,
    pub inputs_pda: Pubkey,
    pub lobby_id: u64,

    pub program_client: F::ProgramClient,
    /// Built `set_inputs` instructions are handed to the [`commit_task`], which owns
    /// the RPC send path. The channel is bounded, so a slow commit task backpressures
    /// the sim loop rather than piling up unbounded work.
    pub commit_tx: mpsc::Sender<Instruction>,
    /// Per-commit send times (batch max tick -> send instant), read by the
    /// inputs-account RTT probe to measure true end-to-end latency.
    #[cfg(feature = "rtt-inputs")]
    pub commit_times: Arc<Mutex<BTreeMap<u64, Instant>>>,

    /// Authoritative lobby states decoded from `accountSubscribe`.
    pub state_rx: mpsc::UnboundedReceiver<LobbyState<F::UserLogic>>,
    /// Latest WebSocket ping RTT, in microseconds, published by the WS task.
    pub rtt_micros: Arc<AtomicU64>,

    pub cancellation_token: CancellationToken,
    pub set_inputs_receiver: mpsc::UnboundedReceiver<<F::UserLogic as DeformUserLogic>::Inputs>,
    pub backend_state: Arc<Mutex<DeformSharedBackendState<F::UserLogic>>>,
    pub user_logic: F::UserLogic,

    pub smoother: <F::UserLogic as DeformUserLogic>::Smoother,
    pub visual_tick_micros: u64,
    /// The ER's slot/block time in micros, provided by the caller (matching on-chain
    /// `get_micros_per_slot`). Drives the commit cadence (~one tx per slot) and is the
    /// inclusion-latency floor folded into the ticks-ahead target.
    pub slot_time_micros: u64,
    pub last_sim_instant: Instant,
    /// Absolute deadline for the next simulation tick, anchored to the previous
    /// deadline so per-tick work and jitter don't accumulate as drift.
    pub next_tick_deadline: tokio::time::Instant,

    pub avg_rtt: Duration,
    /// If ticks are below this, the simulation fast-forwards. Also drives time dilation.
    pub min_ticks_ahead: u64,
    /// If ticks are above this, the simulation stops; hard limit, always at least 5.
    pub max_ticks_ahead: u64,
}

impl<F: DeformFocLogic> FocBackend<F> {
    /// How many game ticks are batched into a single `set_inputs` transaction: as many
    /// as fit in one ER slot, at least one. Committing every tick is not viable here,
    /// so this is also the worst-case wait an input can sit through before being sent,
    /// which is why [`Self::update_ticks_ahead`] folds it into the ticks-ahead target.
    fn commit_interval_ticks(&self) -> u64 {
        self.slot_time_micros
            .div_ceil(<F::UserLogic as DeformUserLogic>::TICK_RATE_MICROS)
            .max(1)
    }

    pub async fn tick_loop(
        mut self,
        starting_tick_info: TickInfo<F::UserLogic>,
    ) -> UserFacingResult<F::UserLogic> {
        self.info_per_tick.insert(0, starting_tick_info);

        self.next_tick_deadline = tokio::time::Instant::now()
            + Duration::from_micros(<F::UserLogic as DeformUserLogic>::TICK_RATE_MICROS);
        let mut tick_sleep = Box::pin(sleep_until(self.next_tick_deadline));
        let mut visual_ticker = interval(Duration::from_micros(self.visual_tick_micros));
        // Each commit carries the full accumulated input batch, so committing less
        // often loses no inputs, it only adds a little commit latency.
        let tick_micros = <F::UserLogic as DeformUserLogic>::TICK_RATE_MICROS;
        let commit_interval_ticks = self.commit_interval_ticks();
        let mut last_commit_tick = self.local_tick;
        let mut last_commit_at = tokio::time::Instant::now();
        let mut rtt_ticker = interval(Duration::from_millis(RTT_SAMPLE_INTERVAL_MS));

        loop {
            tokio_select!(match .. {
                // Simulation tick, possibly dilated to let the chain catch up.
                .. if let _ = &mut tick_sleep => {
                    let remote_tick = match &self.remote_lobby.state {
                        LobbyState::Finished(_) => break,
                        // No authoritative stream to reconcile against yet; hold.
                        LobbyState::NotStarted(_) => break,
                        LobbyState::Ongoing(ongoing) => {
                            let remote_tick = ongoing.tick;
                            let min_target_tick = remote_tick + self.min_ticks_ahead;
                            let current_tick = self.local_tick;

                            if current_tick < min_target_tick {
                                let delta_ticks = min_target_tick - current_tick;
                                for _ in 0..delta_ticks {
                                    self.advance_local_simulation()?
                                }
                                self.last_sim_instant = Instant::now();
                            } else {
                                let max_target_tick = ongoing.tick + self.max_ticks_ahead;
                                if current_tick < max_target_tick {
                                    self.advance_local_simulation()?;
                                    self.last_sim_instant = Instant::now();
                                }
                            }

                            remote_tick
                        }
                    };

                    // Commit once enough new input ticks have piled up, rather than on a
                    // wall-clock timer. Inputs are produced by ticks, so counting ticks keeps
                    // the batch size constant under time dilation; a timer would instead fire
                    // with little new to send and pay for a near-empty transaction.
                    //
                    // The elapsed check is only a backstop: the simulation stops advancing
                    // while frozen at `max_ticks_ahead`, and the tick count alone would then
                    // never reach the threshold again.
                    let ticks_since_commit = self.local_tick.saturating_sub(last_commit_tick);
                    let commit_stalled = last_commit_at.elapsed()
                        > Duration::from_micros(2 * commit_interval_ticks * tick_micros);

                    if ticks_since_commit >= commit_interval_ticks || commit_stalled {
                        #[cfg(feature = "metrics")]
                        if let Some(max_input) = self.inputs.keys().max() {
                            deform_metrics::plot!("commit_inputs", *max_input as f64);
                        }

                        self.commit_inputs().await?;
                        last_commit_tick = self.local_tick;
                        last_commit_at = tokio::time::Instant::now();
                    }

                    let dilated = self.compute_dilated_tick_interval(remote_tick);
                    self.next_tick_deadline += dilated;
                    let now = tokio::time::Instant::now();
                    if self.next_tick_deadline < now {
                        self.next_tick_deadline = now;
                    }
                    tick_sleep.as_mut().reset(self.next_tick_deadline);
                }

                // Visual: interpolate between previous and current sim state.
                .. if let _ = visual_ticker.tick() => {
                    let prev_tick = self.local_tick.saturating_sub(1);

                    if let (Some(prev), Some(current)) = (
                        self.info_per_tick.get(&prev_tick),
                        self.info_per_tick.get(&self.local_tick),
                    ) {
                        let elapsed = self.last_sim_instant.elapsed().as_micros() as f32;
                        let t = (elapsed
                            / <F::UserLogic as DeformUserLogic>::TICK_RATE_MICROS as f32)
                            .clamp(0.0, 1.0);

                        let mut visual_state = current.clone();
                        self.smoother
                            .apply(&prev.game_state, &mut visual_state.game_state, t);

                        {
                            let mut shared = self
                                .backend_state
                                .lock()
                                .map_err(|_| DeformError::LockPoisoned)?;

                            if let LobbyState::Ongoing(ongoing) = &mut shared.lobby.state {
                                ongoing.tick_info = visual_state;
                            }
                        }
                    }
                }

                // Refresh the latency estimate and re-derive the ticks-ahead target.
                .. if let _ = rtt_ticker.tick() => {
                    if !matches!(self.remote_lobby.state, LobbyState::Finished(_)) {
                        self.avg_rtt =
                            Duration::from_micros(self.rtt_micros.load(Ordering::Relaxed));

                        #[cfg(feature = "metrics")]
                        deform_metrics::plot!("RTT", self.avg_rtt.as_secs_f64() * 1000.0);

                        self.update_ticks_ahead()?;
                    }
                }

                // New inputs from the game engine.
                .. if let new_inputs = self.set_inputs_receiver.recv() => {
                    if new_inputs.is_none() {
                        break;
                    }
                    if !matches!(self.remote_lobby.state, LobbyState::Finished(_))
                        && let Some(new_inputs) = new_inputs
                    {
                        // The engine can provide several samples within one tick. Merge them
                        // instead of keeping only the first, which silently dropped the rest.
                        // Safe to mutate in place: this entry is not sent until the tick closes.
                        match self.inputs.get_mut(&self.local_tick) {
                            Some(existing) => existing.merge(&new_inputs),
                            None => {
                                self.inputs.insert(self.local_tick, new_inputs);
                            }
                        }
                    }
                }

                // Authoritative state from the ER (accountSubscribe on the lobby).
                .. if let state = self.state_rx.recv() => {
                    match state {
                        None => break,
                        Some(state) => {
                            if !matches!(self.remote_lobby.state, LobbyState::Finished(_)) {
                                self.process_new_state(state, &mut tick_sleep)?;
                            }
                        }
                    }
                }
                .. if let _ = self.cancellation_token.cancelled() => {
                    break;
                }
            });
        }

        // when leaving, if the game has finished, set the final visual state to the state of the lobby
        if let LobbyState::Finished(mut finished) = self.remote_lobby.state {
            if let Some(tick_info) = self.info_per_tick.get(&self.local_tick) {
                let visual_state = tick_info.clone();
                {
                    let mut shared = self
                        .backend_state
                        .lock()
                        .map_err(|_| DeformError::LockPoisoned)?;

                    finished.0.tick_info = visual_state;
                    shared.lobby.state = LobbyState::Finished(finished);
                }
            }
        }

        #[cfg(feature = "metrics")]
        deform_metrics::flush();

        debug!("foc tick_loop exiting");
        Ok(())
    }

    pub fn update_ticks_ahead(&mut self) -> DeformResult {
        let rtt_secs = self.avg_rtt.as_secs_f64();
        // // getSlot/ping measure network RTT only, so add one ER slot of inclusion. The
        // // inputs-account probe already includes inclusion, so it adds nothing on top.
        // #[cfg(any(feature = "rtt-getslot", feature = "rtt-ping"))]
        // let latency_micros = rtt_secs * 1_000_000.0 + self.slot_time_micros as f64 + 3000.0;
        // #[cfg(feature = "rtt-inputs")]
        // let latency_micros = rtt_secs * 1_000_000.0 + 3000.0;
        //
        // // Inputs are batched, so one for a tick that just missed a commit waits out the
        // // rest of the interval before it is even sent. The lead has to cover that wait,
        // // not just the network latency, or we fall behind and start fast-forwarding.
        // self.min_ticks_ahead = (latency_micros
        //     / <F::UserLogic as DeformUserLogic>::TICK_RATE_MICROS as f64)
        //     .ceil() as u64
        //     + self.commit_interval_ticks();
        // self.max_ticks_ahead = (3 * self.min_ticks_ahead).max(5);

        // Testing: use the same formula as the QUIC backend
        let mut rtt_micros = rtt_secs * 1_000_000.0;
        rtt_micros += 3000.0;
        self.min_ticks_ahead =
            (rtt_micros / <F::UserLogic as DeformUserLogic>::TICK_RATE_MICROS as f64).ceil() as u64
                + 1;
        self.max_ticks_ahead = (3 * self.min_ticks_ahead).max(5);

        #[cfg(feature = "metrics")]
        {
            deform_metrics::plot!("min ticks ahead", self.min_ticks_ahead as f64);
            deform_metrics::plot!("max ticks ahead", self.max_ticks_ahead as f64);
        }

        {
            let mut shared = self
                .backend_state
                .lock()
                .map_err(|_| DeformError::LockPoisoned)?;
            // Report the network ping (not the inclusion-inflated target).
            shared.stats.ping_ms = rtt_secs * 1_000.0;
        }

        Ok(())
    }

    fn compute_dilated_tick_interval(&mut self, remote_tick: u64) -> Duration {
        let base_sleep_ms: f32 =
            <F::UserLogic as DeformUserLogic>::TICK_RATE_MICROS as f32 / 1000.0;
        let mid_sleep_ms: f32 = base_sleep_ms * 1.5;
        let max_sleep_ms: f32 = base_sleep_ms * 4.0;

        let ticks_ahead = self.local_tick.saturating_sub(remote_tick);

        let ahead_over_min = ticks_ahead.saturating_sub(self.min_ticks_ahead) as f32;
        let window = (self.max_ticks_ahead.saturating_sub(self.min_ticks_ahead)).max(1) as f32;
        let ahead_percent = (ahead_over_min / window).max(0.0);

        let sleep_ms = if ahead_percent <= 0.30 {
            base_sleep_ms
        } else if ahead_percent <= 0.60 {
            let t = ((ahead_percent - 0.30) / 0.30).clamp(0.0, 1.0);
            base_sleep_ms.lerp(mid_sleep_ms, t)
        } else {
            let t = ((ahead_percent - 0.60) / 0.40).clamp(0.0, 1.0);
            mid_sleep_ms.lerp(max_sleep_ms, t)
        };

        #[cfg(feature = "metrics")]
        deform_metrics::plot!("sleep_time", sleep_ms as f64);

        Duration::from_micros((sleep_ms * 1000.0) as u64)
    }

    pub fn advance_local_simulation(&mut self) -> UserFacingResult<F::UserLogic> {
        let current_tick = self.local_tick;
        let new_tick = self.local_tick + 1;

        // inneficient but only used in dev
        #[cfg(feature = "metrics")]
        {
            let remote_tick = match &self.remote_lobby.state {
                LobbyState::Ongoing(ongoing) => ongoing.tick,
                LobbyState::Finished(LobbyFinished(finished)) => finished.tick,
                LobbyState::NotStarted(_) => 0,
            };

            deform_metrics::plot!(
                "current_vs_remote_adv",
                self.local_tick as f64 - remote_tick as f64
            );
        }

        let current_info = self
            .info_per_tick
            .get(&current_tick)
            .ok_or(DeformError::InvalidState("slot not found".into()))?;

        // clone the old array so we keep the correct pubkeys; inputs are overwritten
        let mut new_players_inputs: BTreeMap<Pubkey, <F::UserLogic as DeformUserLogic>::Inputs> =
            current_info.inputs.clone();

        for (player, inputs) in new_players_inputs.iter_mut() {
            *inputs = if *player == self.player {
                if let Some(provided_inputs) = self.inputs.get(&current_tick) {
                    provided_inputs.clone()
                } else {
                    inputs.predict()
                }
            } else {
                inputs.predict()
            }
        }

        #[cfg(feature = "metrics")]
        deform_metrics::plot!("advance_sim", new_tick as f64);

        let new_state = self
            .user_logic
            .advance_frame(&current_info.game_state, &new_players_inputs)
            .map_err(UserFacingError::User)?;

        let next_info = TickInfo {
            game_state: new_state,
            inputs: new_players_inputs,
        };

        self.local_tick = new_tick;
        self.info_per_tick.insert(new_tick, next_info);

        // Attribute every subsequent record to the tick it happened on. Rollbacks and
        // fast-forwards re-enter here, so this alone keeps the attribution honest.
        #[cfg(feature = "metrics")]
        deform_metrics::set_tick(new_tick);

        Ok(())
    }

    /// Build the `set_inputs` instruction for our accumulated inputs and hand it to
    /// the [`commit_task`]. Only the (cheap) instruction build happens on the sim
    /// loop; the network send is the task's job. An ix-build error is surfaced
    /// synchronously.
    pub async fn commit_inputs(&self) -> UserFacingResult<F::UserLogic> {
        // Never send the tick still in progress: samples are still being merged into it,
        // and the program commits first-write-wins, so a non-final value would be locked
        // in on-chain while we keep changing ours, guaranteeing a mismatch and a rollback.
        let pending: HashMap<u64, <F::UserLogic as DeformUserLogic>::Inputs> = self
            .inputs
            .iter()
            .filter(|(tick, _)| **tick < self.local_tick)
            .map(|(tick, inputs)| (*tick, inputs.clone()))
            .collect();

        if pending.is_empty() {
            return Ok(());
        }

        let ix = self
            .program_client
            .set_inputs_ix(
                self.player,
                self.inputs_pda,
                self.lobby_pda,
                self.lobby_id,
                &pending,
            )
            .map_err(UserFacingError::User)?;

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

        // Record when this batch (identified by its max tick) was sent, so the
        // inputs-account RTT probe can time when it lands.
        #[cfg(feature = "rtt-inputs")]
        if let Some(&max_tick) = pending.keys().max() {
            if let Ok(mut times) = self.commit_times.lock() {
                times.insert(max_tick, Instant::now());
                // bound memory if the probe stalls (drop oldest)
                while times.len() > 256 {
                    let oldest = *times.keys().next().unwrap();
                    times.remove(&oldest);
                }
            }
        }

        // Bounded channel: this awaits — backpressuring the sim loop — if the commit
        // task has fallen behind. A closed channel just means we're shutting down.
        let _ = self.commit_tx.send(sdk_ix).await;
        Ok(())
    }
    pub fn process_new_state(
        &mut self,
        new_remote_state: LobbyState<F::UserLogic>,
        tick_sleep: &mut Pin<Box<Sleep>>,
    ) -> UserFacingResult<F::UserLogic> {
        // inneficient since the variant is checked below, but this is only used in dev
        #[cfg(feature = "metrics")]
        {
            let new_remote = match &new_remote_state {
                LobbyState::Finished(LobbyFinished(finished_state)) => finished_state.tick,
                LobbyState::NotStarted(_) => 0,
                LobbyState::Ongoing(ongoing) => ongoing.tick,
            };

            deform_metrics::plot!(
                "current_vs_remote_reception",
                self.local_tick as f64 - new_remote as f64
            );
        }

        match new_remote_state {
            LobbyState::Finished(LobbyFinished(ref finished_state)) => {
                let new_remote_tick = finished_state.tick;
                let new_tick_info = finished_state.tick_info.clone();

                self.inputs.clear();
                self.info_per_tick.clear();
                self.info_per_tick.insert(new_remote_tick, new_tick_info);
                self.local_tick = new_remote_tick;
                self.remote_lobby.state = new_remote_state;

                Ok(())
            }
            LobbyState::Ongoing(ongoing) => self.handle_new_ongoing(ongoing, tick_sleep),
            LobbyState::NotStarted(_) => Ok(()),
        }
    }

    pub fn handle_rollback(
        &mut self,
        new_tick_info: TickInfo<F::UserLogic>,
        conflicting_tick: u64,
        tick_sleep: &mut Pin<Box<Sleep>>,
    ) -> UserFacingResult<F::UserLogic> {
        #[cfg(feature = "metrics")]
        deform_metrics::event!(
            "rollback",
            to_tick = conflicting_tick,
            depth = self.local_tick.saturating_sub(conflicting_tick),
        );

        let previous_local_tick = self.local_tick;
        let pre_rollback_info = self
            .info_per_tick
            .remove(&previous_local_tick)
            .ok_or(DeformError::InvalidState("State not found!".into()))?;

        self.info_per_tick.insert(conflicting_tick, new_tick_info);
        self.local_tick = conflicting_tick;

        for _tick in conflicting_tick..previous_local_tick {
            self.advance_local_simulation()?;
        }

        let post_rollback_info = self
            .info_per_tick
            .get(&self.local_tick)
            .ok_or(DeformError::InvalidState("State not found!".into()))?;

        self.smoother.on_rollback(
            &pre_rollback_info.game_state,
            &post_rollback_info.game_state,
        );

        self.user_logic
            .on_rollback(pre_rollback_info, post_rollback_info)
            .map_err(UserFacingError::User)?;

        let dilated = self.compute_dilated_tick_interval(conflicting_tick);
        let new_deadline = tokio::time::Instant::now() + dilated;
        self.next_tick_deadline = new_deadline.min(self.next_tick_deadline);
        tick_sleep.as_mut().reset(self.next_tick_deadline);

        Ok(())
    }

    pub fn handle_new_ongoing(
        &mut self,
        remote_ongoing: LobbyOngoing<F::UserLogic>,
        tick_sleep: &mut Pin<Box<Sleep>>,
    ) -> UserFacingResult<F::UserLogic> {
        let new_remote_tick = remote_ongoing.tick;
        let old_remote_tick = match &self.remote_lobby.state {
            LobbyState::Ongoing(old_ongoing) => old_ongoing.tick,
            _ => Err(DeformError::InvalidState(
                "Previous lobby was not Ongoing".into(),
            ))?,
        };
        let new_tick_info = remote_ongoing.tick_info.clone();

        #[cfg(feature = "metrics")]
        deform_metrics::plot!("last_tick_slot", new_remote_tick as f64);

        #[derive(Clone, Copy)]
        enum ReceivedScenario {
            OldOrRepeated,
            FastForward,
            Gap,
            Rollback,
            Default,
        }

        let scenario = if old_remote_tick >= new_remote_tick {
            ReceivedScenario::OldOrRepeated
        } else if new_remote_tick > self.local_tick {
            ReceivedScenario::FastForward
        } else if new_remote_tick > old_remote_tick + 1 {
            ReceivedScenario::Gap
        } else {
            let predicted_inputs = &self
                .info_per_tick
                .get(&new_remote_tick)
                .ok_or(DeformError::InvalidState(
                    "remote tick has not been predicted".into(),
                ))?
                .inputs;

            let remote_inputs = &new_tick_info.inputs;

            let mut mismatch = false;
            for (player, predicted_input) in predicted_inputs.iter() {
                let remote_input = remote_inputs.get(player).ok_or(DeformError::InvalidState(
                    "player not found in remote inputs".into(),
                ))?;

                if remote_input != predicted_input {
                    mismatch = true;
                    break;
                }
            }

            if mismatch {
                ReceivedScenario::Rollback
            } else {
                ReceivedScenario::Default
            }
        };

        match scenario {
            ReceivedScenario::OldOrRepeated => {
                return Ok(());
            }
            ReceivedScenario::FastForward => {
                let last_computed_state =
                    self.info_per_tick
                        .get(&self.local_tick)
                        .ok_or(DeformError::InvalidState(
                            "Local state not found, wtf".into(),
                        ))?;

                self.user_logic
                    .on_fast_forward(last_computed_state, &new_tick_info)
                    .map_err(UserFacingError::User)?;

                self.smoother.reset();
                self.local_tick = new_remote_tick;
                self.info_per_tick.clear();
                self.info_per_tick.insert(new_remote_tick, new_tick_info);
                self.remote_lobby.state = LobbyState::Ongoing(remote_ongoing);
                self.inputs.clear();

                self.next_tick_deadline = tokio::time::Instant::now();
                tick_sleep.as_mut().reset(self.next_tick_deadline);

                return Ok(());
            }
            _ => {}
        }

        self.remote_lobby.state = LobbyState::Ongoing(remote_ongoing);

        // prune local inputs older than the new remote tick
        self.inputs.retain(|tick, _| *tick >= new_remote_tick);

        #[cfg(feature = "metrics")]
        deform_metrics::plot!("remote_tick (clean)", new_remote_tick as f64);

        match scenario {
            ReceivedScenario::Gap => {
                let old_remote_state =
                    self.info_per_tick
                        .get(&old_remote_tick)
                        .ok_or(DeformError::InvalidState(
                            "Remote state not found, wtf".into(),
                        ))?;

                self.user_logic
                    .on_gap(old_remote_state, &new_tick_info)
                    .map_err(UserFacingError::User)?;

                self.handle_rollback(new_tick_info, new_remote_tick, tick_sleep)?;
            }
            ReceivedScenario::Rollback => {
                self.handle_rollback(new_tick_info, new_remote_tick, tick_sleep)?;
            }
            ReceivedScenario::Default => {}
            _ => unreachable!(),
        }

        for slot in old_remote_tick..new_remote_tick {
            self.info_per_tick.remove(&slot);
        }

        Ok(())
    }

    pub async fn commit_task_wrapper(
        rpc: Arc<RpcClient>,
        keypair: Arc<Keypair>,
        rx: mpsc::Receiver<Instruction>,
        slot_time_micros: u64,
        client: DeformClient<F::UserLogic>,
    ) {
        if let Err(e) = commit_task(
            rpc,
            keypair,
            rx,
            slot_time_micros,
            client.cancellation_token.clone(),
        )
        .await
        {
            // Cancelling here tears down the sim loop, which drops `set_inputs_receiver`
            // and surfaces to the game as an opaque "channel closed". Log the real cause.
            error!("foc commit task died, ending the match: {e}");
            if let Ok(mut backend) = client.backend_state.lock() {
                backend.internal_error = Err(UserFacingError::Deform(
                    DeformError::CommitInputsError(e.to_string()),
                ));
            }
            client.cancellation_token.cancel();
        }
    }
}

// task that runs in a loop sending transactions
// takes care of refreshing the blockhash on its own as well
pub async fn commit_task(
    rpc: Arc<RpcClient>,
    keypair: Arc<Keypair>,
    mut rx: mpsc::Receiver<Instruction>,
    slot_time_micros: u64,
    cancellation_token: CancellationToken,
) -> anyhow::Result<()> {
    let mut blockhash = rpc.get_latest_blockhash().await?;

    let mut blockhash_refresh_interval = interval(Duration::from_micros(slot_time_micros * 100));

    let pubkey = keypair.pubkey();

    loop {
        tokio_select!(match .. {
            .. if let _ = blockhash_refresh_interval.tick() => {
                // A transient RPC blip must not end the match. Blockhashes stay valid for
                // ~2 minutes, so keeping the current one until the next refresh is fine.
                match rpc.get_latest_blockhash().await {
                    Ok(new_blockhash) => blockhash = new_blockhash,
                    Err(e) => warn!("blockhash refresh failed, keeping the previous one: {e}"),
                }
            }
            .. if let ix = rx.recv() => {
                match ix {
                    None => {
                        // The sim loop dropped its sender, so it has already exited. This is
                        // ordinary shutdown, not a failure: treating it as one used to report
                        // a spurious commit error that masked whatever really ended the loop.
                        debug!("foc commit task: sim loop closed the channel");
                        break;
                    }
                    Some(ix) => {
                        let msg = Message::new(&[ix], Some(&pubkey));
                        let mut tx = Transaction::new_unsigned(msg);
                        tx.sign(&[keypair.as_ref()], blockhash);
                        // fire-and-forget: skip confirmation for the lowest possible latency.
                        // A failed send is not fatal either: every commit carries the whole
                        // pending input batch, so the next one re-sends whatever this tx
                        // would have carried. Killing the match over one dropped tx is worse.
                        if let Err(e) = rpc.send_transaction(&tx).await {
                            warn!("set_inputs tx send failed, retrying on next commit: {e}");
                        }
                    }
                }
            }
            .. if let _ = cancellation_token.cancelled() => {
                break;
            }
        })
    }
    debug!("foc commit task exiting");
    Ok(())
}
