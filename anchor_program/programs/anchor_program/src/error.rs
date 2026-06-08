use anchor_lang::prelude::*;

#[error_code]
pub enum ErrorCode {
    #[msg("Failed to serialize lobby")]
    SerializeLobby,
    #[msg("Failed to deserialize lobby")]
    DeserializeLobby,
    #[msg("Invalid account type")]
    InvalidAccountType,
    #[msg("Invalid PDA")]
    InvalidPda,
    #[msg("Player already in lobby")]
    PlayerAlreadyInLobby,
    #[msg("Lobby is not accepting players")]
    LobbyNotJoinable,
}
