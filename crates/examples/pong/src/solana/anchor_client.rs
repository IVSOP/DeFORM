use deform_core::{
    Pubkey,
    accounts::lobby::{DevnetRegion, Lobby, LocalRegion, MainnetRegion, Network, ValidatorNetwork},
    game_program_client::{GameProgramClient, ReadyArgs},
};
use solana_instruction::Instruction;

use crate::{
    generated::{
        instructions::{
            CreateLobby, CreateLobbyInstructionArgs, JoinLobby, JoinLobbyInstructionArgs, Ready,
            ReadyInstructionArgs, WriteAndClose, WriteAndCloseInstructionArgs,
        },
        types::PlayerScore,
    },
    pong_logic::PongGame,
};

pub const GAME_PROGRAM: Pubkey = crate::generated::ANCHOR_PROGRAM_ID;

#[derive(Clone)]
pub struct PongAnchorClient;

// impl specific to PongGame!!
impl GameProgramClient<PongGame> for PongAnchorClient {
    fn game_program(&self) -> Pubkey {
        GAME_PROGRAM
    }

    fn create_lobby_ix(
        &self,
        user: Pubkey,
        lobby: Pubkey,
        id: u64,
        network: Network,
    ) -> Instruction {
        CreateLobby {
            user,
            lobby,
            system_program: solana_system_interface::program::ID,
        }
        .instruction(CreateLobbyInstructionArgs {
            id,
            network: network.into(),
        })
    }

    fn join_lobby_ix(&self, user: Pubkey, lobby: Pubkey, id: u64) -> Instruction {
        JoinLobby {
            user,
            lobby,
            system_program: solana_system_interface::program::ID,
        }
        .instruction(JoinLobbyInstructionArgs { id })
    }

    fn ready_ix(&self, args: ReadyArgs) -> Instruction {
        match args {
            ReadyArgs::Web2 { user, lobby, id } => Ready {
                user,
                lobby,
                inputs: None,
                system_program: solana_system_interface::program::ID,
            }
            .instruction(ReadyInstructionArgs { id }),
            ReadyArgs::FullyOnchain {
                user,
                lobby,
                id,
                inputs,
            } => Ready {
                user,
                lobby,
                inputs: Some(inputs),
                system_program: solana_system_interface::program::ID,
            }
            .instruction(ReadyInstructionArgs { id }),
        }
    }

    fn write_and_close_ix(
        &self,
        admin: Pubkey,
        lobby_pubkey: Pubkey,
        creator: Pubkey,
        lobby: Lobby<PongGame>,
    ) -> Instruction {
        let scores = lobby
            .game_state
            .unwrap()
            .players
            .iter()
            .map(|s| PlayerScore {
                player: *s.0,
                score: s.1.score,
            })
            .collect();
        WriteAndClose {
            admin,
            lobby: lobby_pubkey,
            creator,
        }
        .instruction(WriteAndCloseInstructionArgs {
            id: lobby.id,
            scores,
        })
    }
}

// Codama regenerates a structurally-identical copy of `Network` (and its nested
// types) from the IDL. These `From` impls bridge the canonical `deform_core`
// types to the generated ones so callers only ever deal with `deform_core::Network`.
// The Borsh wire format matches because the `deform_core` discriminants are 0,1,2..
// in positional order, which is exactly what codama's default encoding produces.
impl From<Network> for crate::generated::types::Network {
    fn from(network: Network) -> Self {
        match network {
            Network::Web2 => Self::Web2,
            Network::FullyOnChain(v) => Self::FullyOnChain(v.into()),
        }
    }
}

impl From<ValidatorNetwork> for crate::generated::types::ValidatorNetwork {
    fn from(network: ValidatorNetwork) -> Self {
        match network {
            ValidatorNetwork::Mainnet(r) => Self::Mainnet(r.into()),
            ValidatorNetwork::Devnet(r) => Self::Devnet(r.into()),
            ValidatorNetwork::Localhost(r) => Self::Localhost(r.into()),
        }
    }
}

impl From<MainnetRegion> for crate::generated::types::MainnetRegion {
    fn from(region: MainnetRegion) -> Self {
        match region {
            MainnetRegion::Asia => Self::Asia,
            MainnetRegion::EU => Self::EU,
            MainnetRegion::US => Self::US,
            MainnetRegion::TEE => Self::TEE,
        }
    }
}

impl From<DevnetRegion> for crate::generated::types::DevnetRegion {
    fn from(region: DevnetRegion) -> Self {
        match region {
            DevnetRegion::Asia => Self::Asia,
            DevnetRegion::EU => Self::EU,
            DevnetRegion::US => Self::US,
            DevnetRegion::TEE => Self::TEE,
        }
    }
}

impl From<LocalRegion> for crate::generated::types::LocalRegion {
    fn from(region: LocalRegion) -> Self {
        match region {
            LocalRegion::Local => Self::Local,
        }
    }
}
