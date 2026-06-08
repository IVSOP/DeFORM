use anchor_lang::prelude::*;
use deform_core::lobby::Lobby;
use wincode::{SchemaRead, SchemaWrite};

use crate::deform::{GameState, Inputs};

// Since some types use manual serialization, we need to ensure the user can still use discriminators to check the account and to fetch it from the RPC
// u64 so it is the same size as anchor's discriminants
// FIX: somehow check that it is impossible for these to conflict with the discriminants from anchor
#[repr(u64)]
#[derive(SchemaRead, SchemaWrite)]
pub enum AccountTypes {
    Lobby = 0,
    Inputs = 1,
}

// This account is not marked with #[account] as using borsch is slow, and IDL generation means a lot of limitations towards our data types.
// This struct is for now exactly the same as Lobby, it is here so you can add aditional data if you want.
// WARN: serialization, deserialization, PDA derivation and checking must all be performed manually.
#[derive(SchemaRead, SchemaWrite)]
pub struct LobbyAccount {
    pub account_type: AccountTypes,
    pub lobby: Lobby<Inputs, GameState>
}
