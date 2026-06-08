use better_tokio_select::tokio_select;
use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};
use tokio::{
    sync::{Notify, mpsc, oneshot},
    time::interval,
};

use deform_core::{
    DeformClient, DeformError, DeformReadState, DeformResult, DeformUserLogic, Pubkey, TickInfo,
    lobby::LobbyStatus,
};

pub(crate) struct OfflineBackend<T: DeformUserLogic> {
    /// Last inputs set by the player. Since in offline mode we never roll back, there is no need to use any other structures.
    ///
    /// [`OfflineBackend::current_info`] also has inputs, but those are the last inputs that were used to commit the state (current == has produced the current state).
    pub player_input: T::Inputs,
    pub local_tick: u64,
    pub last_status: LobbyStatus,
    // this will pretty much always have the same value as whatever is inside the mutex, it's just to decrease contention
    pub current_info: TickInfo<T>,
    pub player: Pubkey,

    // pub lobby: Pubkey,
    // pub lobby_id: u64,
    pub terminate: Arc<Notify>,
    pub set_inputs_receiver: mpsc::UnboundedReceiver<T::Inputs>,
    pub sdk_game_state: Arc<std::sync::Mutex<DeformReadState<T>>>,
    pub user_logic: T,
    pub bot_fn: fn(&T::GameState, &Pubkey, &T::Inputs) -> T::Inputs,
}

impl<T: DeformUserLogic> OfflineBackend<T> {
    pub fn init(
        player: Pubkey,
        players: HashSet<Pubkey>,
        bot_fn: fn(&T::GameState, &Pubkey, &T::Inputs) -> T::Inputs,
    ) -> DeformResult<DeformClient<T>> {
        let (setup_tx, setup_rx) = oneshot::channel::<DeformResult>();

        let terminate = Arc::new(Notify::new());
        let (set_inputs_sender, set_inputs_receiver) = mpsc::unbounded_channel::<T::Inputs>();
        let sdk_game_state = Arc::new(std::sync::Mutex::new(DeformReadState::<T>::new(&players)));
        let backend_dead = Arc::new(AtomicBool::new(false));

        // cursed
        let sdk_game_state_clone = sdk_game_state.clone();
        let sdk_game_state_clone_2 = sdk_game_state.clone();

        let terminate_clone = terminate.clone();
        let backend_dead_clone = backend_dead.clone();

        // #[cfg(feature = "log")]
        // info!("QUIC init with rpc: {}", rpc_url);
        // #[cfg(feature = "log")]
        // info!(
        //     "QUIC init with server: {} (SNI: {})",
        //     server_addr, server_name
        // );

        let _rss_thread = thread::spawn(move || {
            match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => {
                    rt.block_on(async move {
                        let current_info: TickInfo<T> = {
                            let shared = match sdk_game_state_clone.lock() {
                                Ok(state) => state,
                                Err(_e) => {
                                    let _ = setup_tx.send(Err(DeformError::LockPoisoned));
                                    return;
                                }
                            };

                            shared.tick_info.clone()
                        };

                        // Setup succeeded
                        let _ = setup_tx.send(Ok(()));
                        // --- Runtime phase ---
                        let tick_thread = tokio::spawn(async move {
                            // FIX: criar um estado default com os 2 jogadores
                            // FIX: fazer um game state message e tambem o current info estarem em sync com isso

                            let tick_info = OfflineBackend {
                                player_input: T::Inputs::default(),
                                local_tick: 0,
                                last_status: LobbyStatus::NotStarted,
                                current_info,

                                player,
                                terminate: terminate_clone,
                                set_inputs_receiver,
                                sdk_game_state: sdk_game_state_clone.clone(),
                                user_logic: T::default(),
                                bot_fn,
                            };

                            if let Err(e) = tick_info.tick_loop().await
                                && let Ok(mut shared) = sdk_game_state_clone.lock()
                            {
                                shared.internal_error = Err(e);
                            }
                        });

                        if let Err(e) = tick_thread.await {
                            // if error aquiring lock, there is really no way to report it
                            if let Ok(mut shared) = sdk_game_state_clone_2.lock() {
                                shared.internal_error =
                                    Err(DeformError::BackendPanicked(format!("{e:?}")));
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
            backend_dead_clone.store(true, Ordering::Relaxed);
        });

        setup_rx.blocking_recv().map_err(|_| {
            DeformError::Connection("setup thread terminated unexpectedly".into())
        })??;

        Ok(DeformClient {
            terminate,
            set_inputs_sender,
            sdk_game_state,
            backend_dead,
        })
    }

    pub async fn tick_loop(mut self) -> DeformResult {
        let mut tick_sleep = interval(Duration::from_micros(T::TICK_RATE_MICROS));
        let mut terminated = false;

        loop {
            tokio_select!(match .. {
                .. if let _ = tick_sleep.tick() => {
                    if self.last_status != LobbyStatus::Finished {
                        self.advance_local_simulation()?;

                        {
                            let mut shared = self
                                .sdk_game_state
                                .lock()
                                .map_err(|_| DeformError::LockPoisoned)?;
                            shared.tick_info = self.current_info.clone();
                            shared.remote_status = self.last_status;
                            // shared.events.append(&mut tick_info.events_queue);
                        }
                    } else {
                        break;
                    }
                }
                // Shutdown signal
                .. if let _ = self.terminate.notified() => {
                    // #[cfg(feature = "log")]
                    // warn!("Shutdown signal received; exiting");
                    terminated = true;
                    break;
                }
                // New inputs from the game engine
                .. if let new_inputs = self.set_inputs_receiver.recv() => {
                    if new_inputs.is_none() {
                        break;
                    }
                    if self.last_status != LobbyStatus::Finished
                        && let Some(new_inputs) = new_inputs
                    {
                        self.player_input = new_inputs;
                    }
                }
            });
        }

        if !terminated && self.last_status == LobbyStatus::Finished {
            // Wait for termination signal
            self.terminate.notified().await;
        }

        Ok(())
    }

    pub fn advance_local_simulation(&mut self) -> DeformResult {
        let current_state = &self.current_info.game_state;

        // clone the old array so that we have the correct pubkeys
        // the inputs will be overwritten
        let mut new_players_inputs: HashMap<Pubkey, T::Inputs> = self.current_info.inputs.clone();

        for (player, inputs) in new_players_inputs.iter_mut() {
            *inputs = if *player == self.player {
                // for our own player: get the last inputs from the map
                self.player_input.clone()
            } else {
                let prev = self
                    .current_info
                    .inputs
                    .get(player)
                    .cloned()
                    .unwrap_or_default();
                (self.bot_fn)(current_state, player, &prev)
            }
        }

        let new_state = self
            .user_logic
            .advance_frame(current_state, &new_players_inputs)
            .map_err(|e| DeformError::UserLogic(Box::new(e)))?;

        let next_info = TickInfo {
            game_state: new_state,
            inputs: new_players_inputs,
        };

        self.local_tick += 1;
        self.current_info = next_info;

        Ok(())
    }
}
