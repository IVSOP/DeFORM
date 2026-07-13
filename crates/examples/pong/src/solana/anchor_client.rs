use deform_core::{
    Pubkey,
    accounts::{
        inputs::InputsAccount,
        lobby::{
            DevnetRegion, Lobby, LobbyFinished, LobbyMetadata, LobbyState, LocalRegion,
            MainnetRegion, Network, ValidatorNetwork, not_started::LobbyNotStarted,
        },
    },
    game_program_client::{GameProgramClient, ReadyArgs},
};
use ephemeral_rollups_sdk::{consts::DELEGATION_PROGRAM_ID, delegate_args::DelegateAccounts};
use solana_instruction::Instruction;

use crate::{
    generated::{
        instructions::{
            CreateLobby, CreateLobbyInstructionArgs, JoinLobby, JoinLobbyInstructionArgs, Ready,
            ReadyInstructionArgs, Start, StartInstructionArgs, WriteAndClose,
            WriteAndCloseInstructionArgs,
        },
        types::PlayerScore,
    },
    pong_logic::{PongError, PongGame},
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
        lobby: &Lobby<PongGame>,
    ) -> Result<Instruction, PongError> {
        let game_state = match &lobby.state {
            LobbyState::NotStarted(_) => Err(PongError::LobbyNotStarted)?,
            LobbyState::Ongoing(ongoing) => &ongoing.tick_info.game_state,
            LobbyState::Finished(LobbyFinished(finished)) => &finished.tick_info.game_state,
        };

        let scores = game_state
            .players
            .iter()
            .map(|s| PlayerScore {
                player: *s.0,
                score: s.1.score,
            })
            .collect();

        Ok(WriteAndClose {
            admin,
            lobby: lobby_pubkey,
            creator,
        }
        .instruction(WriteAndCloseInstructionArgs {
            id: lobby.metadata.id,
            scores,
        }))
    }

    fn start_ix(
        &self,
        user: Pubkey,
        lobby_pubkey: Pubkey,
        lobby_metadata: &LobbyMetadata,
        not_started: &LobbyNotStarted,
        game: Pubkey,
    ) -> Result<Instruction, PongError> {
        let lobby_delegation_accounts = DelegateAccounts::new(lobby_pubkey, game);

        let mut inputs_accounts = Vec::new();
        for user in not_started.player_status.keys() {
            let (inputs_account, _) =
                InputsAccount::<PongGame>::find_program_address(lobby_metadata.id, user, &game);

            let inputs_delegation_accounts = DelegateAccounts::new(lobby_pubkey, game);

            inputs_accounts.push(inputs_account);
            inputs_accounts.push(inputs_delegation_accounts.delegate_buffer);
            inputs_accounts.push(inputs_delegation_accounts.delegation_record);
            inputs_accounts.push(inputs_delegation_accounts.delegation_metadata);
        }

        Ok(Start {
            user,
            lobby: lobby_pubkey,
            owner_program: game,
            lobby_buffer: lobby_delegation_accounts.delegate_buffer,
            lobby_delegation_record: lobby_delegation_accounts.delegation_record,
            lobby_delegation_metadata: lobby_delegation_accounts.delegation_metadata,
            delegation_program: DELEGATION_PROGRAM_ID,
            system_program: solana_system_interface::program::ID,
        }
        .instruction_with_remaining_accounts(
            StartInstructionArgs {
                id: lobby_metadata.id,
            },
            &[],
        ))
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
