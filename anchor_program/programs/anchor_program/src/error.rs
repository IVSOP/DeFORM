use anchor_lang::prelude::*;

#[error_code]
pub enum ErrorCode {
    #[msg("Failed to serialize lobby")]
    SerializeLobby,
}
