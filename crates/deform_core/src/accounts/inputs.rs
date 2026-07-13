use std::collections::HashMap;

use solana_address::error::AddressError;
use wincode::{SchemaRead, SchemaWrite};

use crate::{DeformUserLogic, Pubkey};

#[cfg_attr(not(target_arch = "bpf"), derive(serde::Serialize))]
#[derive(Clone, Debug, SchemaRead, SchemaWrite)]
pub struct InputsAccount<T: DeformUserLogic> {
    pub bump: u8,
    pub lobby_id: u64,
    pub player: Pubkey,
    pub inputs: HashMap<u64, T::Inputs>,
}

impl<T: DeformUserLogic> InputsAccount<T> {
    pub fn new(lobby_id: u64, player: Pubkey, bump: u8) -> Self {
        Self {
            bump,
            lobby_id,
            player,
            inputs: HashMap::new(),
        }
    }

    pub fn find_program_address(lobby_id: u64, player: &Pubkey, game: &Pubkey) -> (Pubkey, u8) {
        Pubkey::find_program_address(
            &[b"inputs", &lobby_id.to_le_bytes(), player.as_array()],
            game,
        )
    }

    pub fn create_program_address(
        lobby_id: u64,
        player: &Pubkey,
        game: &Pubkey,
        bump: u8,
    ) -> Result<Pubkey, AddressError> {
        Pubkey::create_program_address(
            &[
                b"inputs",
                &lobby_id.to_le_bytes(),
                player.as_array(),
                &[bump],
            ],
            game,
        )
    }
}
