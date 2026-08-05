// The single indirection that makes the program generic over the game: enable
// exactly one game feature and everything else refers to these three aliases.
#[cfg(any(
    all(feature = "pong", feature = "shooter"),
    all(feature = "pong", feature = "soccer"),
    all(feature = "shooter", feature = "soccer"),
))]
compile_error!("enable exactly one game feature: `pong`, `shooter`, or `soccer`");

#[cfg(feature = "pong")]
pub use pong::pong_logic::{
    PongGame as UserLogic, PongGameState as GameState, PongInputs as Inputs,
};

#[cfg(feature = "shooter")]
pub use shooter::shooter_logic::{
    ShooterGame as UserLogic, ShooterGameState as GameState, ShooterInputs as Inputs,
};

#[cfg(feature = "soccer")]
pub use soccer::soccer_logic::{
    SoccerGame as UserLogic, SoccerGameState as GameState, SoccerInputs as Inputs,
};
