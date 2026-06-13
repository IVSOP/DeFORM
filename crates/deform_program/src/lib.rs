use deform_core::lobby::LobbyData;
use deform_core::{DeformGameState, DeformInputs, DeformUserLogic, Pubkey};
use solana_instruction::Instruction;
use wincode::{SchemaRead, SchemaWrite};

#[derive(Debug, thiserror::Error)]
pub enum ProgramClientError {
    #[error("Failed to deserialize lobby: {0}")]
    DeserializeLobby(String),
    #[error("Instruction build error: {0}")]
    InstructionBuild(String),
}

pub type Result<T> = std::result::Result<T, ProgramClientError>;

pub struct PlayerScore {
    pub player: Pubkey,
    pub score: u32,
}

#[repr(u64)]
#[derive(SchemaRead, SchemaWrite)]
pub enum AccountType {
    Lobby = 0,
    Inputs = 1,
}

#[doc(hidden)]
#[derive(SchemaRead, SchemaWrite)]
pub struct LobbyAccountData<I: DeformInputs, G: DeformGameState> {
    pub account_type: AccountType,
    pub bump: u8,
    pub lobby: LobbyData<I, G>,
}

/// A [`LobbyAccountData`] keyed off a [`DeformUserLogic`] `T`. Same rationale as
/// [`deform_core::lobby::Lobby`]: the derived struct stays generic over the *data types*
/// (so wincode's derive bounds land on them, satisfied by the `DeformInputs` /
/// `DeformGameState` supertraits), while this alias is the ergonomic spelling for code
/// that is generic over `T`.
pub type LobbyAccount<T> =
    LobbyAccountData<<T as DeformUserLogic>::Inputs, <T as DeformUserLogic>::GameState>;

impl<I: DeformInputs, G: DeformGameState> LobbyAccountData<I, G> {
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        wincode::deserialize(bytes).map_err(|e| ProgramClientError::DeserializeLobby(e.to_string()))
    }
}

pub trait DeformProgramClient {
    fn program_id(&self) -> Pubkey;

    fn find_lobby_address(&self, id: u64) -> (Pubkey, u8) {
        Pubkey::find_program_address(&[b"lobby", &id.to_le_bytes()], &self.program_id())
    }

    fn create_lobby(&self, user: Pubkey, lobby: Pubkey, id: u64) -> Result<Instruction>;

    fn join_lobby(&self, user: Pubkey, lobby: Pubkey, id: u64) -> Result<Instruction>;

    fn ready(&self, user: Pubkey, lobby: Pubkey, id: u64) -> Result<Instruction>;

    fn write_and_close(
        &self,
        admin: Pubkey,
        lobby: Pubkey,
        creator: Pubkey,
        id: u64,
        scores: Vec<PlayerScore>,
    ) -> Result<Instruction>;

    fn deserialize_lobby<T: DeformUserLogic>(&self, data: &[u8]) -> Result<LobbyAccount<T>>;
}
