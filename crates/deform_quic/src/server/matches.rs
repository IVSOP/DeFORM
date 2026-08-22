use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use better_tokio_select::tokio_select;
use deform_core::{
    DeformError, DeformGameState, DeformInputs, DeformUserLogic, Pubkey, TickInfo,
    accounts::lobby::{
        Lobby, LobbyMetadata, LobbyState, not_started::LobbyNotStarted, ongoing::LobbyOngoing,
    },
    error::{UserFacingError, UserFacingResult},
};
use tokio::{
    sync::{broadcast, mpsc},
    time::interval,
};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::{
    CompressedSerializedUnreliableServerResponse, DeformQuicLogic, ReliableMessage,
    UnreliableServerResponse, datagram::compress, server::DeformQuicServer,
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

/// Messages sent internally from the match task to EVERY client-handling task (broadcast)
// FIX: this is all just very cursed
// I wanted to have the lobby be pre-compressed and serialized, which I think has to be done otherwise it gets too expensive
// but this erases the types completely. I need to change it to a transparent struct
// then I also use Arc<InternalServerBroadcast> but this doesn't end up helping much as I always need an owned Vec...
#[derive(Clone, Debug)]
pub enum InternalServerBroadcast<Q: DeformQuicLogic> {
    /// This looks a bit cursed but we are using a broadcast which then needs to send per-client data.
    /// The datagram also has shared data so we can avoid reserializing and recompressing for every client.
    /// This is why this struct is usually sent with an Arc in the broadcast channels!
    ///
    /// Also see [`UnreliableServerResponsePacket`]
    SendDatagrams {
        serialized_compressed_response: CompressedSerializedUnreliableServerResponse,
        player_inputs_buffers_len: HashMap<Pubkey, u8>,
    },
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
    pub state_sender: broadcast::Sender<Arc<InternalServerBroadcast<Q>>>,
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

    state_sender: broadcast::Sender<Arc<InternalServerBroadcast<Q>>>,

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

    let mut inputs = BTreeMap::new();
    for player in not_started.player_status.keys() {
        inputs.insert(
            *player,
            <Q::UserLogic as DeformUserLogic>::Inputs::default(),
        );
    }

    let mut lobby = Lobby {
        metadata: lobby_metadata,
        state: LobbyState::Ongoing(LobbyOngoing {
            slot: None,
            tick: 0,
            user_logic,
            tick_info: TickInfo { game_state, inputs },
        }),
    };

    // The game decides the rate, not this loop. Hardcoding 60 Hz here silently desynced
    // every game whose tick rate is anything else: the server advanced at its own pace
    // while the client simulated at the game's, so the client could never hold its lead
    // and every input arrived for a tick the server had already passed.
    let mut tick_timer = interval(Duration::from_micros(
        <Q::UserLogic as DeformUserLogic>::TICK_RATE_MICROS,
    ));

    // inputs last applied to the game state
    // used to predict new inputs if none are provided
    let mut last_applied_inputs: BTreeMap<Pubkey, <Q::UserLogic as DeformUserLogic>::Inputs> =
        BTreeMap::new();
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

    let mut player_inputs_buffers_len: HashMap<Pubkey, u8> =
        HashMap::with_capacity(players_data.len());
    // ensure every client gets the initial broadcasts even if they don't send any inputs
    for player in players_data.keys() {
        player_inputs_buffers_len.insert(*player, 0);
    }

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
                    // if there were no inputs, predict
                    if let Some(inputs) = player_inputs.get(&current_tick) {
                        last_applied_inputs.insert(*player, inputs.clone());
                    } else if let Some(stale) = last_applied_inputs.get_mut(player) {
                        *stale = stale.predict();
                    };

                    // remove old inputs, including from current tick since they have already been copied
                    player_inputs.retain(|k, _| *k > current_tick);

                    player_inputs_buffers_len
                        .insert(*player, player_inputs.len().min(u8::MAX as usize) as u8);
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
                if let Ok(serialized_message) = wincode::serialize(&message)
                    && let Ok(body) = compress(serialized_message, Q::COMPRESSION)
                {
                    let _ = state_sender.send(Arc::new(InternalServerBroadcast::SendDatagrams {
                        serialized_compressed_response:
                            CompressedSerializedUnreliableServerResponse(body),
                        player_inputs_buffers_len: player_inputs_buffers_len.clone(),
                    }));
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

    let _ = state_sender.send(Arc::new(InternalServerBroadcast::SendReliableMessage(
        ReliableMessage::Finish(lobby.clone()),
    )));
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
