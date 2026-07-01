use solana_program_error::ProgramError;
use thiserror::Error;
use wincode::{SchemaRead, SchemaWrite};

use crate::DeformUserLogic;

// TODO: this is not very good
// quinn has good errors, like SendDatagramError, but I don't want the core library to import quinn. what to do?
#[derive(Clone, Debug, Error, serde::Serialize, SchemaRead, SchemaWrite)]
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
    InvalidState(String),

    #[error("lock poisoned")]
    LockPoisoned,

    #[error("channel closed")]
    ChannelClosed,

    #[error("rpc: {0}")]
    Rpc(String),

    #[error("io: {0}")]
    Io(String),

    #[error("serialize lobby: {0}")]
    SerializeLobby(String),

    #[error("deserialize lobby: {0}")]
    DeserializeLobby(String),

    #[error("backend panicked: {0}")]
    BackendPanicked(String),

    #[error("serialize: {0}")]
    Auth(String),
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
            DeformError::SerializeLobby(_) => 10,
            DeformError::DeserializeLobby(_) => 11,
            DeformError::BackendPanicked(_) => 12,
            DeformError::Auth(_) => 13,
        })
    }
}

impl From<std::io::Error> for DeformError {
    fn from(e: std::io::Error) -> Self {
        DeformError::Io(e.to_string())
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

#[derive(Clone, Debug, serde::Serialize, SchemaRead, SchemaWrite, thiserror::Error)]
pub enum UserFacingError<D: DeformUserLogic> {
    #[error("{0}")]
    Deform(DeformError),
    #[error("{0}")]
    User(D::Error),
}

impl<D: DeformUserLogic> From<DeformError> for UserFacingError<D> {
    fn from(e: DeformError) -> Self {
        UserFacingError::Deform(e)
    }
}

pub type UserFacingResult<D, T = ()> = Result<T, UserFacingError<D>>;
