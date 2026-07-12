use wincode::{SchemaRead, SchemaWrite};

use crate::{
    accounts::{inputs::InputsAccount, lobby::Lobby},
    DeformError, DeformResult, DeformUserLogic,
};

pub mod inputs;
pub mod lobby;

// FIX: somehow check that it is impossible for these to conflict with the discriminants from anchor
// NOTE: #[repr(u64)] since that is anchor's discriminator size
#[repr(u64)]
#[derive(Clone, Debug, SchemaRead, SchemaWrite)]
pub enum DeformAccount<T: DeformUserLogic> {
    Lobby(Lobby<T>) = 0,
    Inputs(InputsAccount<T>) = 1,
}

impl<T: DeformUserLogic> DeformAccount<T> {
    pub fn from_bytes(bytes: &[u8]) -> DeformResult<Self> {
        wincode::deserialize(bytes)
            .map_err(|e| DeformError::DeserializeInputsAccount(e.to_string()))
    }

    pub fn write_into(&self, dst: &mut [u8]) -> DeformResult<()> {
        wincode::serialize_into(dst, self)
            .map_err(|e| DeformError::SerializeInputsAccount(e.to_string()))
    }
}
