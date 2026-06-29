use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Duration,
};

use anyhow::Context;
use better_tokio_select::tokio_select;
use deform_core::{
    DeformGameState, DeformUserLogic, Pubkey,
    error::UserFacingError,
    lobby::{LobbyData, LobbyStatus},
};
use tokio::{
    sync::{Notify, RwLock, broadcast, mpsc},
    time::interval,
};
use wincode::{SchemaRead, SchemaWrite};

use crate::{
    DeformQuicLogic, ReliableMessage, SerializedUnreliableServerResponse, UnreliableServerResponse,
    server::DeformQuicServer,
};

// TODO: put this in the PlayerInputsAccount
pub const MAX_INPUTS: usize = 18;

pub enum MatchMessage<U: DeformUserLogic> {
    PlayerJoined {
        pubkey: Pubkey,
    },
    Inputs {
        pubkey: Pubkey,
        inputs: HashMap<u64, U::Inputs>,
    },
}

/// Messages sent internally from the match task to each client-handling task
#[derive(Debug, SchemaRead, SchemaWrite)]
pub enum InternalServerResponse<Q: DeformQuicLogic> {
    SendDatagram(SerializedUnreliableServerResponse),
    SendReliableMessage(ReliableMessage<Q>),
}

/// Match information that is shared between all tasks
pub struct MatchInfo<T: DeformUserLogic + DeformQuicLogic> {
    /// subscribe() this to read messages produced by the match task
    pub state_sender: broadcast::Sender<InternalServerResponse<T>>,
    /// use this to send messages to the match task
    pub match_sender: mpsc::Sender<MatchMessage<T>>,

    // new clients that see this value as true will immediately be met with an error
    pub game_ended: bool,

    /// Use this when the match task should exit and be removed from the map, so that a new match can start
    pub release_notify: Arc<Notify>,
}

pub enum MatchConfig {
    /// Waits for all players to join before starting a game
    WaitPlayers,
    /// Waits for at least one player to join.
    /// Then, the match will start after a specified duration, or if all players join.
    WaitForTimeout(Duration),
}

