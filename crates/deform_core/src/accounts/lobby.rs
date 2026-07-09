use std::collections::HashMap;

use solana_address::error::AddressError;
use wincode::{SchemaRead, SchemaWrite};

use crate::{
    accounts::AccountType, DeformError, DeformInputs, DeformResult, DeformUserLogic, Pubkey,
    TickInfo,
};

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

#[cfg_attr(not(target_arch = "bpf"), derive(serde::Serialize))]
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

#[cfg_attr(not(target_arch = "bpf"), derive(serde::Serialize))]
#[cfg_attr(
    feature = "anchor",
    derive(anchor_lang::AnchorSerialize, anchor_lang::AnchorDeserialize)
)]
#[derive(Clone, SchemaRead, SchemaWrite)]
pub struct PlayerInfo<I: DeformInputs> {
    pub status: PLayerStatus,
    pub inputs: I,
}

#[cfg_attr(not(target_arch = "bpf"), derive(serde::Serialize))]
#[cfg_attr(
    feature = "anchor",
    derive(anchor_lang::AnchorSerialize, anchor_lang::AnchorDeserialize)
)]
#[cfg_attr(feature = "anchor", borsh(use_discriminant = true))]
#[derive(Clone, Eq, PartialEq, SchemaRead, SchemaWrite)]
pub enum MainnetRegion {
    Asia = 0,
    EU = 1,
    US = 2,
    TEE = 3,
}

impl MainnetRegion {
    pub fn address(&self) -> Pubkey {
        match self {
            MainnetRegion::Asia => {
                Pubkey::from_str_const("MAS1Dt9qreoRMQ14YQuhg8UTZMMzDdKhmkZMECCzk57")
            }
            MainnetRegion::EU => {
                Pubkey::from_str_const("MEUGGrYPxKk17hCr7wpT6s8dtNokZj5U2L57vjYMS8e")
            }
            MainnetRegion::US => {
                Pubkey::from_str_const("MUS3hc9TCw4cGC12vHNoYcCGzJG1txjgQLZWVoeNHNd")
            }
            MainnetRegion::TEE => {
                Pubkey::from_str_const("MTEWGuqxUpYZGFJQcp8tLN7x5v9BSeoFHYWQQ3n3xzo")
            }
        }
    }
}

#[cfg_attr(not(target_arch = "bpf"), derive(serde::Serialize))]
#[cfg_attr(
    feature = "anchor",
    derive(anchor_lang::AnchorSerialize, anchor_lang::AnchorDeserialize)
)]
#[cfg_attr(feature = "anchor", borsh(use_discriminant = true))]
#[derive(Clone, Eq, PartialEq, SchemaRead, SchemaWrite)]
pub enum DevnetRegion {
    Asia = 0,
    EU = 1,
    US = 2,
    TEE = 3,
}

impl DevnetRegion {
    pub fn address(&self) -> Pubkey {
        match self {
            DevnetRegion::Asia => {
                Pubkey::from_str_const("MAS1Dt9qreoRMQ14YQuhg8UTZMMzDdKhmkZMECCzk57")
            }
            DevnetRegion::EU => {
                Pubkey::from_str_const("MEUGGrYPxKk17hCr7wpT6s8dtNokZj5U2L57vjYMS8e")
            }
            DevnetRegion::US => {
                Pubkey::from_str_const("MUS3hc9TCw4cGC12vHNoYcCGzJG1txjgQLZWVoeNHNd")
            }
            DevnetRegion::TEE => {
                Pubkey::from_str_const("MTEWGuqxUpYZGFJQcp8tLN7x5v9BSeoFHYWQQ3n3xzo")
            }
        }
    }
}

#[cfg_attr(not(target_arch = "bpf"), derive(serde::Serialize))]
#[cfg_attr(
    feature = "anchor",
    derive(anchor_lang::AnchorSerialize, anchor_lang::AnchorDeserialize)
)]
#[cfg_attr(feature = "anchor", borsh(use_discriminant = true))]
#[derive(Clone, Eq, PartialEq, SchemaRead, SchemaWrite)]
pub enum LocalRegion {
    Local = 0,
}

impl LocalRegion {
    pub fn address(&self) -> Pubkey {
        match self {
            LocalRegion::Local => {
                Pubkey::from_str_const("mAGicPQYBMvcYveUZA5F5UNNwyHvfYh5xkLS2Fr1mev")
            }
        }
    }
}

#[cfg_attr(not(target_arch = "bpf"), derive(serde::Serialize))]
#[cfg_attr(
    feature = "anchor",
    derive(anchor_lang::AnchorSerialize, anchor_lang::AnchorDeserialize)
)]
#[cfg_attr(feature = "anchor", borsh(use_discriminant = true))]
#[derive(Clone, Eq, PartialEq, SchemaRead, SchemaWrite)]
#[repr(u8)]
pub enum ValidatorNetwork {
    Mainnet(MainnetRegion) = 0,
    Devnet(DevnetRegion) = 1,
    Localhost(LocalRegion) = 2,
}

#[cfg_attr(not(target_arch = "bpf"), derive(serde::Serialize))]
#[cfg_attr(
    feature = "anchor",
    derive(anchor_lang::AnchorSerialize, anchor_lang::AnchorDeserialize)
)]
#[cfg_attr(feature = "anchor", borsh(use_discriminant = true))]
#[derive(Clone, Eq, PartialEq, SchemaRead, SchemaWrite)]
#[repr(u8)]
pub enum Network {
    // TODO: allow user to pass in a custom region??
    // adding a <N> here will make things messy in the lobby
    // maybe have it in DeformUserLogic or something
    Web2 = 0,
    FullyOnChain(ValidatorNetwork) = 1,
}

// FIX: let the user pass in additional data as an arbitrary &U
/// An on-chain lobby account.
/// Serialized with wincode (not borsh), so it does not use `#[account]` in Anchor.
#[cfg_attr(not(target_arch = "bpf"), derive(serde::Serialize))]
#[derive(Clone, SchemaRead, SchemaWrite)]
pub struct Lobby<T: DeformUserLogic> {
    pub account_type: AccountType,
    pub id: u64,
    pub tick: u64,
    pub creator: Pubkey,
    pub status: LobbyStatus,
    pub network: Network,
    // FIX: serde correct serialization of pubkey
    pub player_infos: HashMap<Pubkey, PlayerInfo<T::Inputs>>,
    pub game_state: Option<T::GameState>,
    pub bump: u8,
}

impl<T: DeformUserLogic> Lobby<T> {
    pub fn new(
        id: u64,
        tick: u64,
        creator: Pubkey,
        status: LobbyStatus,
        network: Network,
        game_state: Option<T::GameState>,
        player_infos: HashMap<Pubkey, PlayerInfo<T::Inputs>>,
        bump: u8,
    ) -> Self {
        Self {
            account_type: AccountType::Lobby,
            id,
            tick,
            creator,
            status,
            network,
            game_state,
            player_infos,
            bump,
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

impl<T: DeformUserLogic> TryFrom<Lobby<T>> for TickInfo<T> {
    type Error = DeformError;

    fn try_from(lobby: Lobby<T>) -> Result<Self, Self::Error> {
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
