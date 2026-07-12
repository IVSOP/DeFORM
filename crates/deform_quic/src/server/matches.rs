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
    DeformError, DeformGameState, DeformUserLogic, Pubkey, TickInfo,
    accounts::lobby::{
        Lobby, LobbyMetadata, LobbyState, not_started::LobbyNotStarted, started::LobbyOngoing,
    },
    error::{UserFacingError, UserFacingResult},
};
use tokio::{
    sync::{broadcast, mpsc},
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
    lobby_metadata: LobbyMetadata,
    not_started: LobbyNotStarted,

    state_sender: broadcast::Sender<InternalServerResponse<Q>>,

    mut match_receiver: mpsc::Receiver<MatchMessage<Q::UserLogic>>,
) -> UserFacingResult<Q::UserLogic> {
    let lobby_id = lobby_metadata.id;

    // inputs per-tick of each player
    // NOTE: a player existing in this map means the player is currently joined
    let mut players_data: HashMap<Pubkey, HashMap<u64, <Q::UserLogic as DeformUserLogic>::Inputs>> =
        HashMap::new();

    // always wait for the first player to join
    // FIX: pass cancellation token into here
    wait_for_first_player(&mut match_receiver, &mut players_data).await?;

    // TODO: should these also await a cancellation token?
    match server.match_config {
        MatchConfig::WaitPlayers => loop {
            match match_receiver.recv().await {
                Some(MatchMessage::PlayerJoined { pubkey }) => {
                    players_data.insert(pubkey, HashMap::new());

                    if players_data.len() == not_started.player_status.len() {
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

                                if players_data.len() == not_started.player_status.len() {
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

    let user_logic = Q::UserLogic::new_from_lobby(&lobby_metadata, &not_started)
        .map_err(|e| UserFacingError::User(e))?;
    let game_state = Q::UserLogic::new_game_from_lobby(&lobby_metadata, &not_started)
        .map_err(|e| UserFacingError::User(e))?;

    let mut inputs = HashMap::new();
    for player in not_started.player_status.keys() {
        inputs.insert(
            *player,
            <Q::UserLogic as DeformUserLogic>::Inputs::default(),
        );
    }

    let mut lobby = Lobby {
        metadata: lobby_metadata,
        state: LobbyState::Ongoing(LobbyOngoing {
            tick: 0,
            user_logic,
            tick_info: TickInfo { game_state, inputs },
        }),
    };

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

    let ongoing = match &mut lobby.state {
        LobbyState::Ongoing(ongoing) => ongoing,
        _ => unreachable!(),
    };

    loop {
        tokio_select!(match .. {
            .. if let _ = tick_timer.tick() => {
                let current_tick = ongoing.tick;
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

                match ongoing
                    .user_logic
                    .advance_frame(&ongoing.tick_info.game_state, &last_applied_inputs)
                {
                    Ok(new_state) => {
                        ongoing.tick_info.game_state = new_state;
                    }
                    Err(e) => {
                        mark_match_as_finished(&server, lobby.metadata.id).await?;
                        return Err(UserFacingError::User(e));
                    }
                }

                ongoing.tick = new_tick;

                // TODO: wtf is this doing?
                for (player, applied_input) in last_applied_inputs.iter() {
                    if let Some(inputs) = ongoing.tick_info.inputs.get_mut(player) {
                        *inputs = applied_input.clone();
                    }
                }

                let message = UnreliableServerResponse {
                    lobby_state: LobbyState::Ongoing(ongoing.clone()),
                };
                // TODO: TREAT ERRORS
                if let Ok(serialized_message) = wincode::serialize(&message) {
                    let _ = state_sender.send(InternalServerResponse::SendDatagram(
                        SerializedUnreliableServerResponse(serialized_message),
                    ));
                }

                if ongoing.tick_info.game_state.has_ended() {
                    info!(
                        lobby_id,
                        tick = new_tick,
                        "Game has ended, running on_match_end"
                    );
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
                            if tick < &ongoing.tick {
                                warn!(
                                    lobby_id,
                                    player = %pubkey,
                                    input_tick = tick,
                                    current_tick = ongoing.tick,
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

    mark_match_as_finished(&server, lobby.metadata.id).await?;

    server
        .user_server_logic
        .on_match_end(
            &lobby,
            &server.rpc_client,
            &server.admin_keypair,
            &server.game_program_client,
        )
        .await?;

    let _ = state_sender.send(InternalServerResponse::SendReliableMessage(
        ReliableMessage::Finish(lobby.clone()),
    ));
    info!(lobby_id, "Match finished successfully");

    server.matches.write().await.remove(&lobby.metadata.id);

    Ok(())
}

async fn mark_match_as_finished<Q: DeformQuicLogic>(
    server: &DeformQuicServer<Q>,
    lobby_id: u64,
) -> UserFacingResult<Q::UserLogic> {
    match server.matches.write().await.get_mut(&lobby_id) {
        Some(MatchInfo::Started(match_info)) => {
            match_info.game_ended.store(true, Ordering::SeqCst);
            Ok(())
        }
        _ => Err(UserFacingError::Deform(DeformError::InvalidState(
            "match does not exist or has already finished".into(),
        ))),
    }
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
