use wincode::{SchemaRead, SchemaWrite};

use crate::{accounts::lobby::Lobby, DeformUserLogic};

pub mod inputs;
pub mod lobby;

// FIX: somehow check that it is impossible for these to conflict with the discriminants from anchor
// NOTE: #[repr(u64)] since that is anchor's discriminator size
#[repr(u64)]
#[derive(Clone, Debug, SchemaRead, SchemaWrite)]
pub enum DeformAccount<T: DeformUserLogic> {
    Lobby(Lobby<T>) = 0,
    Inputs = 1,
}
