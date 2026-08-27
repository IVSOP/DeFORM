use wincode::{SchemaRead, SchemaWrite};

use crate::{
    accounts::{inputs::InputsAccount, lobby::Lobby},
    DeformError, DeformResult, DeformUserLogic,
};

pub mod inputs;
pub mod lobby;

// FIX: somehow check that it is impossible for these to conflict with the discriminants from anchor
// NOTE: #[repr(u64)] since that is anchor's discriminator size
// FIX: as u64 here is very cursed, but I could not find another way as using From is not const
#[repr(u64)]
#[derive(Clone, Debug, SchemaRead, SchemaWrite)]
#[cfg_attr(not(target_arch = "bpf"), derive(serde::Serialize))]
pub enum DeformAccount<T: DeformUserLogic> {
    Lobby(Lobby<T>) = DeformAccountType::Lobby as u64,
    Inputs(InputsAccount<T>) = DeformAccountType::Inputs as u64,
}

// see the NOTE: in [`DeformAccount`]
/// This struct exists so I have a way of serializing the discriminants of each account by themselves, but is mostly a hack
#[repr(u64)]
#[derive(Clone, Debug, SchemaRead, SchemaWrite)]
pub enum DeformAccountType {
    Lobby = 0,
    Inputs = 1,
}

impl<T: DeformUserLogic> DeformAccount<T> {
    pub fn from_bytes(bytes: &[u8]) -> DeformResult<Self> {
        wincode::deserialize(bytes).map_err(|e| DeformError::DeserializeAccount(e.to_string()))
    }

    pub fn write_into(&self, dst: &mut [u8]) -> DeformResult<()> {
        wincode::serialize_into(dst, self).map_err(|e| DeformError::SerializeAccount(e.to_string()))
    }
}
