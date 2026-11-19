//! Coordinate writes to Kindle user storage with the launcher supervisor's USB daemon control.

use std::{
    fs, io,
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

pub const CAPABILITY: &str = "KL_STORE_USB_LEASE";
const REQUEST: &str = "store-storage-request";
const REQUEST_TMP: &str = "store-storage-request.tmp";
const REPLY: &str = "store-storage-reply";
const TIMEOUT: Duration = Duration::from_secs(5);
const POLL: Duration = Duration::from_millis(25);

pub struct StorageLease {
    controller: Option<Controller>,
    writing: bool,
}

struct Controller {
    run: PathBuf,
    pid: u32,
    sequence: u64,
    timeout: Duration,
}

impl StorageLease {
    pub fn new(run: &Path, enabled: bool) -> Self {
        let controller = enabled.then(|| Controller {
            run: run.to_path_buf(),
            pid: std::process::id(),
            sequence: 0,
            timeout: TIMEOUT,
        });
        Self {
            controller,
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
        self.begin_if(user_storage_is_mounted)
    }

    fn begin_if(&mut self, mounted: impl FnOnce() -> bool) -> Result<(), String> {
        if self.writing {
            return Err("Wait for the current Store change to finish.".into());
        }
        if let Some(controller) = &mut self.controller {
            let request = controller.pause();
            let result = match request {
                Ok(true) if mounted() => Ok(()),
                Ok(_) => Err("Eject the Kindle drive before changing the Store.".into()),
                Err(e) => Err(format!("Could not reserve Kindle storage. {e}")),
            };
            if let Err(e) = result {
                // Supersede a timed-out request in case its acknowledgment arrives late.
                let _ = controller.send("idle");
                return Err(e);
            }
        }
        self.writing = true;
        Ok(())
    }

    pub fn finish_write(&mut self) -> io::Result<()> {
        self.writing = false;
        if let Some(controller) = &mut self.controller {
            controller.send("idle")?;
        }
        Ok(())
    }
}

impl Controller {
    fn send(&mut self, state: &str) -> io::Result<String> {
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or_else(|| io::Error::other("USB request sequence exhausted"))?;
        let token = format!("{} {}", self.pid, self.sequence);
        let tmp = self.run.join(REQUEST_TMP);
        fs::write(&tmp, format!("{token} {state}\n"))?;
        fs::rename(tmp, self.run.join(REQUEST))?;
        Ok(token)
    }

    fn pause(&mut self) -> io::Result<bool> {
        let token = self.send("write")?;
        let paused = format!("{token} paused");
        let unavailable = format!("{token} unavailable");
        let deadline = Instant::now() + self.timeout;
        let reply = self.run.join(REPLY);
        loop {
            match fs::read_to_string(&reply) {
                Ok(line) if line.trim_end() == paused => return Ok(true),
                Ok(line) if line.trim_end() == unavailable => return Ok(false),
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "The launcher did not acknowledge the request.",
                ));
            }
            thread::sleep(POLL);
        }
    }
}

fn user_storage_is_mounted() -> bool {
    // /mnt/us is a FUSE layer that stays mounted when /mnt/base-us is handed to the computer.
    fs::read_to_string("/proc/mounts").is_ok_and(|mounts| {
        mounts
            .lines()
            .any(|line| line.split_whitespace().nth(1) == Some("/mnt/base-us"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let number = NEXT.fetch_add(1, Ordering::Relaxed);
            let run =
                std::env::temp_dir().join(format!("store-lease-{}-{number}", std::process::id()));
            fs::create_dir(&run).unwrap();
            Self(run)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn controller(lease: &mut StorageLease) -> &mut Controller {
        let controller = lease.controller.as_mut().unwrap();
        controller.pid = 42;
        controller.timeout = Duration::from_millis(125);
        controller
    }

    #[test]
    fn old_supervisor_uses_no_control_files() {
        let run = Fixture::new();
        let mut lease = StorageLease::new(&run.0, false);
        lease
            .begin_if(|| panic!("old supervisor is responsible for the mount"))
            .unwrap();
        assert!(lease.writing());
        assert!(lease.begin_if(|| true).is_err());
        lease.finish_write().unwrap();
        assert!(!lease.writing());
        assert_eq!(fs::read_dir(&run.0).unwrap().count(), 0);
    }

    #[test]
    fn a_matching_reply_acquires_storage_and_finish_releases_it() {
        let run = Fixture::new();
        let mut lease = StorageLease::new(&run.0, true);
        controller(&mut lease);
        fs::write(run.0.join(REPLY), "42 1 paused\n").unwrap();
        lease.begin_if(|| true).unwrap();
        assert!(lease.writing());
        assert_eq!(
            fs::read_to_string(run.0.join(REQUEST)).unwrap(),
            "42 1 write\n"
        );
        lease.finish_write().unwrap();
        assert!(!lease.writing());
        assert_eq!(
            fs::read_to_string(run.0.join(REQUEST)).unwrap(),
            "42 2 idle\n"
        );
    }

    #[test]
    fn an_old_reply_times_out_and_the_pending_request_is_released() {
        let run = Fixture::new();
        let mut lease = StorageLease::new(&run.0, true);
        controller(&mut lease);
        fs::write(run.0.join(REPLY), "41 1 paused\n").unwrap();
        let message = lease.begin_if(|| true).unwrap_err();
        assert!(message.contains("acknowledge"));
        assert!(!lease.writing());
        assert_eq!(
            fs::read_to_string(run.0.join(REQUEST)).unwrap(),
            "42 2 idle\n"
        );
    }

    #[test]
    fn an_unmounted_drive_stays_read_only_even_after_a_pause_reply() {
        let run = Fixture::new();
        let mut lease = StorageLease::new(&run.0, true);
        controller(&mut lease);
        fs::write(run.0.join(REPLY), "42 1 paused\n").unwrap();
        assert!(lease.begin_if(|| false).unwrap_err().contains("Eject"));
        assert!(!lease.writing());
        assert_eq!(
            fs::read_to_string(run.0.join(REQUEST)).unwrap(),
            "42 2 idle\n"
        );
    }
}
