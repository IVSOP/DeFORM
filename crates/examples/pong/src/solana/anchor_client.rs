use deform_core::{GameProgramClient, Pubkey, accounts::lobby::Lobby};
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

    fn create_lobby_ix(&self, user: Pubkey, lobby: Pubkey, id: u64) -> Instruction {
        CreateLobby {
            user,
            lobby,
            system_program: solana_system_interface::program::ID,
        }
        .instruction(CreateLobbyInstructionArgs { id })
    }

    fn join_lobby_ix(&self, user: Pubkey, lobby: Pubkey, id: u64) -> Instruction {
        JoinLobby {
            user,
            lobby,
            system_program: solana_system_interface::program::ID,
        }
        .instruction(JoinLobbyInstructionArgs { id })
    }

    fn ready_ix(&self, user: Pubkey, lobby: Pubkey, id: u64) -> Instruction {
        Ready { user, lobby }.instruction(ReadyInstructionArgs { id })
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
