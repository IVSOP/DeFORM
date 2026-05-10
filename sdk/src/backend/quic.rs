use crate::{Client, SdkLogic};

struct QuicBackend<T: SdkLogic> {
    /// The user-provided logic struct. Used to execute the callbacks.
    /// Since we store the type, it is also possible for the user
    /// to pass in some arbitrary data, as well as mutate it inside of the callbacks.
    user_logic: T,



    // internal data this needs to have:
    // per-frame inputs
    // latest state
}

impl<T: SdkLogic> QuicBackend<T> {
    pub fn new() -> Client<T> {
        todo!()
    }
}
