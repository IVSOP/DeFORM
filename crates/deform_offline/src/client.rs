use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

use better_tokio_select::tokio_select;
use deform_core::{
    DeformClient, DeformError, DeformResult, DeformSharedBackendState, DeformUserLogic, Pubkey,
    Smooth, TickInfo,
    accounts::lobby::{Lobby, LobbyState, ongoing::LobbyOngoing},
    error::{UserFacingError, UserFacingResult},
};
use tokio::{
    sync::{mpsc, oneshot},
    time::interval,
};
use tokio_util::sync::CancellationToken;

pub(crate) struct OfflineBackend<T: DeformUserLogic> {
    /// Last inputs set by the player. Since in offline mode we never roll back, there is no need to use any other structures.
    ///
    /// [`OfflineBackend::current_info`] also has inputs, but those are the last inputs that were used to commit the state (current == has produced the current state).
    pub player_input: T::Inputs,
    pub local_lobby: Lobby<T>,
    // needed for visual update
    pub previous_game_state: T::GameState,
    pub player: Pubkey,

    // pub lobby: Pubkey,
    // pub lobby_id: u64,
    pub cancellation_token: CancellationToken,
    pub set_inputs_receiver: mpsc::UnboundedReceiver<T::Inputs>,
    // where we write the state to be read by the game engine
    pub backend_state: Arc<std::sync::Mutex<DeformSharedBackendState<T>>>,
    pub bot_fn: fn(&T::GameState, &Pubkey, &T::Inputs) -> T::Inputs,
    pub smoother: T::Smoother,
    pub visual_tick_micros: u64,
    pub last_sim_instant: Instant,
}

impl<T: DeformUserLogic> OfflineBackend<T> {
    pub fn init(
        player: Pubkey,
        lobby: Lobby<T>,
        bot_fn: fn(&T::GameState, &Pubkey, &T::Inputs) -> T::Inputs,
        visual_tick_micros: u64,
        cancellation_token: CancellationToken,
    ) -> UserFacingResult<T, DeformClient<T>> {
        let (setup_tx, setup_rx) = oneshot::channel::<DeformResult>();

        let (lobby, game_state) = match &lobby.state {
            LobbyState::Finished(_) => {
                Err(DeformError::InvalidState("Game already ended!".into()))?
            }
            LobbyState::NotStarted(not_started) => {
                let mut inputs = BTreeMap::new();
                for player in not_started.player_status.keys() {
                    inputs.insert(*player, T::Inputs::default());
                }

                let user_logic = T::new_from_lobby(&lobby.metadata, not_started)
                    .map_err(|e| UserFacingError::User(e))?;
                let game_state = T::new_game_from_lobby(&lobby.metadata, not_started)
                    .map_err(|e| UserFacingError::User(e))?;

                (
                    Lobby {
                        metadata: lobby.metadata.clone(),
                        state: LobbyState::Ongoing(LobbyOngoing {
                            slot: None,
                            tick: 0,
                            tick_info: TickInfo {
                                game_state: game_state.clone(),
                                inputs,
                            },
                            user_logic,
                        }),
                    },
                    game_state,
                )
            }
            LobbyState::Ongoing(ongoing) => (lobby.clone(), ongoing.tick_info.game_state.clone()),
        };

        let (set_inputs_sender, set_inputs_receiver) = mpsc::unbounded_channel::<T::Inputs>();
        let backend_state = Arc::new(Mutex::new(DeformSharedBackendState::new_from_lobby(
            lobby.clone(),
        )?));
        let backend_state_clone = backend_state.clone();
        let backend_state_clone_2 = backend_state.clone();
        let cancellation_token_clone = cancellation_token.clone();

        let _rss_thread = thread::spawn(move || {
            match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => {
                    rt.block_on(async move {
                        // Setup succeeded
                        let _ = setup_tx.send(Ok(()));
                        // --- Runtime phase ---
                        let tick_thread = tokio::spawn(async move {
                            let mut smoother = T::Smoother::default();
                            let decay_ratio =
                                visual_tick_micros as f32 / T::TICK_RATE_MICROS as f32;
                            smoother.scale_decay(decay_ratio);

                            let tick_info = OfflineBackend {
                                player_input: T::Inputs::default(),
                                local_lobby: lobby,
                                previous_game_state: game_state,
                                player,
                                set_inputs_receiver,
                                backend_state: backend_state.clone(),
                                bot_fn,
                                smoother,
                                visual_tick_micros,
                                last_sim_instant: Instant::now(),
                                cancellation_token: cancellation_token_clone,
                            };

                            if let Err(e) = tick_info.tick_loop().await
                                && let Ok(mut shared) = backend_state.lock()
                            {
                                shared.internal_error = Err(e);
                            }
                        });

                        if let Err(e) = tick_thread.await {
                            // if error aquiring lock, there is really no way to report it
                            if let Ok(mut shared) = backend_state_clone.lock() {
                                shared.internal_error =
                                    Err(DeformError::BackendPanicked(format!("{e:?}")).into());
                            }
                        }
                    });
                }
                Err(e) => {
                    let _ = setup_tx.send(Err(DeformError::Connection(format!(
                        "failed to build tokio runtime: {e}"
                    ))));
                }
            }
        });

