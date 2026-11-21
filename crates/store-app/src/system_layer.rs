//! Store under Primitive Launcher (`PRIMITIVE_LAUNCHER=1`), the system layer (`docs/system-architecture.md` in the
//! umbrella repo). The launcher owns sleep and wake, the lock screen, quick settings and the theme. Store.sh declares
//! `# Wake: redraw`, so the launcher sends SIGUSR1 when it wants the whole screen again (after a theme change or a
//! turn, or when it could not put the Store's screen back). Store then reads the theme again and draws everything
//! without a flash.

use std::{
    sync::atomic::{AtomicBool, Ordering},
    sync::mpsc::Sender,
    thread,
    time::Duration,
};

use kl_core::{Action, Input, Settings};
use kl_kindle::Ev;

/// Set by SIGUSR1: the launcher asks for the whole screen again, in the current theme.
static REDRAW: AtomicBool = AtomicBool::new(false);

extern "C" fn on_redraw(_: libc::c_int) {
    REDRAW.store(true, Ordering::Relaxed);
}

/// The launcher started Store.
pub fn under_launcher() -> bool {
    std::env::var("PRIMITIVE_LAUNCHER").as_deref() == Ok("1")
}

/// Take SIGUSR1 as the launcher's redraw request. Store.sh ignores the signal under the launcher, and an ignored
/// signal stays ignored through exec, so Store sets its own handler as early as it can.
pub fn take_signal() {
    // SAFETY: the handler only stores to an atomic, which is safe in a signal handler.
    unsafe { libc::signal(libc::SIGUSR1, on_redraw as *const () as libc::sighandler_t) };
}

/// Wake the main loop soon after a redraw request. The loop takes the request itself, before the event that woke it,
/// so nothing that waited behind the launcher's freeze is drawn in the old theme first. A tick is the event that
/// does the least.
pub fn wake_loop_on_request(tx: Sender<Ev>) {
    thread::spawn(move || {
        let mut woken = false;
        loop {
            thread::sleep(Duration::from_millis(150));
            let pending = REDRAW.load(Ordering::Relaxed);
            if pending && !woken && tx.send(Ev::Tick).is_err() {
                break;
            }
            woken = pending;
        }
    });
}

/// Whether the launcher asked for a redraw since the last call.
pub fn take_request() -> bool {
    REDRAW.swap(false, Ordering::Relaxed)
}

/// The swipe that opens the launcher's quick settings: down from the top edge, the launcher's own rule
/// (`opens_quick_settings` in kl-launcher: the status bar plus 40 px).
pub fn is_system_swipe(input: Input, status_bottom: i32) -> bool {
    matches!(input, Input::Swipe { dx, dy, start_y }
        if dy > 150.0 && dy.abs() > dx.abs() * 1.5 && start_y <= status_bottom as f32 + 40.0)
}

/// What Store does with the system layer's requests. Inactive (standalone, from the Kindle library) it changes
/// nothing.
#[derive(Debug, Default)]
pub struct SystemLayer {
    active: bool,
    redraw_on_wake: bool,
}

impl SystemLayer {
    pub fn new(active: bool) -> SystemLayer {
        SystemLayer { active, redraw_on_wake: false }
    }

    pub fn active(&self) -> bool {
        self.active
    }

    /// A redraw request was taken. Awake, Store draws now (true). Asleep, the sleep picture stays and Store draws
    /// when it wakes.
    pub fn draw_now(&mut self, asleep: bool) -> bool {
        if asleep {
            self.redraw_on_wake = true;
        }
        !asleep
    }

    /// Store woke. Under the launcher the screen was put back already: a second full update would only flash, so
    /// Store draws only when a redraw was asked for while it slept (with a full update, over the sleep picture).
    pub fn after_wake(&mut self, action: Action) -> Action {
        if !self.active || action == Action::None {
            return action;
        }
        if std::mem::take(&mut self.redraw_on_wake) {
            Action::Redraw { full: true }
        } else {
            Action::None
        }
    }

    /// The input is the system's, not Store's: the top-edge swipe opens the launcher's quick settings over Store.
    pub fn ignores(&self, input: Input, status_bottom: i32) -> bool {
        self.active && is_system_swipe(input, status_bottom)
    }
}

