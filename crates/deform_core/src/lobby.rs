use std::collections::HashMap;

use solana_address::error::AddressError;
use wincode::{SchemaRead, SchemaWrite};

use crate::{DeformError, DeformInputs, DeformResult, DeformUserLogic, Pubkey, TickInfo};

#[cfg_attr(not(target_arch = "bpf"), derive(serde::Serialize))]
#[cfg_attr(
    feature = "anchor",
    derive(anchor_lang::AnchorSerialize, anchor_lang::AnchorDeserialize)
)]
#[cfg_attr(feature = "anchor", borsh(use_discriminant = true))]
#[derive(Clone, Copy, Eq, PartialEq, Default, SchemaRead, SchemaWrite)]
pub enum LobbyStatus {
    #[default]
    NotStarted = 0,
    Started = 1,
    Finished = 2,
}

#[cfg_attr(
    feature = "anchor",
    derive(anchor_lang::AnchorSerialize, anchor_lang::AnchorDeserialize)
)]
#[cfg_attr(feature = "anchor", borsh(use_discriminant = true))]
#[derive(Clone, Copy, Eq, PartialEq, Default, SchemaRead, SchemaWrite)]
pub enum PLayerStatus {
    #[default]
    NotReady = 0,
    Ready = 1,
}

#[cfg_attr(
    feature = "anchor",
    derive(anchor_lang::AnchorSerialize, anchor_lang::AnchorDeserialize)
)]
#[derive(Clone, SchemaRead, SchemaWrite)]
pub struct PlayerInfo<I: DeformInputs> {
    pub status: PLayerStatus,
    pub inputs: I,
}

/// An on-chain lobby account.
/// Serialized with wincode (not borsh), so it does not use `#[account]` in Anchor.
#[derive(Clone, SchemaRead, SchemaWrite)]
pub struct LobbyData<T: DeformUserLogic> {
    pub id: u64,
    pub tick: u64,
    pub creator: Pubkey,
    pub status: LobbyStatus,
    // FIX: serde correct serialization of pubkey
    pub player_infos: HashMap<Pubkey, PlayerInfo<T::Inputs>>,
    // TODO: this is very messy for many reasons, but I don't see another way
    // for users to for example select powerups, I will need to let the user:
    // - store additional data in this lobby data, or in its own wrapper (probably better)
    // - pass in arbitrary data into GameState::new(). Allow passing in whatever &T.
    pub game_state: Option<T::GameState>,
}

impl<T: DeformUserLogic> LobbyData<T> {
    pub fn new(
        id: u64,
        tick: u64,
        creator: Pubkey,
        status: LobbyStatus,
        game_state: Option<T::GameState>,
        player_infos: HashMap<Pubkey, PlayerInfo<T::Inputs>>,
    ) -> Self {
        Self {
            id,
            tick,
            creator,
            status,
            game_state,
            player_infos,
        }
    }

    pub fn find_program_address(id: u64, game: &Pubkey) -> (Pubkey, u8) {
        Pubkey::find_program_address(&[b"lobby", &id.to_le_bytes()], game)
    }

    pub fn create_program_address(
        id: u64,
        game: &Pubkey,
        bump: u8,
    ) -> Result<Pubkey, AddressError> {
        Pubkey::create_program_address(&[b"lobby", &id.to_le_bytes(), &[bump]], game)
    }

    pub fn from_bytes(bytes: &[u8]) -> DeformResult<Self> {
        wincode::deserialize(bytes).map_err(|e| DeformError::DeserializeLobby(e.to_string()))
    }

    pub fn write_into(&self, dst: &mut [u8]) -> DeformResult<()> {
        wincode::serialize_into(dst, self).map_err(|e| DeformError::SerializeLobby(e.to_string()))
    }
}

impl<T: DeformUserLogic> TryFrom<LobbyData<T>> for TickInfo<T> {
    type Error = DeformError;

    fn try_from(lobby: LobbyData<T>) -> Result<Self, Self::Error> {
        Ok(TickInfo {
            game_state: lobby
                .game_state
                .ok_or(DeformError::InvalidState("game not started".into()))?,
            inputs: lobby
                .player_infos
                .into_iter()
                .map(|(k, v)| (k, v.inputs))
                .collect(),
        })
    }
}
