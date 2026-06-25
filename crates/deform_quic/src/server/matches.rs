use std::{collections::HashMap, sync::Arc};

use deform_core::{DeformUserLogic, Pubkey};
use tokio::sync::{Notify, broadcast, mpsc};
use wincode::{SchemaRead, SchemaWrite};

use crate::{DeformQuicLogic, ReliableMessage, SerializedUnreliableServerResponse};

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
    pub lobby_players: HashMap<u64, Pubkey>,
}
