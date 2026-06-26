use std::{collections::HashMap, sync::Arc, time::Duration};

use better_tokio_select::tokio_select;
use deform_core::{DeformUserLogic, Pubkey, lobby::LobbyData};
use tokio::sync::{Notify, broadcast, mpsc};
use wincode::{SchemaRead, SchemaWrite};

use crate::{
    DeformQuicLogic, ReliableMessage, SerializedUnreliableServerResponse, server::DeformQuicServer,
};

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

    /// Players which are expected, according to the lobby
    pub lobby_state: Arc<LobbyData<T>>,
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
        match_info: MatchInfo<T>,
        mut match_receiver: mpsc::Receiver<MatchMessage<T>>,
    ) -> anyhow::Result<()> {
        // inputs per-tick of each player
        // NOTE: a player existing in this map means the player is currently joined
        let mut players_data: HashMap<Pubkey, HashMap<u64, T::Inputs>> = HashMap::new();

        // always wait for the first player to join
        Self::wait_for_first_player(
            &mut match_receiver,
            &match_info.lobby_state,
            &mut players_data,
        )
        .await?;

        // TODO: depending on self.match_config, wait for all players to join
        match self.match_config {
            MatchConfig::WaitPlayers => {
                loop {
                    match match_receiver.recv().await {
                        Some(MatchMessage::PlayerJoined { pubkey }) => {
                            players_data.insert(pubkey, HashMap::new());

                            if players_data.len() == match_info.lobby_state.player_infos.len() {
                                // info!("Starting lobby {} (both players joined)", lobby_state.lobby);
                                break;
                            }
                        }
                        Some(_) => {}
                        None => {
                            anyhow::bail!(
                                "Match channel closed before start for lobby {}",
                                match_info.lobby_state.id
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

                                    if players_data.len()
                                        == match_info.lobby_state.player_infos.len()
                                    {
                                        // info!("Starting lobby {} (both players joined)", lobby_state.lobby);
                                        break;
                                    }
                                }
                                Some(_) => {}
                                None => {
                                    anyhow::bail!(
                                        "Match channel closed before start for lobby {}",
                                        match_info.lobby_state.id
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