        setup_rx.blocking_recv().map_err(|_| {
            DeformError::Connection("setup thread terminated unexpectedly".into())
        })??;

        Ok(DeformClient {
            set_inputs_sender,
            backend_state: backend_state_clone_2,
            cancellation_token,
        })
    }

    pub async fn tick_loop(mut self) -> UserFacingResult<T> {
        let mut tick_sleep = interval(Duration::from_micros(T::TICK_RATE_MICROS));
        let mut visual_ticker = interval(Duration::from_micros(self.visual_tick_micros));

        loop {
            tokio_select!(match .. {
                .. if let _ = tick_sleep.tick() => {
                    if matches!(self.local_lobby.state, LobbyState::Ongoing(_)) {
                        self.advance_local_simulation()?;
                        self.last_sim_instant = Instant::now();
                    } else {
                        break;
                    }
                }
                .. if let _ = visual_ticker.tick() => {
                    if let LobbyState::Ongoing(ongoing) = &self.local_lobby.state {
                        // set the state for the game engine to read as being the exact same, except the game state is replaced with a visually interpolated state
                        let elapsed = self.last_sim_instant.elapsed().as_micros() as f32;
                        let t = (elapsed / T::TICK_RATE_MICROS as f32).clamp(0.0, 1.0);
                        let mut visual_state = ongoing.tick_info.game_state.clone();

                        self.smoother
                            .apply(&self.previous_game_state, &mut visual_state, t);

                        let mut fake_visual_lobby = ongoing.clone();
                        fake_visual_lobby.tick_info.game_state = visual_state;

                        {
                            let mut shared = self
                                .backend_state
                                .lock()
                                .map_err(|_| DeformError::LockPoisoned)?;

                            shared.lobby.state = LobbyState::Ongoing(fake_visual_lobby);
                        }
                    }
                }
                // New inputs from the game engine
                .. if let new_inputs = self.set_inputs_receiver.recv() => {
                    if new_inputs.is_none() {
                        break;
                    }
                    if !matches!(self.local_lobby.state, LobbyState::Finished(_))
                        && let Some(new_inputs) = new_inputs
                    {
                        self.player_input = new_inputs;
                    }
                }
                .. if let _ = self.cancellation_token.cancelled() => {
                    break;
                }
            });
        }

        // when leaving, if the game has finished, set the final visual state to the state of the lobby
        if let LobbyState::Finished(finished) = self.local_lobby.state {
            let mut shared = self
                .backend_state
                .lock()
                .map_err(|_| DeformError::LockPoisoned)?;

            shared.lobby.state = LobbyState::Finished(finished);
        }

        Ok(())
    }

    pub fn advance_local_simulation(&mut self) -> UserFacingResult<T> {
        // FIX: what is a clean way of doing this?
        // I can't just pass in a &mut ongoing I think, since this would be two mutable refs to the struct
        // I trust that the compiler will handle it
        let ongoing = match &mut self.local_lobby.state {
            LobbyState::Ongoing(ongoing) => ongoing,
            _ => unreachable!(),
        };

        let current_state = &ongoing.tick_info.game_state;

        // clone the old array so that we have the correct pubkeys
        // the inputs will be overwritten
        let mut new_players_inputs: BTreeMap<Pubkey, T::Inputs> = ongoing.tick_info.inputs.clone();

        for (player, inputs) in new_players_inputs.iter_mut() {
            *inputs = if *player == self.player {
                // for our own player: get the last inputs from the map
                self.player_input.clone()
            } else {
                let prev = ongoing
                    .tick_info
                    .inputs
                    .get(player)
                    .cloned()
                    .unwrap_or_default();
                (self.bot_fn)(current_state, player, &prev)
            }
        }

        let new_state = ongoing
            .user_logic
            .advance_frame(current_state, &new_players_inputs)
            .map_err(UserFacingError::User)?;

        let next_info = TickInfo {
            game_state: new_state,
            inputs: new_players_inputs,
        };

        ongoing.tick += 1;
        // The outgoing state becomes the origin of the visual interpolation, the
        // same (prev tick, current tick) pair the networked backends read from
        // their history. Leaving it stale makes every visual tick lerp from the
        // *initial* state instead, which oscillates the whole world at tick rate.
        self.previous_game_state = std::mem::replace(&mut ongoing.tick_info, next_info).game_state;

        Ok(())
    }
}
