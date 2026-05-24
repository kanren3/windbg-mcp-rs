use std::sync::{LazyLock, Mutex};

use windows::{
    Win32::System::Diagnostics::Debug::Extensions::{DebugCreate, IDebugClient},
};

struct SharedPrimaryClient(IDebugClient);

// DbgEng documents CreateClient as callable from any thread. We only keep the
// primary client to derive thread-local clients through that method.
unsafe impl Send for SharedPrimaryClient {}
unsafe impl Sync for SharedPrimaryClient {}

static PRIMARY_CLIENT: LazyLock<Mutex<Option<SharedPrimaryClient>>> =
    LazyLock::new(|| Mutex::new(None));

pub fn initialize_primary_client() -> Result<(), String> {
    let client = unsafe { DebugCreate::<IDebugClient>() }.map_err(|error| error.to_string())?;
    let mut state = PRIMARY_CLIENT
        .lock()
        .map_err(|_| "primary client state lock poisoned".to_string())?;
    *state = Some(SharedPrimaryClient(client));
    Ok(())
}

pub fn clear_primary_client() {
    if let Ok(mut state) = PRIMARY_CLIENT.lock() {
        state.take();
    }
}

pub fn create_client_from_primary() -> Result<IDebugClient, String> {
    let primary = {
        let state = PRIMARY_CLIENT
            .lock()
            .map_err(|_| "primary client state lock poisoned".to_string())?;
        state
            .as_ref()
            .map(|client| client.0.clone())
            .ok_or_else(|| "primary debug client is not initialized".to_string())?
    };

    unsafe { primary.CreateClient() }.map_err(|error| error.to_string())
}
