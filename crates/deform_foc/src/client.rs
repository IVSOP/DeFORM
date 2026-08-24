use std::{
    collections::{BTreeMap, HashMap},
    pin::Pin,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use better_tokio_select::tokio_select;
use deform_core::{
    ChannelInputs, DeformClient, DeformError, DeformInputs, DeformSharedBackendState,
    DeformUserLogic, Pubkey, Smooth, TickInfo,
    accounts::{
        DeformAccount,
        lobby::{Lobby, LobbyFinished, LobbyState, ongoing::LobbyOngoing},
    },
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
use tokio::{
    sync::mpsc,
    time::{Sleep, interval, sleep_until},
};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, warn};

use crate::DeformFocLogic;

/// How many inputs the server should have queued up ideally. If it has 0, it means it has starved
const TARGET_BUFFER: f32 = 1.0;
/// Only after this deadzone do we start slowing the simulation down
const SLOWDOWN_DEADZONE: f32 = 1.0;

// coefficients for the math
const SPEEDUP_SOFTNESS: f32 = 1.0;
const SLOWDOWN_SOFTNESS: f32 = 3.0;
const SLOWDOWN_RATIO: f32 = 0.5;
// make the buffer estimate drop fast
const BUFFER_FALL: f32 = 0.60;
// make the buffer estimate climb slow
const BUFFER_RISE: f32 = 0.05;
/// When our own input is late this means we are either too slow or there was input loss, so instantly kick the tick target up
const ROLLBACK_KICK: f32 = 1.0;
/// Max value the tick target can be kicked by
const PANIC_MAX: f32 = 3.0;
const PANIC_DECAY: f32 = 0.955;
/// Freeze if we get this many ticks ahead. Means reports stopped and either server died or we lost connection.
const MAX_PREDICTION_TICKS: u64 = 30;

pub struct FocBackend<F: DeformFocLogic> {
    pub local_tick: u64,
    pub info_per_tick: HashMap<u64, TickInfo<F::UserLogic>>,
    pub remote_lobby: Lobby<F::UserLogic>,
    /// Inputs from our own player, keyed by the tick they apply to.
    pub inputs: HashMap<u64, ChannelInputs<F::UserLogic>>,

    pub player: Pubkey,
    pub lobby_pda: Pubkey,
    pub inputs_pda: Pubkey,
    pub lobby_id: u64,

    pub program_client: F::ProgramClient,
    /// Built `set_inputs` instructions are handed to the [`commit_task`], which owns
    /// the RPC send path. The channel is bounded, so a slow commit task backpressures
    /// the sim loop rather than piling up unbounded work.
    pub commit_tx: mpsc::Sender<Instruction>,

    /// Lobby and inputs accounts, both decoded from `accountSubscribe`.
    pub account_update_rx: mpsc::UnboundedReceiver<DeformAccount<F::UserLogic>>,

    pub cancellation_token: CancellationToken,
    pub set_inputs_receiver: mpsc::UnboundedReceiver<ChannelInputs<F::UserLogic>>,
    pub backend_state: Arc<Mutex<DeformSharedBackendState<F::UserLogic>>>,
    pub user_logic: F::UserLogic,

    pub smoother: <F::UserLogic as DeformUserLogic>::Smoother,
    pub visual_tick_micros: u64,
    /// The ER's slot/block time in micros, provided by the caller (matching on-chain
    /// `get_micros_per_slot`). Drives the commit cadence (~one tx per slot) and is the
    /// inclusion-latency floor folded into the ticks-ahead target.
    pub slot_time_micros: u64,
    pub last_sim_instant: Instant,
    /// Measured duration of the last sim interval. Denominator for visual `t`.
    pub last_tick_interval: Duration,
    /// Absolute deadline for the next simulation tick, anchored to the previous
    /// deadline so per-tick work and jitter don't accumulate as drift.
    pub next_tick_deadline: tokio::time::Instant,

    /// Pessimistic estimate of how many of our inputs the chain holds for future ticks.
    /// (we assume it has less than reality by being agressive when reducing and slow to increase)
    pub buffer_estimate: f32,
    /// Target bonus applied after a self-rollback, decaying on every inputs update.
    pub rollback_panic: f32,
}

impl<F: DeformFocLogic> FocBackend<F> {
    /// How many game ticks are batched into a single `set_inputs` transaction: as many
    /// as fit in one ER slot, at least one. Committing every tick is not viable here,
    /// so this is also the worst-case wait an input can sit through before being sent.
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

        loop {
            tokio_select!(match .. {
                // Simulation tick, possibly dilated to let the chain catch up.
                .. if let _ = &mut tick_sleep => {
                    match &self.remote_lobby.state {
                        LobbyState::Finished(_) => break,
                        // No authoritative stream to reconcile against yet; hold.
                        LobbyState::NotStarted(_) => break,
                        LobbyState::Ongoing(ongoing) => {
                            let remote_tick = ongoing.tick;
                            if self.local_tick == remote_tick {
                                // at match start the data has not been populated, so estimate an RTT of 250ms
                                let fake_lead = (Duration::from_millis(250).as_micros() as u64
                                    + 3000)
                                    .div_ceil(tick_micros)
                                    + 1;

                                for _ in 0..fake_lead {
                                    self.advance_local_simulation()?;
                                }
                                // A burst has no interval to measure; hold the base rate.
                                self.last_tick_interval = Duration::from_micros(tick_micros);
                                self.last_sim_instant = Instant::now();
                                self.buffer_estimate = TARGET_BUFFER;
                            } else {
                                // Every smaller correction, catching up or shedding
                                // lead, is handled by time dilation (see
                                // `compute_dilated_tick_interval`).
                                if self.local_tick < remote_tick + MAX_PREDICTION_TICKS {
                                    self.advance_local_simulation()?;
                                    self.close_sim_interval();
                                }
                            }
                        }
                    }

                    // Commit once enough new input ticks have piled up, rather than on a
                    // wall-clock timer. Inputs are produced by ticks, so counting ticks keeps
                    // the batch size constant under time dilation; a timer would instead fire
                    // with little new to send and pay for a near-empty transaction.
                    //
                    // The elapsed check is only a backstop: the simulation stops advancing
                    // while frozen at `MAX_PREDICTION_TICKS`, and the tick count alone would then
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

                    let dilated = self.compute_dilated_tick_interval();
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
                        let t =
                            (elapsed / self.last_tick_interval.as_micros() as f32).clamp(0.0, 1.0);
                        #[cfg(feature = "metrics")]
                        deform_metrics::plot!("visual_t", t as f64);

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
                            // Merging keeps the existing entry's creation_time, so a tick
                            // reports its first sample: the one that waited longest.
                            #[cfg(feature = "metrics")]
                            Some(existing) => existing.inputs.merge(&new_inputs.inputs),
                            #[cfg(not(feature = "metrics"))]
                            Some(existing) => existing.merge(&new_inputs),
                            None => {
                                self.inputs.insert(self.local_tick, new_inputs);
                            }
                        }
                    }
                }

                // Account updates from the ER: the lobby is the authoritative state, our
                // inputs account is how deep the chain's queue of our inputs is.
                .. if let account = self.account_update_rx.recv() => {
                    let Some(account) = account else { break };

                    if !matches!(self.remote_lobby.state, LobbyState::Finished(_)) {
                        match account {
                            DeformAccount::Lobby(lobby) => {
                                self.process_new_state(lobby.state, &mut tick_sleep)?;
                            }
                            DeformAccount::Inputs(inputs) => {
                                self.rollback_panic *= PANIC_DECAY;

                                #[cfg(feature = "metrics")]
                                deform_metrics::plot!("rollback_panic", self.rollback_panic as f64);

                                let buffered = inputs.inputs.len().min(u8::MAX as usize) as u8;
                                self.process_buffer_len_update(buffered);
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

    /// Measure the interval that just closed and re-anchor `last_sim_instant`.
    /// Clamped to dilation's own range so a frozen tick can't stall interpolation.
    fn close_sim_interval(&mut self) {
        let now = Instant::now();
        let base = Duration::from_micros(<F::UserLogic as DeformUserLogic>::TICK_RATE_MICROS);
        self.last_tick_interval = now
            .duration_since(self.last_sim_instant)
            .clamp(base / 2, base * 4);
        self.last_sim_instant = now;
    }

    /// Updates values according to a newly received buffered_inputs_len value
    ///
    /// - buffer_estimate: pessimistic EWMA (fast down, slow up)
    fn process_buffer_len_update(&mut self, buffered: u8) {
        let sample = buffered as f32;
        let alpha = if sample < self.buffer_estimate {
            BUFFER_FALL
        } else {
            BUFFER_RISE
        };
        self.buffer_estimate += alpha * (sample - self.buffer_estimate);

        #[cfg(feature = "metrics")]
        {
            deform_metrics::plot!("input_buffer", sample as f64);
            deform_metrics::plot!("input_buffer_est", self.buffer_estimate as f64);
        }
    }

    /// Time dilation steered by how many of our inputs the server has queued up.
    ///
    /// Time speeds up (shorter ticks) when we are behind our target, or when self-rollbacks happen.
    ///
    /// Time slows down (longer ticks) when we are ahead of our target.
    fn compute_dilated_tick_interval(&self) -> Duration {
        let base_micros = <F::UserLogic as DeformUserLogic>::TICK_RATE_MICROS as f32;

        let target = TARGET_BUFFER + F::JITTER_SLACK + self.rollback_panic;

        let behind = (target - self.buffer_estimate).max(0.0);
        let ahead = (self.buffer_estimate - target - SLOWDOWN_DEADZONE).max(0.0);

        let rate = 1.0 + F::TIME_DILATION * (behind / SPEEDUP_SOFTNESS).tanh()
            - F::TIME_DILATION * SLOWDOWN_RATIO * (ahead / SLOWDOWN_SOFTNESS).tanh();

        let micros = base_micros / rate;

        #[cfg(feature = "metrics")]
        {
            deform_metrics::plot!("input_buffer_target", target as f64);
            deform_metrics::plot!("sleep_time", micros as f64 / 1000.0);
        }

        Duration::from_micros(micros as u64)
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
                    #[cfg(feature = "metrics")]
                    deform_metrics::plot!(
                        "local_input_delay",
                        provided_inputs.creation_time.elapsed().as_micros() as f64
                    );
                    #[cfg(feature = "metrics")]
                    let provided_inputs = &provided_inputs.inputs;
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

        let new_state = {
            #[cfg(feature = "metrics")]
            let _span = deform_metrics::span!("sim_compute");
            self.user_logic
                .advance_frame(&current_info.game_state, &new_players_inputs)
        }
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
            .map(|(tick, inputs)| {
                #[cfg(feature = "metrics")]
                let inputs = &inputs.inputs;
                (*tick, inputs.clone())
            })
            .collect();

        if pending.is_empty() {
            return Ok(());
        }

        #[cfg(feature = "metrics")]
        deform_metrics::plot!("commit_batch_ticks", pending.len() as f64);

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

        #[cfg(feature = "metrics")]
        let max_tick = pending.keys().max().copied();

        // Bounded channel: this awaits — backpressuring the sim loop — if the commit
        // task has fallen behind. A closed channel just means we're shutting down.
        let _ = self.commit_tx.send(sdk_ix).await;

        #[cfg(feature = "metrics")]
        if let Some(newest) = max_tick
            && let Some(entry) = self.inputs.get(&newest)
        {
            deform_metrics::plot!(
                "input_to_commit",
                entry.creation_time.elapsed().as_micros() as f64
            );
        }

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

        #[cfg(feature = "metrics")]
        let discarded_before = self.smoother.corrections_discarded();

        self.smoother.on_rollback(
            &pre_rollback_info.game_state,
            &post_rollback_info.game_state,
        );

        #[cfg(feature = "metrics")]
        deform_metrics::event!(
            "rollback",
            to_tick = conflicting_tick,
            depth = previous_local_tick.saturating_sub(conflicting_tick),
            magnitude = self.smoother.correction_magnitude_sq().sqrt(),
            corrections_discarded = self.smoother.corrections_discarded() - discarded_before,
        );

        self.user_logic
            .on_rollback(pre_rollback_info, post_rollback_info)
            .map_err(UserFacingError::User)?;

        let dilated = self.compute_dilated_tick_interval();
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
            Rollback { self_mismatch: bool },
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

            // a mismatch on our own player is singled out: it means our inputs reached
            // the chain after it needed them, which is what the pacing has to react to.
            let mut mismatch = false;
            let mut self_mismatch = false;
            for (player, predicted_input) in predicted_inputs.iter() {
                let remote_input = remote_inputs.get(player).ok_or(DeformError::InvalidState(
                    "player not found in remote inputs".into(),
                ))?;

                if remote_input != predicted_input {
                    mismatch = true;
                    if *player == self.player {
                        self_mismatch = true;
                        break;
                    }
                }
            }

            if mismatch {
                ReceivedScenario::Rollback { self_mismatch }
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

                #[cfg(feature = "metrics")]
                deform_metrics::event!(
                    "fast_forward",
                    from_tick = self.local_tick,
                    to_tick = new_remote_tick,
                    jump = new_remote_tick.saturating_sub(self.local_tick),
                );

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

                #[cfg(feature = "metrics")]
                deform_metrics::event!(
                    "gap",
                    from_tick = old_remote_tick,
                    to_tick = new_remote_tick,
                    missed = new_remote_tick.saturating_sub(old_remote_tick + 1),
                );

                self.user_logic
                    .on_gap(old_remote_state, &new_tick_info)
                    .map_err(UserFacingError::User)?;

                self.handle_rollback(new_tick_info, new_remote_tick, tick_sleep)?;
            }
            ReceivedScenario::Rollback { self_mismatch } => {
                // Direct evidence that the buffer ran dry, which no report can show as
                // sharply: raise the target and let it decay back down.
                if self_mismatch {
                    self.rollback_panic = (self.rollback_panic + ROLLBACK_KICK).min(PANIC_MAX);

                    #[cfg(feature = "metrics")]
                    deform_metrics::event!("self_rollback", panic = self.rollback_panic);
                }

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
