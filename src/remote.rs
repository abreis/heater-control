use crate::state::StateError;
use arrayvec::ArrayString;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub enum RemoteControlRequest {
    Ping,
    Init { id: ArrayString<8> },
    UpdateDuty { id: ArrayString<8>, duty: u8 },
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub enum RemoteControlResponse {
    Ok,
    Error(StateError),
}

// #[derive(Clone, Copy, Debug, Serialize, Deserialize, Error)]
// pub enum RemoteControlError {
//     #[error("{0}")]
//     StateError(#[from] StateError),
// }
