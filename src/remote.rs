use crate::state::StateError;
use arrayvec::ArrayString;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub enum RemoteControlRequest {
    Init { id: ArrayString<8> },
    UpdateDuty { id: ArrayString<8>, duty: u8 },
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub enum RemoteControlResponse {
    Ok,
    Error(RemoteControlError),
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, Error)]
pub enum RemoteControlError {
    #[error("{0}")]
    StateError(#[from] StateError),
}