impl<T: DeformQuicLogic + DeformUserLogic> DeformQuicServer<T> {
    /// Does not perform any validation on the lobby's data
    pub async fn match_loop(
        &self,
        matches: Arc<RwLock<HashMap<u64, MatchInfo<T>>>>,
        mut lobby_state: LobbyData<T>,

        // TODO: cleaner to have the function access this from the matches array instead??
        state_sender: broadcast::Sender<InternalServerResponse<T>>,
        release_notify: Arc<tokio::sync::Notify>,

        mut match_receiver: mpsc::Receiver<MatchMessage<T>>,
        mut user_logic: T,
    ) -> anyhow::Result<()> {
        // inputs per-tick of each player
        // NOTE: a player existing in this map means the player is currently joined
        let mut players_data: HashMap<Pubkey, HashMap<u64, T::Inputs>> = HashMap::new();

        // always wait for the first player to join
        Self::wait_for_first_player(&mut match_receiver, &lobby_state, &mut players_data).await?;

        // TODO: depending on self.match_config, wait for all players to join
        match self.match_config {
            MatchConfig::WaitPlayers => {
                loop {
                    match match_receiver.recv().await {
                        Some(MatchMessage::PlayerJoined { pubkey }) => {
                            players_data.insert(pubkey, HashMap::new());

                            if players_data.len() == lobby_state.player_infos.len() {
                                // info!("Starting lobby {} (both players joined)", lobby_state.lobby);
                                break;
                            }
                        }
                        Some(_) => {}
                        None => {
                            anyhow::bail!(
                                "Match channel closed before start for lobby {}",
                                lobby_state.id
                            );
                        }
                    }
                }
            }
            MatchConfig::WaitForTimeout(timeout) => {
                let timeout = tokio::time::sleep(timeout);
                tokio::pin!(timeout);
                loop {
                    tokio_select!(match .. {
                        .. if let message = match_receiver.recv() => {
                            match message {
                                Some(MatchMessage::PlayerJoined { pubkey }) => {
                                    players_data.insert(pubkey, HashMap::new());

                                    if players_data.len() == lobby_state.player_infos.len() {
                                        // info!("Starting lobby {} (both players joined)", lobby_state.lobby);
                                        break;
                                    }
                                }
                                Some(_) => {}
                                None => {
                                    anyhow::bail!(
                                        "Match channel closed before start for lobby {}",
                                        lobby_state.id
                                    );
                                }
                            }
                        }
                        .. if let _ = &mut timeout => {
                            // info!("Starting lobby {} (10s timeout elapsed)", lobby_state.lobby);
                            break;
                        }
                    });
                }
            }
        }

        let mut players_hashset = HashSet::new();
        for player in players_data.keys() {
            players_hashset.insert(*player);
        }

        // mark game as started
        lobby_state.status = LobbyStatus::Started;
        // init the game state
        lobby_state.game_state = Some(T::GameState::new(&players_hashset));

        let mut tick_timer = interval(Duration::from_micros(16667));

        // inputs that were last applied to the game state
        // if the user does not send any inputs, these are used, as server-side input prediction
        // NOTE: doubles as a cache to pass to advance_frame() the inputs that are supposed to be applied in this tick
        let mut last_applied_inputs: HashMap<Pubkey, T::Inputs> = HashMap::new();
        for player in players_data.keys() {
            last_applied_inputs.insert(*player, T::Inputs::default());
        }

        loop {
            tokio_select!(match .. {
                .. if let _ = tick_timer.tick() => {
                    let current_tick = lobby_state.tick;
                    let new_tick = current_tick + 1;

                    // not cleared, to not mess with the len
                    // values have to be overwritten anyway
                    // tick_inputs.clear();

                    for (player, player_inputs) in players_data.iter_mut() {
                        // read inputs from this slot
                        // if there were no inputs, last_applied_inputs will be used anyway
                        // but if there were, then overwrite them now
                        if let Some(inputs) = player_inputs.get(&current_tick) {
                            last_applied_inputs.insert(*player, inputs.clone());
                        };

                        // no nee
                        // player_inputs.insert(current_tick, new_inputs);

                        // remove old inputs, including from current tick since they have already been copied
                        player_inputs.retain(|k, _| *k > current_tick);
                    }

                    if let Err(e) = user_logic.advance_frame(
                        lobby_state.game_state.as_ref().unwrap(),
                        &last_applied_inputs,
                    ) {
                        let _ = state_sender.send(InternalServerResponse::SendReliableMessage(
                            ReliableMessage::Error(UserFacingError::User(e)),
                        ));
                        break;
                    }

                    lobby_state.tick = new_tick;

                    // broadcast the new state
                    // #[cfg(feature = "debug")]
                    // info!("Broadcasting state for lobby {}: tick={}", state.lobby, new_tick);
                    let message = UnreliableServerResponse {
                        lobby_info: lobby_state.clone(),
                    };
                    // TODO: TREAT ERRORS
                    if let Ok(serialized_message) = wincode::serialize(&message) {
                        let _ = state_sender.send(InternalServerResponse::SendDatagram(
                            SerializedUnreliableServerResponse(serialized_message),
                        ));
                    }

                    if lobby_state.game_state.as_ref().unwrap().has_ended() {
                        // TODO: TREAT ERRORS
                        let _ = state_sender.send(InternalServerResponse::SendReliableMessage(
                            ReliableMessage::Finish,
                        ));
                        break;
                    }
                }
                .. if let message = match_receiver.recv() => {
                    if let Some(MatchMessage::<T>::Inputs { pubkey, inputs }) = message {
                        // iter over the newly received inputs
                        // filter out those that are too old
                        // if the total number of inputs is too big, they will get clamped

                        // TODO: handle error here
                        if let Some(player_inputs) = players_data.get_mut(&pubkey) {
                            for (tick, new_input) in inputs.iter() {
                                if tick < &lobby_state.tick {
                                    // warn!(
                                    //     "Inputs ignored: got tick {}, but current tick is {} (delta {})",
                                    //     tick,
                                    //     lobby_state.tick,
                                    //     lobby_state.tick - tick
                                    // );
                                } else if player_inputs.len() > MAX_INPUTS {
                                    // warn!("There are already too many inputs");
                                    break;
                                } else {
                                    // only insert if entry did not already exist
                                    player_inputs.entry(*tick).or_insert(new_input.clone());

                                    // TODO: is overwritting better?
                                    // player_inputs.insert(*tick, *new_input);
                                }
                            }
                        }
                        // info!(
                        //     "Processed inputs for player {} in lobby {}: {} slots",
                        //     inputs.player_id,
                        //     lobby_state.lobby,
                        //     inputs.inputs.len()
                        // );
                    }
                }
            });
        }

        // info!(
        //     "Crank loop for lobby {} finished  {}-{}",
        //     lobby_state.lobby, game_state.players[0].score, game_state.players[1].score
        // );
        match matches.write().await.get_mut(&lobby_state.id) {
            Some(match_info) => {
                match_info.game_ended = true;
            }
            None => {
                // error!("Match does not exist");
                anyhow::bail!("Internal server error: match does not exist");
            }
        }

        release_notify.notified().await;

        // FIX: remove from match info array...

        Ok(())
    }

    async fn wait_for_first_player(
        match_receiver: &mut mpsc::Receiver<MatchMessage<T>>,
        lobby_state: &LobbyData<T>,
        players_data: &mut HashMap<Pubkey, HashMap<u64, T::Inputs>>,
    ) -> anyhow::Result<()> {
        loop {
            match match_receiver.recv().await {
                Some(MatchMessage::PlayerJoined { pubkey }) => {
                    players_data.insert(pubkey, HashMap::new());
                    // info!("First player {} joined lobby {}", id, lobby_state.lobby);
                    return Ok(());
                }
                Some(_) => {}
                None => {
                    anyhow::bail!(
                        "Match channel closed before start for lobby {}",
                        lobby_state.id
                    );
                }
            }
        }
    }
}
