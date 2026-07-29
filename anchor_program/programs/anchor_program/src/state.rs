// The single indirection that makes the program generic over the game: enable
// exactly one game feature and everything else refers to these three aliases.
#[cfg(all(feature = "pong", feature = "shooter"))]
compile_error!("enable exactly one game feature: `pong` or `shooter`");

#[cfg(feature = "pong")]
pub use pong::pong_logic::{
    PongGame as UserLogic, PongGameState as GameState, PongInputs as Inputs,
};

#[cfg(feature = "shooter")]
pub use shooter::shooter_logic::{
    ShooterGame as UserLogic, ShooterGameState as GameState, ShooterInputs as Inputs,
};
