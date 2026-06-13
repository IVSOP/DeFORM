use deform_core::{DeformError, DeformResult, Pubkey, lobby::Lobby};
use wincode::{SchemaRead, SchemaWrite};

use crate::pong_logic::PongGame;

// Since some types use manual serialization, we need to ensure the user can still use discriminators to check the account and to fetch it from the RPC
// u64 so it is the same size as anchor's discriminants
// FIX: somehow check that it is impossible for these to conflict with the discriminants from anchor
// TODO: put this in a lib crate?
#[repr(u64)]
#[derive(SchemaRead, SchemaWrite)]
pub enum AccountType {
    Lobby = 0,
    Inputs = 1,
}

// This account is not marked with #[account] as using borsch is slow, and IDL generation means a lot of limitations towards our data types.
// This struct is for now exactly the same as Lobby, it is here so you can add aditional data if you want.
// WARN: serialization, deserialization, PDA derivation and checking must all be performed manually.
#[derive(SchemaRead, SchemaWrite)]
pub struct LobbyAccount {
    pub account_type: AccountType,
    pub bump: u8,
    pub lobby: Lobby<PongGame>,
}

impl LobbyAccount {
    pub fn find_program_address(id: u64, game: &Pubkey) -> (Pubkey, u8) {
        Lobby::<PongGame>::find_program_address(id, game)
    }

    pub fn create_program_address(id: u64, bump: u8, game: &Pubkey) -> anchor_lang::Result<Pubkey> {
        Lobby::<PongGame>::create_program_address(id, game, bump)
            .map_err(|_| anchor_lang::error::ErrorCode::ConstraintSeeds.into())
    }

    pub fn from_bytes(bytes: &[u8]) -> DeformResult<Self> {
        wincode::deserialize(bytes).map_err(|e| DeformError::DeserializeLobby(e.to_string()))
    }

    pub fn write_into(&self, dst: &mut [u8]) -> DeformResult<()> {
        wincode::serialize_into(dst, self).map_err(|e| DeformError::SerializeLobby(e.to_string()))
    }
}
