//! Keep Store operations serialized while the supervisor controls the USB daemon.

use kl_core::storage_lease::StorageLease as SupervisorLease;
use std::{io, path::Path};

pub const CAPABILITY: &str = "KL_STORE_USB_LEASE";

pub struct StorageLease {
    controller: Option<SupervisorLease>,
    writing: bool,
}

impl StorageLease {
    pub fn new(run: &Path, enabled: bool) -> Self {
        Self {
            controller: enabled.then(|| SupervisorLease::new(run)),
            writing: false,
        }
    }

    pub fn managed(&self) -> bool {
        self.controller.is_some()
    }

    pub fn writing(&self) -> bool {
        self.writing
    }

    pub fn begin_write(&mut self) -> Result<(), String> {
        if self.writing {
            return Err("Wait for the current Store change to finish.".into());
        }
        if let Some(controller) = &mut self.controller {
            controller.begin_write().map_err(|error| {
                if error.kind() == io::ErrorKind::WouldBlock {
                    error.to_string()
                } else {
                    format!("Could not reserve Kindle storage. {error}")
                }
            })?;
        }
        self.writing = true;
        Ok(())
    }

    pub fn finish_write(&mut self) -> io::Result<()> {
        self.writing = false;
        if let Some(controller) = &mut self.controller {
            controller.release()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_legacy_supervisor_serializes_store_changes_without_a_mailbox() {
        let missing = Path::new("/var/tmp/primitive-store-test-unmanaged");
        let mut lease = StorageLease::new(missing, false);
        assert!(!lease.managed());
        assert!(!lease.writing());
        lease.begin_write().unwrap();
        assert!(lease.writing());
        assert!(lease.begin_write().is_err());
        lease.finish_write().unwrap();
        assert!(!lease.writing());
        lease.begin_write().unwrap();
    }
}
