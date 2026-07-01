use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use better_tokio_select::tokio_select;
use deform_core::{
    DeformError, DeformGameState, DeformUserLogic, Pubkey,
    accounts::lobby::{Lobby, LobbyStatus},
    error::{UserFacingError, UserFacingResult},
};
use tokio::{
    sync::{Notify, broadcast, mpsc},
    time::interval,
};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};
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
#[derive(Clone, Debug, SchemaRead, SchemaWrite)]
pub enum InternalServerResponse<Q: DeformQuicLogic> {
    SendDatagram(SerializedUnreliableServerResponse),
    SendReliableMessage(ReliableMessage<Q>),
}

/// When the match is being initialized, a fetch is made to solana.
/// In the meantime, a lock is not held on the matches hashmap,
/// which could lead to a race condition as other players try and create the match.
///
/// To solve this, [`MatchInfo::Initializing`] is inserted until the match is actually validated and created.
/// The [`CancellationToken`] inside can be used to await until the match is actually created. A [`CancellationToken`] was chosen as it is 'sticky', so there cannot be an issue where, by the time a client listens in on the notification, another client has notified it in the past and dropped it, making it so that the client will never actually see the notification being triggered.
#[derive(Clone)]
pub enum MatchInfo<T: DeformQuicLogic> {
    Initializing(CancellationToken),
    Started(Match<T>),
}

/// Match information that is shared between all tasks
#[derive(Clone)]
pub struct Match<Q: DeformQuicLogic> {
    /// subscribe() this to read messages produced by the match task
    pub state_sender: broadcast::Sender<InternalServerResponse<Q>>,
    /// use this to send messages to the match task
    pub match_sender: mpsc::Sender<MatchMessage<Q::UserLogic>>,

    pub game_ended: Arc<AtomicBool>,

    /// Use this when the match task should exit and be removed from the map, so that a new match can start
    pub release_notify: Arc<Notify>,

    pub expected_players: Arc<HashSet<Pubkey>>,
}

#[derive(Clone, Copy)]
pub enum MatchConfig {
    /// Waits for all players to join before starting a game
    WaitPlayers,
    /// Waits for at least one player to join.
    /// Then, the match will start after a specified duration, or if all players join.
    WaitForTimeout(Duration),
}

