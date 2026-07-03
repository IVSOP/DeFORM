use deform_core::Pubkey;

pub mod generated;
pub mod pong_logic;
pub mod solana;

// generated/ depends on crate::ANCHOR_PROGRAM_ID, which is related to this and not to generated/ itself which is a mess, but this fixes it
pub const ANCHOR_PROGRAM_ID: Pubkey = crate::generated::ANCHOR_PROGRAM_ID;
