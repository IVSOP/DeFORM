use anchor_lang::prelude::*;

// TODO: lots of errors are repeated from the errors in deform_core
#[error_code]
pub enum GameProgramError {
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
    #[msg("Lobby account already initialized")]
    LobbyAlreadyInitialized,
    #[msg("Inputs account not provided")]
    MissingInputsAccount,
    #[msg("Inputs account already initialized")]
    InputsAccountAlreadyInitialized,
    #[msg("Failed to serialize inputs account")]
    SerializeInputsAccount,
    #[msg("Failed to deserialize inputs account")]
    DeserializeInputsAccount,
    #[msg("Serialized inputs exceed MAX_INPUTS_ACCOUNT_BYTES")]
    InputsAccountTooLarge,
    #[msg("Unauthorized")]
    Unauthorized,
    #[msg("Creator mismatch")]
    CreatorMismatch,
    #[msg("Address creation error")]
    AddressError,
    #[msg("Player is not ready")]
    PlayerNotReady,
    #[msg("Lobby is not fully on-chain")]
    NotFullyOnChain,
    #[msg("Lobby is not in a NotStarted state")]
    LobbyAlreadyStarted,
    #[msg("Could not init user logic")]
    InitUserLogic,
    #[msg("Could not init game state")]
    InitGameState,
    #[msg("Failed to delegate lobby to the ephemeral rollup")]
    DelegateLobby,
}
