use anchor_lang::prelude::*;

#[error_code]
pub enum GameError {
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
    #[msg("Player not in lobby")]
    PlayerNotInLobby,
    #[msg("Player already ready")]
    PlayerAlreadyReady,
    #[msg("Unauthorized")]
    Unauthorized,
    #[msg("Creator mismatch")]
    CreatorMismatch,
}
