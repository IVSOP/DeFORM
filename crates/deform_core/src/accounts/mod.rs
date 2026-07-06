use serde::{Deserialize, Serialize};
use wincode::{SchemaRead, SchemaWrite};

pub mod inputs;
pub mod lobby;

// FIX: somehow check that it is impossible for these to conflict with the discriminants from anchor
#[repr(u64)]
#[derive(Clone, Debug, Serialize, Deserialize, SchemaRead, SchemaWrite)]
pub enum AccountType {
    Lobby = 0,
    Inputs = 1,
}
