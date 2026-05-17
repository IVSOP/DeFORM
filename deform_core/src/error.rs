use pinocchio::program_error::ProgramError;
use thiserror::Error;

fn serialize_as_display<T: std::fmt::Display, S: serde::Serializer>(
    value: &T,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(&value.to_string())
}

#[derive(Debug, Error, serde::Serialize)]
#[non_exhaustive]
pub enum DeformError {
    #[error("serialize: {0}")]
    Serialize(String),

    #[error("deserialize: {0}")]
    Deserialize(String),

    #[error("{0}")]
    Connection(String),

    #[error("{0}")]
    Protocol(String),

    #[error("{0}")]
    InvalidState(&'static str),

    #[error("lock poisoned")]
    LockPoisoned,

    #[error("channel closed")]
    ChannelClosed,

    #[error("rpc: {0}")]
    Rpc(String),

    #[error("io: {0}")]
    Io(
        #[serde(serialize_with = "serialize_as_display")]
        #[from]
        std::io::Error,
    ),

    // TODO: ugly, but I can't figure out an alternative
    #[error("user logic: {0}")]
    UserLogic(
        #[serde(serialize_with = "serialize_as_display")] Box<dyn std::error::Error + Send + Sync>,
    ),

    #[error("serialize lobby: {0}")]
    SerializeLobby(String),

    #[error("deserialize lobby: {0}")]
    DeserializeLobby(String),

    #[error("backend panicked: {0}")]
    BackendPanicked(String),
}

pub type DeformResult<T = ()> = Result<T, DeformError>;

impl From<DeformError> for ProgramError {
    fn from(e: DeformError) -> Self {
        ProgramError::Custom(match e {
            DeformError::Serialize(_) => 0,
            DeformError::Deserialize(_) => 1,
            DeformError::Connection(_) => 2,
            DeformError::Protocol(_) => 3,
            DeformError::InvalidState(_) => 4,
            DeformError::LockPoisoned => 5,
            DeformError::ChannelClosed => 6,
            DeformError::Rpc(_) => 8,
            DeformError::Io(_) => 9,
            DeformError::UserLogic(_) => 10,
            DeformError::SerializeLobby(_) => 11,
            DeformError::DeserializeLobby(_) => 12,
            DeformError::BackendPanicked(_) => 13,
        })
    }
}

impl From<wincode::WriteError> for DeformError {
    fn from(e: wincode::WriteError) -> Self {
        DeformError::Serialize(format!("{e:?}"))
    }
}

impl From<wincode::ReadError> for DeformError {
    fn from(e: wincode::ReadError) -> Self {
        DeformError::Deserialize(format!("{e:?}"))
    }
}
