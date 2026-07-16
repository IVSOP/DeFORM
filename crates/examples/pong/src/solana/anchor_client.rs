use std::collections::HashMap;

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
use ephemeral_rollups_sdk::{
    consts::{DELEGATION_PROGRAM_ID, MAGIC_PROGRAM_ID},
    delegate_args::DelegateAccounts,
};
use magicblock_magic_program_api::{args::ScheduleTaskArgs, instruction::MagicBlockInstruction};
use solana_instruction::{AccountMeta, Instruction};

use crate::{
    generated::{
        instructions::{
            CreateLobby, CreateLobbyInstructionArgs, JoinLobby, JoinLobbyInstructionArgs, Ready,
            ReadyInstructionArgs, SetInputs, SetInputsInstructionArgs, Start, StartInstructionArgs,
            Tick, TickInstructionArgs, WriteAndClose, WriteAndCloseInstructionArgs,
        },
        types::PlayerScore,
    },
    pong_logic::{PongError, PongGame, PongInputs},
};

pub const GAME_PROGRAM: Pubkey = crate::generated::ANCHOR_PROGRAM_ID;

#[derive(Clone)]
pub struct PongAnchorClient;

// the tick micros of the pong game don't really matter here so I'll just make this generic
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
    ) -> Result<Instruction, PongError> {
        Ok(CreateLobby {
            user,
            lobby,
            system_program: solana_system_interface::program::ID,
        }
        .instruction(CreateLobbyInstructionArgs {
            id,
            network: network.into(),
        }))
    }

    fn join_lobby_ix(
        &self,
        user: Pubkey,
        lobby: Pubkey,
        id: u64,
    ) -> Result<Instruction, PongError> {
        Ok(JoinLobby {
            user,
            lobby,
            system_program: solana_system_interface::program::ID,
        }
        .instruction(JoinLobbyInstructionArgs { id }))
    }

    fn ready_ix(&self, args: ReadyArgs) -> Result<Instruction, PongError> {
        match args {
            ReadyArgs::Web2 { user, lobby, id } => Ok(Ready {
                user,
                lobby,
                inputs: None,
                system_program: solana_system_interface::program::ID,
            }
            .instruction(ReadyInstructionArgs { id })),
            ReadyArgs::FullyOnchain {
                user,
                lobby,
                id,
                inputs,
            } => Ok(Ready {
                user,
                lobby,
                inputs: Some(inputs),
                system_program: solana_system_interface::program::ID,
            }
            .instruction(ReadyInstructionArgs { id })),
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

            let inputs_delegation_accounts = DelegateAccounts::new(inputs_account, game);

            // TODO: some of these should be readonly
            inputs_accounts.push(AccountMeta::new(inputs_account, false));
            inputs_accounts.push(AccountMeta::new(
                inputs_delegation_accounts.delegate_buffer,
                false,
            ));
            inputs_accounts.push(AccountMeta::new(
                inputs_delegation_accounts.delegation_record,
                false,
            ));
            inputs_accounts.push(AccountMeta::new(
                inputs_delegation_accounts.delegation_metadata,
                false,
            ));
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
            &inputs_accounts,
        ))
    }

    fn set_inputs_ix(
        &self,
        user: Pubkey,
        inputs_account: Pubkey,
        lobby_account: Pubkey,
        lobby_id: u64,
        inputs: &HashMap<u64, PongInputs>,
    ) -> Result<Instruction, PongError> {
        let batch_inputs_bytes =
            wincode::serialize(inputs).map_err(|e| PongError::SerializeInputs(e.to_string()))?;

        Ok(SetInputs {
            user,
            lobby: lobby_account,
            inputs: inputs_account,
        }
        .instruction(SetInputsInstructionArgs {
            id: lobby_id,
            batch_inputs_bytes,
        }))
    }

    fn tick_ix(
        &self,
        lobby_account: Pubkey,
        lobby_id: u64,
        inputs_accounts: &[Pubkey],
    ) -> Result<Instruction, PongError> {
        // `tick` reads every player's inputs account (writable) as remaining
        // accounts, in the same lobby order the on-chain handler expects. No signer:
        // the crank executor drives it on the ephemeral rollup.
        let remaining: Vec<AccountMeta> = inputs_accounts
            .iter()
            .map(|inputs| AccountMeta::new(*inputs, false))
            .collect();

        Ok(Tick {
            lobby: lobby_account,
        }
        .instruction_with_remaining_accounts(TickInstructionArgs { id: lobby_id }, &remaining))
    }

    fn init_crank_ix(
        &self,
        payer: Pubkey,
        lobby_account: Pubkey,
        lobby_id: u64,
        inputs_accounts: &[Pubkey],
        execution_interval_millis: i64,
        iterations: i64,
    ) -> Result<Instruction, PongError> {
        // The scheduled `tick` is signerless — when the crank fires, the magic-program
        // executor runs it unattended, so nothing needs to sign the inner instruction.
        let tick_ix = self.tick_ix(lobby_account, lobby_id, inputs_accounts)?;

        // task_id is unique per lobby, so a lobby's crank can later be cancelled by id.
        let data = MagicBlockInstruction::ScheduleTask(ScheduleTaskArgs {
            task_id: lobby_id as i64,
            execution_interval_millis,
            iterations,
            instructions: vec![tick_ix],
        })
        .try_to_vec()
        .map_err(|e| PongError::ScheduleCrank(e.to_string()))?;

        // ScheduleTask accounts: payer (signer) followed by every account the
        // scheduled `tick` touches, so the validator can load them for the task.
        let mut accounts = Vec::with_capacity(2 + inputs_accounts.len());
        accounts.push(AccountMeta::new(payer, true));
        accounts.push(AccountMeta::new(lobby_account, false));
        for inputs in inputs_accounts {
            accounts.push(AccountMeta::new(*inputs, false));
        }

        Ok(Instruction {
            program_id: MAGIC_PROGRAM_ID,
            accounts,
            data,
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
