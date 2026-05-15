use crate::{Client, UserLogic};

struct QuicBackend<T: UserLogic> {
    /// The user-provided logic struct. Used to execute the callbacks.
    /// Since we store the type, it is also possible for the user
    /// to pass in some arbitrary data, as well as mutate it inside of the callbacks.
    user_logic: T,



    // internal data this needs to have:
    // per-frame inputs
    // latest state
}

impl<T: UserLogic> QuicBackend<T> {
    pub fn new() -> Client<T> {
        todo!()
    }
}