/// Follow the launcher's settings after a redraw request. The theme (and the low power and daily check choices) come
/// from the file. A turn comes from it too, unless Store follows the device's own turns. True when the screen
/// turned. Keeping `settings` current also means saving Store's own choice never writes an old theme back.
pub fn follow_settings(settings: &mut Settings, fresh: Settings) -> bool {
    let turned = !settings.auto_rotation && fresh.rotation != settings.rotation;
    let rotation = if turned { fresh.rotation } else { settings.rotation };
    *settings = Settings { rotation, auto_rotation: settings.auto_rotation, ..fresh };
    turned
}

#[cfg(test)]
mod tests {
    use super::*;
    use kl_core::Rotation;

    const STATUS_BOTTOM: i32 = 120;

    #[test]
    fn the_system_swipe_is_down_from_the_top_edge_only() {
        let swipe = |dx, dy, start_y| Input::Swipe { dx, dy, start_y };
        assert!(is_system_swipe(swipe(0.0, 400.0, 30.0), STATUS_BOTTOM));
        assert!(is_system_swipe(swipe(0.0, 400.0, 160.0), STATUS_BOTTOM), "the status bar plus 40 px");
        assert!(!is_system_swipe(swipe(0.0, 400.0, 161.0), STATUS_BOTTOM), "a scroll in Store");
        assert!(!is_system_swipe(swipe(0.0, -400.0, 30.0), STATUS_BOTTOM), "up is Store's");
        assert!(!is_system_swipe(swipe(400.0, 20.0, 30.0), STATUS_BOTTOM), "sideways is Store's");
        assert!(!is_system_swipe(swipe(0.0, 150.0, 30.0), STATUS_BOTTOM), "too short");
        assert!(!is_system_swipe(Input::Tap(10.0, 10.0), STATUS_BOTTOM));
    }

    #[test]
    fn only_the_launcher_takes_the_top_edge_swipe() {
        let swipe = Input::Swipe { dx: 0.0, dy: 400.0, start_y: 30.0 };
        assert!(SystemLayer::new(true).ignores(swipe, STATUS_BOTTOM));
        assert!(!SystemLayer::new(false).ignores(swipe, STATUS_BOTTOM), "standalone the swipe is Store's");
    }

    #[test]
    fn wake_under_the_launcher_redraws_only_when_asked_while_asleep() {
        let woke = Action::Redraw { full: true };
        let mut layer = SystemLayer::new(true);
        assert_eq!(layer.after_wake(woke.clone()), Action::None, "the launcher put the screen back");
        assert!(layer.draw_now(false), "awake: draw at once");
        assert_eq!(layer.after_wake(woke.clone()), Action::None);
        assert!(!layer.draw_now(true), "asleep: the sleep picture stays");
        assert_eq!(layer.after_wake(woke.clone()), woke);
        assert_eq!(layer.after_wake(woke.clone()), Action::None, "once per request");
        assert!(!layer.draw_now(true));
        assert_eq!(layer.after_wake(Action::None), Action::None, "a second wake event does nothing");
        assert_eq!(layer.after_wake(woke.clone()), woke, "the request waits for the real wake");
    }

    #[test]
    fn standalone_wake_is_unchanged() {
        let mut layer = SystemLayer::new(false);
        assert_eq!(layer.after_wake(Action::Redraw { full: true }), Action::Redraw { full: true });
        assert_eq!(layer.after_wake(Action::None), Action::None);
    }

    #[test]
    fn a_redraw_reads_the_theme_and_keeps_the_device_turn() {
        let mut settings = Settings { auto_rotation: false, store_daily_check: false, ..Settings::default() };
        let fresh = Settings { dark_mode: true, rotation: Rotation::UpsideDown, ..Settings::default() };
        assert!(follow_settings(&mut settings, fresh.clone()));
        assert!(settings.dark_mode);
        assert_eq!(settings.rotation, Rotation::UpsideDown);
        assert!(settings.store_daily_check, "the file's own choice");

        let mut turning = Settings { auto_rotation: true, ..Settings::default() };
        assert!(!follow_settings(&mut turning, Settings { auto_rotation: false, ..fresh }));
        assert!(turning.dark_mode);
        assert_eq!(turning.rotation, Rotation::Upright, "Store follows the device's turns itself");
        assert!(turning.auto_rotation, "the turn watch started with Store");
    }
}
