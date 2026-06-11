mod generated;

pub use generated::*;

use deform_core::{DeformGameState, DeformInputs, Pubkey};
use deform_program::{DeformProgramClient, LobbyAccount, PlayerScore, Result};
use generated::instructions::*;
use generated::types::PlayerScore as AnchorPlayerScore;
use solana_instruction::Instruction;

pub struct AnchorClient {
    program_id: Pubkey,
}

impl AnchorClient {
    pub fn new(program_id: Pubkey) -> Self {
        Self { program_id }
    }

    fn with_program_id(&self, mut ix: Instruction) -> Instruction {
        ix.program_id = self.program_id;
        ix
    }
}

impl<I: DeformInputs, G: DeformGameState> DeformProgramClient<I, G> for AnchorClient {
    fn program_id(&self) -> Pubkey {
        self.program_id
    }

    fn create_lobby(&self, user: Pubkey, lobby: Pubkey, id: u64) -> Result<Instruction> {
        let ix = CreateLobby {
            user,
            lobby,
            system_program: solana_system_interface::program::ID,
        }
        .instruction(CreateLobbyInstructionArgs { id });
        Ok(self.with_program_id(ix))
    }

    fn join_lobby(&self, user: Pubkey, lobby: Pubkey, id: u64) -> Result<Instruction> {
        let ix = JoinLobby {
            user,
            lobby,
            system_program: solana_system_interface::program::ID,
        }
        .instruction(JoinLobbyInstructionArgs { id });
        Ok(self.with_program_id(ix))
    }

    fn ready(&self, user: Pubkey, lobby: Pubkey, id: u64) -> Result<Instruction> {
        let ix = Ready { user, lobby }.instruction(ReadyInstructionArgs { id });
        Ok(self.with_program_id(ix))
    }

    fn write_and_close(
        &self,
        admin: Pubkey,
        lobby: Pubkey,
        creator: Pubkey,
        id: u64,
        scores: Vec<PlayerScore>,
    ) -> Result<Instruction> {
        let scores = scores
            .into_iter()
            .map(|s| AnchorPlayerScore {
                player: s.player,
                score: s.score,
            })
            .collect();
        let ix = WriteAndClose {
            admin,
            lobby,
            creator,
        }
        .instruction(WriteAndCloseInstructionArgs { id, scores });
        Ok(self.with_program_id(ix))
    }

    fn deserialize_lobby(&self, data: &[u8]) -> Result<LobbyAccount<I, G>> {
        LobbyAccount::from_bytes(data)
    }
}
