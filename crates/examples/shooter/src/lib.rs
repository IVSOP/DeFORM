use deform_core::Pubkey;

pub mod generated;
pub mod shooter_logic;
pub mod solana;

#[cfg(feature = "physics")]
pub mod physics_sim;

// generated/ depends on crate::ANCHOR_PROGRAM_ID, which is related to this and not to generated/ itself which is a mess, but this fixes it
pub const ANCHOR_PROGRAM_ID: Pubkey = crate::generated::ANCHOR_PROGRAM_ID;
