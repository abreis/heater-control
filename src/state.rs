use alloc::{boxed::Box, format};
use arrayvec::ArrayString;
use core::ops::{Deref, DerefMut};
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, mutex::Mutex};
use embassy_time::{Duration, Instant, Timer};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{memlog, task::ssr_control::SsrDutyDynSender};

// Remotes must check in periodically or the heater shuts off.
pub const REMOTE_CHECKIN_INTERVAL: Duration = Duration::from_secs(60);
// How often to check for expired remotes.
pub const CHECKIN_EXPIRE_INTERVAL: Duration = Duration::from_secs(10);

pub type SharedState = &'static Mutex<NoopRawMutex, HeaterControlState>;

#[derive(Clone, Copy, Debug, Default)]
pub struct HeaterControlState {
    duty: u8,
    state: HeaterState,
}

#[derive(Clone, Copy, Debug, Default)]
pub enum HeaterState {
    #[default]
    Off,
    // The heater is being controlled by a remote.
    Remote {
        // An identifier for the remote that is actively controlling the heater.
        remote_id: ArrayString<8>,
        // Automatically turn off the heater if a remote has not been seen for some time.
        expires: embassy_time::Instant,
    },
    // The heater is being controlled manually.
    Manual,
}

impl Deref for HeaterControlState {
    type Target = HeaterState;

    fn deref(&self) -> &Self::Target {
        &self.state
    }
}
impl DerefMut for HeaterControlState {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.state
    }
}

pub fn init() -> SharedState {
    Box::leak(Box::new(Mutex::new(HeaterControlState::default())))
}

#[allow(dead_code)]
impl HeaterControlState {
    pub fn is_remote(&self) -> bool {
        matches!(self.state, HeaterState::Remote { .. })
    }

    pub fn is_manual(&self) -> bool {
        matches!(self.state, HeaterState::Manual { .. })
    }

    pub fn is_off(&self) -> bool {
        matches!(self.state, HeaterState::Off)
    }

    /// Returns the ID of the currently controlling remote, if any.
    pub fn remote_id(&self) -> Option<&str> {
        if let HeaterState::Remote { remote_id, .. } = &self.state {
            Some(remote_id.as_str())
        } else {
            None
        }
    }

    /// Transition to Off.
    ///
    /// This transition is always possible.
    pub fn transition_to_off(&mut self) {
        self.state = HeaterState::Off
    }

    /// Transition to Manual and set a duty cycle.
    ///
    /// This transition is always possible.
    pub fn transition_to_manual(&mut self, heater_duty: u8) {
        self.duty = heater_duty;
        self.state = HeaterState::Manual;
    }

    /// Transition to Remote.
    ///
    /// New remotes override active remotes, causing the former remote to
    /// receive an error message on the next update.
    pub fn transition_to_remote(&mut self, remote_id: impl Into<ArrayString<8>>) {
        self.state = HeaterState::Remote {
            remote_id: remote_id.into(),
            expires: Instant::now() + REMOTE_CHECKIN_INTERVAL,
        }
    }

    /// Updates the duty cycle set by a remote.
    ///
    /// Returns an error if the requesting remote is not the active remote,
    /// whether because it has failed to check in on time, or because another
    /// remote took possession.
    pub fn remote_update_duty(
        &mut self,
        remote_id: &str,
        heater_duty: u8,
    ) -> Result<(), StateError> {
        if let HeaterState::Remote {
            remote_id: current_remote,
            expires,
        } = &mut self.state
        {
            // See if the requesting remote is the one controlling the heater.
            if current_remote.as_str() != remote_id {
                return Err(StateError::RemoteMismatch);
            }

            // See if the expiry time has elapsed.
            // We use checked_duration_since because if `expires` is in the future, a regular duration
            // calculation would underflow since Duration is unsigned.
            if Instant::now().checked_duration_since(*expires).is_some() {
                return Err(StateError::RemoteExpired);
            }

            // Update the recorded duty.
            self.duty = heater_duty;

            // Set a new expiry time.
            *expires = Instant::now() + REMOTE_CHECKIN_INTERVAL;

            Ok(())
        } else {
            Err(StateError::RemoteNotInitialized)
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, Error)]
pub enum StateError {
    #[error("remote not initialized")]
    RemoteNotInitialized,
    #[error("the heater is being controlled by another remote")]
    RemoteMismatch,
    #[error("the remote failed to check in and has expired")]
    RemoteExpired,
}

// Periodically checks if a remote has expired, and sets the heater duty to zero.
#[embassy_executor::task]
pub async fn expire_remote(
    ssrcontrol_duty_sender: SsrDutyDynSender,
    memlog: memlog::SharedLogger,
    state: SharedState,
) {
    loop {
        Timer::after(CHECKIN_EXPIRE_INTERVAL).await;

        let mut state = state.lock().await;
        if let HeaterState::Remote { remote_id, expires } = **state {
            if Instant::now().checked_duration_since(expires).is_some() {
                ssrcontrol_duty_sender.send(0);
                state.transition_to_off();
                memlog.warn(format!("remote {remote_id} expired, duty set to 0"));
            }
        }
    }
}