pub async fn match_loop<Q: DeformQuicLogic>(
    server: Arc<DeformQuicServer<Q>>,
    mut lobby_state: Lobby<Q::UserLogic>,

    state_sender: broadcast::Sender<InternalServerResponse<Q>>,
    release_notify: Arc<tokio::sync::Notify>,

    mut match_receiver: mpsc::Receiver<MatchMessage<Q::UserLogic>>,
) -> UserFacingResult<Q::UserLogic> {
    let lobby_id = lobby_state.id;

    // inputs per-tick of each player
    // NOTE: a player existing in this map means the player is currently joined
    let mut players_data: HashMap<Pubkey, HashMap<u64, <Q::UserLogic as DeformUserLogic>::Inputs>> =
        HashMap::new();

    // always wait for the first player to join
    wait_for_first_player(&mut match_receiver, &mut players_data).await?;

    match server.match_config {
        MatchConfig::WaitPlayers => loop {
            match match_receiver.recv().await {
                Some(MatchMessage::PlayerJoined { pubkey }) => {
                    players_data.insert(pubkey, HashMap::new());

                    if players_data.len() == lobby_state.player_infos.len() {
                        info!(lobby_id, "Starting lobby (all players joined)");
                        break;
                    }
                }
                Some(_) => {}
                None => {
                    Err(DeformError::ChannelClosed)?;
                }
            }
        },
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
                                    info!(lobby_id, "Starting lobby (all players joined)");
                                    break;
                                }
                            }
                            Some(_) => {}
                            None => {
                                Err(DeformError::ChannelClosed)?;
                            }
                        }
                    }
                    .. if let _ = &mut timeout => {
                        info!(lobby_id, "Starting lobby (timeout elapsed)");
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
    lobby_state.game_state =
        Some(<Q::UserLogic as DeformUserLogic>::GameState::new_from_lobby(&lobby_state));

    let mut tick_timer = interval(Duration::from_micros(16667));

    // inputs that were last applied to the game state
    // if the user does not send any inputs, these are used, as server-side input prediction
    // NOTE: doubles as a cache to pass to advance_frame() the inputs that are supposed to be applied in this tick
    let mut last_applied_inputs: HashMap<Pubkey, <Q::UserLogic as DeformUserLogic>::Inputs> =
        HashMap::new();
    for player in players_data.keys() {
        last_applied_inputs.insert(
            *player,
            <Q::UserLogic as DeformUserLogic>::Inputs::default(),
        );
    }

    let mut user_logic = <Q::UserLogic as DeformUserLogic>::new_from_lobby(&lobby_state)
        .map_err(UserFacingError::User)?;

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

                    // remove old inputs, including from current tick since they have already been copied
                    player_inputs.retain(|k, _| *k > current_tick);
                }

                match user_logic.advance_frame(
                    lobby_state.game_state.as_ref().unwrap(),
                    &last_applied_inputs,
                ) {
                    Ok(new_state) => {
                        lobby_state.game_state = Some(new_state);
                    }
                    Err(e) => {
                        let _ = state_sender.send(InternalServerResponse::SendReliableMessage(
                            ReliableMessage::Error(UserFacingError::User(e)),
                        ));
                        break;
                    }
                }

                lobby_state.tick = new_tick;

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
                    info!(lobby_id, tick = new_tick, "Match finished");
                    break;
                }
            }
            .. if let message = match_receiver.recv() => {
                if let Some(MatchMessage::<Q::UserLogic>::Inputs { pubkey, inputs }) = message {
                    // iter over the newly received inputs
                    // filter out those that are too old
                    // if the total number of inputs is too big, they will get clamped

                    // TODO: handle error here
                    if let Some(player_inputs) = players_data.get_mut(&pubkey) {
                        for (tick, new_input) in inputs.iter() {
                            if tick < &lobby_state.tick {
                                warn!(
                                    lobby_id,
                                    player = %pubkey,
                                    input_tick = tick,
                                    current_tick = lobby_state.tick,
                                    "Inputs ignored: tick is in the past"
                                );
                            } else if player_inputs.len() > MAX_INPUTS {
                                warn!(lobby_id, player = %pubkey, "Too many pending inputs");
                                break;
                            } else {
                                // only insert if entry did not already exist
                                player_inputs.entry(*tick).or_insert(new_input.clone());

                                // TODO: is overwritting better?
                                // player_inputs.insert(*tick, *new_input);
                            }
                        }
                    }
                }
            }
        });
    }

    info!(lobby_id, "Match loop ended");

    match server.matches.write().await.get_mut(&lobby_state.id) {
        Some(MatchInfo::Started(match_info)) => {
            match_info.game_ended.store(true, Ordering::SeqCst);
        }
        _ => {
            Err(DeformError::InvalidState(
                "match does not exist or has already finished".into(),
            ))?;
        }
    }

    release_notify.notified().await;

    server.matches.write().await.remove(&lobby_state.id);

    Ok(())
}

async fn wait_for_first_player<T: DeformUserLogic>(
    match_receiver: &mut mpsc::Receiver<MatchMessage<T>>,
    players_data: &mut HashMap<Pubkey, HashMap<u64, T::Inputs>>,
) -> UserFacingResult<T> {
    loop {
        match match_receiver.recv().await {
            Some(MatchMessage::PlayerJoined { pubkey }) => {
                players_data.insert(pubkey, HashMap::new());
                return Ok(());
            }
            Some(_) => {}
            None => {
                Err(DeformError::ChannelClosed)?;
            }
        }
    }
}
