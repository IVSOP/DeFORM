use solana_address::error::AddressError;
use strum_macros::Display;
use wincode::{SchemaRead, SchemaWrite};

use crate::{
    accounts::lobby::{not_started::LobbyNotStarted, started::LobbyOngoing},
    DeformUserLogic, Pubkey,
};

pub mod not_started;
pub mod started;

#[cfg_attr(not(target_arch = "bpf"), derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(
    feature = "anchor",
    derive(anchor_lang::AnchorSerialize, anchor_lang::AnchorDeserialize),
    borsh(use_discriminant = true)
)]
#[derive(Clone, Copy, Eq, PartialEq, Default, SchemaRead, SchemaWrite, Display, Debug)]
pub enum PlayerStatus {
    #[default]
    NotReady = 0,
    Ready = 1,
}

#[cfg_attr(not(target_arch = "bpf"), derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(
    feature = "anchor",
    derive(anchor_lang::AnchorSerialize, anchor_lang::AnchorDeserialize),
    borsh(use_discriminant = true)
)]
#[cfg_attr(feature = "egui-probe", derive(egui_probe::EguiProbe), egui_probe(tags combobox))]
#[derive(Clone, Debug, Default, Eq, PartialEq, SchemaRead, SchemaWrite)]
pub enum MainnetRegion {
    Asia = 0,
    #[default]
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

#[cfg_attr(not(target_arch = "bpf"), derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(
    feature = "anchor",
    derive(anchor_lang::AnchorSerialize, anchor_lang::AnchorDeserialize),
    borsh(use_discriminant = true)
)]
#[cfg_attr(feature = "egui-probe", derive(egui_probe::EguiProbe), egui_probe(tags combobox))]
#[derive(Clone, Debug, Default, Eq, PartialEq, SchemaRead, SchemaWrite)]
pub enum DevnetRegion {
    Asia = 0,
    #[default]
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

#[cfg_attr(not(target_arch = "bpf"), derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(
    feature = "anchor",
    derive(anchor_lang::AnchorSerialize, anchor_lang::AnchorDeserialize),
    borsh(use_discriminant = true)
)]
#[cfg_attr(feature = "egui-probe", derive(egui_probe::EguiProbe), egui_probe(tags combobox))]
#[derive(Clone, Debug, Default, Eq, PartialEq, SchemaRead, SchemaWrite)]
pub enum LocalRegion {
    #[default]
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

#[cfg_attr(not(target_arch = "bpf"), derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(
    feature = "anchor",
    derive(anchor_lang::AnchorSerialize, anchor_lang::AnchorDeserialize),
    borsh(use_discriminant = true)
)]
#[cfg_attr(feature = "egui-probe", derive(egui_probe::EguiProbe), egui_probe(tags combobox))]
#[derive(Clone, Debug, Eq, PartialEq, SchemaRead, SchemaWrite)]
#[repr(u8)]
pub enum ValidatorNetwork {
    Mainnet(MainnetRegion) = 0,
    Devnet(DevnetRegion) = 1,
    Localhost(LocalRegion) = 2,
}

// All variants carry data, so `#[derive(Default)]` (which needs a unit default
// variant) doesn't apply. egui-probe needs this to construct the payload when the
// user switches `Network` to `FullyOnChain` in the picker.
impl Default for ValidatorNetwork {
    fn default() -> Self {
        ValidatorNetwork::Localhost(LocalRegion::Local)
    }
}

#[cfg_attr(not(target_arch = "bpf"), derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(
    feature = "anchor",
    derive(anchor_lang::AnchorSerialize, anchor_lang::AnchorDeserialize),
    borsh(use_discriminant = true)
)]
#[cfg_attr(feature = "egui-probe", derive(egui_probe::EguiProbe), egui_probe(tags combobox))]
#[derive(Clone, Debug, Default, Eq, PartialEq, SchemaRead, SchemaWrite)]
#[repr(u8)]
pub enum Network {
    // TODO: allow user to pass in a custom region??
    // adding a <N> here will make things messy in the lobby
    // maybe have it in DeformUserLogic or something
    #[default]
    Web2 = 0,
    FullyOnChain(ValidatorNetwork) = 1,
}

#[derive(Clone, Debug, SchemaRead, SchemaWrite)]
pub enum Lobby<T: DeformUserLogic> {
    NotStarted(LobbyNotStarted),
    Ongoing(LobbyOngoing<T>),
    Finished(LobbyOngoing<T>),
}

impl<T: DeformUserLogic> Lobby<T> {
    pub fn find_lobby_program_address(id: u64, game: &Pubkey) -> (Pubkey, u8) {
        Pubkey::find_program_address(&[b"lobby", &id.to_le_bytes()], game)
    }

    pub fn create_lobby_program_address(
        id: u64,
        game: &Pubkey,
        bump: u8,
    ) -> Result<Pubkey, AddressError> {
        Pubkey::create_program_address(&[b"lobby", &id.to_le_bytes(), &[bump]], game)
    }
}
