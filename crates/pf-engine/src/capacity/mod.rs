//! Capacity: slots for model sessions and for cold builds (ADR-0009).

use std::sync::Arc;

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// Capacity settings from the workflow.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Settings {
    /// Slots for model sessions. The default is 2.
    pub model_sessions: u32,
    /// Slots for cold builds. The default is 1.
    pub cold_builds: u32,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            model_sessions: 2,
            cold_builds: 1,
        }
    }
}

/// Why a capacity setting is not usable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SettingsError(pub String);

impl std::fmt::Display for SettingsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for SettingsError {}

/// One held slot. The slot is free again when this value is dropped, and the next waiting
/// holder takes it (waiters are served in the order they came).
#[derive(Debug)]
pub struct Slot {
    _permit: OwnedSemaphorePermit,
}

/// The slots of one run.
#[derive(Clone, Debug)]
pub struct Capacity {
    model_sessions: Arc<Semaphore>,
    cold_builds: Arc<Semaphore>,
}

impl Capacity {
    /// Build the slots from workflow settings. A setting of 0 would wait for ever, so it is refused.
    pub fn new(settings: &Settings) -> Result<Capacity, SettingsError> {
        if settings.model_sessions == 0 || settings.cold_builds == 0 {
            return Err(SettingsError(
                "capacity settings must be at least 1".to_string(),
            ));
        }
        Ok(Capacity {
            model_sessions: Arc::new(Semaphore::new(settings.model_sessions as usize)),
            cold_builds: Arc::new(Semaphore::new(settings.cold_builds as usize)),
        })
    }
}

impl Capacity {
    /// Wait for a slot for a model session.
    pub async fn acquire_model_session(&self) -> Slot {
        Self::acquire(&self.model_sessions).await
    }

    /// Take a model session slot now, or return `None` when all are held.
    pub fn try_acquire_model_session(&self) -> Option<Slot> {
        Self::try_acquire(&self.model_sessions)
    }

    /// Wait for the slot for a cold build.
    pub async fn acquire_cold_build(&self) -> Slot {
        Self::acquire(&self.cold_builds).await
    }

    /// Take a cold build slot now, or return `None` when all are held.
    pub fn try_acquire_cold_build(&self) -> Option<Slot> {
        Self::try_acquire(&self.cold_builds)
    }

    pub fn free_model_sessions(&self) -> usize {
        self.model_sessions.available_permits()
    }

    pub fn free_cold_builds(&self) -> usize {
        self.cold_builds.available_permits()
    }

    async fn acquire(semaphore: &Arc<Semaphore>) -> Slot {
        // The semaphores are never closed, so `acquire_owned` cannot fail.
        let permit = semaphore
            .clone()
            .acquire_owned()
            .await
            .expect("a capacity semaphore is never closed");
        Slot { _permit: permit }
    }

    fn try_acquire(semaphore: &Arc<Semaphore>) -> Option<Slot> {
        semaphore
            .clone()
            .try_acquire_owned()
            .ok()
            .map(|permit| Slot { _permit: permit })
    }
}
