use anyhow::Result;
use deform_core::Pubkey;
use solana_instruction::Instruction;

use crate::{
    generated::{
        instructions::{
            CreateLobby, CreateLobbyInstructionArgs, JoinLobby, JoinLobbyInstructionArgs, Ready,
            ReadyInstructionArgs, WriteAndClose, WriteAndCloseInstructionArgs,
        },
        types::PlayerScore,
    },
    solana::accounts::LobbyAccount,
};

pub struct AnchorClient {
    pub program_id: Pubkey,
}

impl AnchorClient {
    pub fn create_lobby(&self, user: Pubkey, lobby: Pubkey, id: u64) -> Instruction {
        CreateLobby {
            user,
            lobby,
            system_program: solana_system_interface::program::ID,
        }
        .instruction(CreateLobbyInstructionArgs { id })
    }

    pub fn join_lobby(&self, user: Pubkey, lobby: Pubkey, id: u64) -> Instruction {
        JoinLobby {
            user,
            lobby,
            system_program: solana_system_interface::program::ID,
        }
        .instruction(JoinLobbyInstructionArgs { id })
    }

    pub fn ready(&self, user: Pubkey, lobby: Pubkey, id: u64) -> Instruction {
        Ready { user, lobby }.instruction(ReadyInstructionArgs { id })
    }

    pub fn write_and_close(
        &self,
        admin: Pubkey,
        lobby: Pubkey,
        creator: Pubkey,
        id: u64,
        scores: Vec<PlayerScore>,
    ) -> Instruction {
        let scores = scores
            .into_iter()
            .map(|s| PlayerScore {
                player: s.player,
                score: s.score,
            })
            .collect();
        WriteAndClose {
            admin,
            lobby,
            creator,
        }
        .instruction(WriteAndCloseInstructionArgs { id, scores })
    }

    pub fn deserialize_lobby(&self, data: &[u8]) -> Result<LobbyAccount> {
        Ok(LobbyAccount::from_bytes(data)?)
    }
}
