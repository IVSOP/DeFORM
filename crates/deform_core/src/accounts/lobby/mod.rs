use solana_address::error::AddressError;
use strum_macros::Display;
use wincode::{SchemaRead, SchemaWrite};

use crate::{
    accounts::lobby::{not_started::LobbyNotStarted, ongoing::LobbyOngoing},
    DeformUserLogic, Pubkey,
};

pub mod not_started;
pub mod ongoing;

/// RPC (HTTP) and PubSub (WebSocket) endpoints of the MagicBlock ephemeral rollup
/// that serves a given [`ValidatorNetwork`]. Send transactions to `rpc`; subscribe
/// to accounts on `ws`.
///
/// For hosted clusters these are the per-region "common entry" FQDNs; the router's
/// `getDelegationStatus` is the real source of truth for which validator currently
/// holds a delegated account, so treat these as sane defaults, not gospel.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ErEndpoints {
    pub rpc: &'static str,
    pub ws: &'static str,
}

#[cfg_attr(not(target_arch = "bpf"), derive(serde::Serialize))]
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

#[cfg_attr(not(target_arch = "bpf"), derive(serde::Serialize))]
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

    pub fn er_endpoints(&self) -> ErEndpoints {
        match self {
            MainnetRegion::Asia => ErEndpoints {
                rpc: "https://as.magicblock.app",
                ws: "wss://as.magicblock.app",
            },
            MainnetRegion::EU => ErEndpoints {
                rpc: "https://eu.magicblock.app",
                ws: "wss://eu.magicblock.app",
            },
            MainnetRegion::US => ErEndpoints {
                rpc: "https://us.magicblock.app",
                ws: "wss://us.magicblock.app",
            },
            MainnetRegion::TEE => ErEndpoints {
                rpc: "https://mainnet-tee-as.magicblock.app",
                ws: "wss://mainnet-tee-as.magicblock.app",
            },
        }
    }
}

#[cfg_attr(not(target_arch = "bpf"), derive(serde::Serialize))]
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

    pub fn er_endpoints(&self) -> ErEndpoints {
        match self {
            DevnetRegion::Asia => ErEndpoints {
                rpc: "https://devnet-as.magicblock.app",
                ws: "wss://devnet-as.magicblock.app",
            },
            DevnetRegion::EU => ErEndpoints {
                rpc: "https://devnet-eu.magicblock.app",
                ws: "wss://devnet-eu.magicblock.app",
            },
            DevnetRegion::US => ErEndpoints {
                rpc: "https://devnet-us.magicblock.app",
                ws: "wss://devnet-us.magicblock.app",
            },
            DevnetRegion::TEE => ErEndpoints {
                rpc: "https://devnet-tee-as.magicblock.app",
                ws: "wss://devnet-tee-as.magicblock.app",
            },
        }
    }
}

#[cfg_attr(not(target_arch = "bpf"), derive(serde::Serialize))]
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

    pub fn er_endpoints(&self) -> ErEndpoints {
        match self {
            // Ports match the docker-compose ER: JSON-RPC 7799, WebSocket 7800
            // (unlike hosted clusters, which serve both over the same wss host).
            LocalRegion::Local => ErEndpoints {
                rpc: "http://127.0.0.1:7799",
                ws: "ws://127.0.0.1:7800",
            },
        }
    }
}

#[cfg_attr(not(target_arch = "bpf"), derive(serde::Serialize))]
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

impl ValidatorNetwork {
    pub fn address(&self) -> Pubkey {
        match self {
            Self::Mainnet(m) => m.address(),
            Self::Devnet(d) => d.address(),
            Self::Localhost(l) => l.address(),
        }
    }

    /// The ephemeral-rollup RPC (HTTP) and PubSub (WebSocket) endpoints for this
    /// network+region. Send instructions to `.rpc`; subscribe to accounts on `.ws`.
    pub fn er_endpoints(&self) -> ErEndpoints {
        match self {
            Self::Mainnet(m) => m.er_endpoints(),
            Self::Devnet(d) => d.er_endpoints(),
            Self::Localhost(l) => l.er_endpoints(),
        }
    }
}

// All variants carry data, so `#[derive(Default)]` (which needs a unit default
// variant) doesn't apply. egui-probe needs this to construct the payload when the
// user switches `Network` to `FullyOnChain` in the picker.
impl Default for ValidatorNetwork {
    fn default() -> Self {
        ValidatorNetwork::Localhost(LocalRegion::Local)
    }
}

#[cfg_attr(not(target_arch = "bpf"), derive(serde::Serialize))]
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

#[cfg_attr(not(target_arch = "bpf"), derive(serde::Serialize))]
#[derive(Clone, Debug, SchemaRead, SchemaWrite)]
pub struct LobbyMetadata {
    pub id: u64,
    pub creator: Pubkey,
    pub network: Network,
    pub bump: u8,
}

#[cfg_attr(not(target_arch = "bpf"), derive(serde::Serialize))]
#[derive(Clone, Debug, SchemaRead, SchemaWrite)]
pub struct Lobby<T: DeformUserLogic> {
    pub metadata: LobbyMetadata,
    pub state: LobbyState<T>,
}

impl<T: DeformUserLogic> Lobby<T> {
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
}

#[cfg_attr(not(target_arch = "bpf"), derive(serde::Serialize))]
#[derive(Clone, Debug, SchemaRead, SchemaWrite)]
pub enum LobbyState<T: DeformUserLogic> {
    NotStarted(LobbyNotStarted),
    Ongoing(LobbyOngoing<T>),
    Finished(LobbyFinished<T>),
}

#[repr(transparent)]
#[derive(Clone, Debug, SchemaRead, SchemaWrite)]
#[cfg_attr(not(target_arch = "bpf"), derive(serde::Serialize))]
pub struct LobbyFinished<T: DeformUserLogic>(pub LobbyOngoing<T>);
