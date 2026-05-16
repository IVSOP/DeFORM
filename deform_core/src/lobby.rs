use std::collections::HashMap;

use pinocchio::pubkey::Pubkey;
use wincode::{SchemaRead, SchemaWrite};

use crate::{DeformGameState, DeformInputs};

#[cfg_attr(not(target_arch = "bpf"), derive(serde::Serialize))]
#[derive(Clone, Copy, Eq, PartialEq, Default, SchemaRead, SchemaWrite)]
pub enum LobbyStatus {
    #[default]
    NotStarted = 0,
    Started = 1,
    Finished = 2,
}

#[derive(Clone, Copy, Eq, PartialEq, Default, SchemaRead, SchemaWrite)]
pub enum PLayerStatus {
    #[default]
    NotReady = 0,
    Ready = 1,
}

#[derive(Clone, SchemaRead, SchemaWrite)]
pub struct PlayerInfo<I: DeformInputs> {
    pub status: PLayerStatus,
    pub inputs: I,
}

/// An on-chain lobby account
// NOTE: Having a hashmap here is not really that efficient as you will also prob have one inside the gamestate. idk how to fix
#[derive(Clone, SchemaRead, SchemaWrite)]
// derive breaks if this is just DeformUserLogic
pub struct Lobby<I: DeformInputs, G: DeformGameState> {
    pub tick: u64,
    pub status: LobbyStatus,
    pub game_state: G,
    // TODO: serde correct serialization of pubkey
    pub player_infos: HashMap<Pubkey, PlayerInfo<I>>,
}

impl<I: DeformInputs, G: DeformGameState> Lobby<I, G> {
    pub fn find_program_address(id: u64, game: &Pubkey) -> (Pubkey, u8) {
        pinocchio::pubkey::find_program_address(&[b"lobby", &id.to_le_bytes()], game)
    }

    pub fn from_bytes(bytes: &[u8]) -> wincode::ReadResult<Self> {
        wincode::deserialize(bytes)
    }

    pub fn write_into(&self, dst: &mut [u8]) -> wincode::WriteResult<()> {
        wincode::serialize_into(dst, self)
    }
}

impl<T: crate::DeformUserLogic> From<Lobby<T::Inputs, T::GameState>> for crate::TickInfo<T> {
    fn from(lobby: Lobby<T::Inputs, T::GameState>) -> Self {
        crate::TickInfo {
            game_state: lobby.game_state,
            inputs: lobby.player_infos.into_iter().map(|(k, v)| (k, v.inputs)).collect(),
        }
    }
}
