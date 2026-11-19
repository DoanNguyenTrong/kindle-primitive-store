//! Primitive Store: the app store for the Kindle, as its own program. The launcher opens it from the library, the way
//! it opens Primitive Browser. It draws on the e-ink panel with the stock framework stopped. Done ends it, and the
//! launcher is back on the grid. Its catalog, installed apps and records are kept beside the launcher's (in
//! `/mnt/us/launcher/store`), so an app installed through the store is the same app the launcher shows.

mod store;

use std::{
    fs,
    path::{Path, PathBuf},
    process,
    sync::{atomic::Ordering, mpsc::channel, mpsc::Sender},
    thread,
    time::Duration,
};

use kl_core::{Action, DeviceCmd, Layout, Screen, Settings, Ui};
use kl_kindle::{device, display::Display, input, log, profile, Ev};
use kl_render::Renderer;

const W: u32 = profile::ACTIVE.screen_w;
const H: u32 = profile::ACTIVE.screen_h;

fn env_dir(name: &str, default: &str) -> PathBuf {
    PathBuf::from(std::env::var(name).unwrap_or_else(|_| default.to_string()))
}

fn font_paths(dir: &Path, run: &Path) -> Vec<String> {
    let names = ["NotoSans-Regular.ttf", "NotoSans-Bold.ttf"];
    let base = if run.join("fonts").join(names[0]).exists() { run.join("fonts") } else { dir.join("fonts") };
    names.iter().map(|n| base.join(n).to_string_lossy().into_owned()).collect()
}

fn main() {
    let dir = env_dir("KL_DIR", "/mnt/us/launcher");
    let run = env_dir("KL_RUN", "/var/tmp/kl-store");
    let _ = fs::create_dir_all(&run);
    log(&format!("store v{} dir {}", env!("CARGO_PKG_VERSION"), dir.display()));
    let root = dir.parent().unwrap_or(Path::new("/mnt/us")).to_path_buf();

    // An install that a power cut interrupted is finished or undone before the store reads what is installed.
    match kl_store::install::recover(&kl_store::install::Paths::under(&root)) {
        Ok(Some(what)) => log(&format!("store: {what}")),
        Ok(None) => {}
        Err(e) => log(&format!("store: cannot check an unfinished install: {e}")),
    }

    let settings = Settings::parse(&fs::read_to_string(dir.join("settings.txt")).unwrap_or_default());
    input::TURN.store(settings.rotation.turn(), Ordering::Relaxed);
    let mut renderer = match Renderer::new(W, H, &font_paths(&dir, &run)) {
        Ok(r) => r,
        Err(e) => {
            log(&format!("renderer: {e}"));
            process::exit(2);
        }
    };
    let mut ui = Ui::new(Vec::new(), Layout::new(W, H, 4, 5));
    ui.version = env!("CARGO_PKG_VERSION").to_string();
    ui.set_store(store::load_store(&dir));
    kl_render::load_store_pictures(&mut renderer, &dir.join("store/cache"));
    ui.open_store();

    let mut display = Display::open();
    let mut status = device::status(&settings);
    let (tx, rx) = channel::<Ev>();
    if let Some(path) = input::find(profile::ACTIVE.touch_name) {
        let t = tx.clone();
        thread::spawn(move || input::touch_loop(path, t));
    }
    if let Some(path) = input::find("gpio-keys") {
        let t = tx.clone();
        thread::spawn(move || input::button_loop(path, t));
    }
    {
        let t = tx.clone();
        thread::spawn(move || loop {
            thread::sleep(Duration::from_secs(1));
            if t.send(Ev::Tick).is_err() {
                break;
            }
        });
    }
    display.show(renderer.draw(&ui, &status), &settings, true);

    while let Ok(ev) = rx.recv() {
        let action = match ev {
            Ev::Input(input) => ui.handle(input),
            Ev::Tick => {
                let clock = status.clock.clone();
                status = device::status(&settings);
                if status.clock != clock {
                    Action::Redraw { full: false }
                } else {
                    Action::None
                }
            }
            Ev::StoreRefreshed { entries, message } => {
                kl_render::load_store_pictures(&mut renderer, &dir.join("store/cache"));
                ui.set_store(entries);
                ui.store_busy = false;
                if ui.screen == Screen::StoreApp {
                    ui.screen = Screen::Store;
                }
                ui.notice = Some(message);
                Action::Redraw { full: false }
            }
            Ev::StoreInstalled { entries, message } => {
                ui.set_store(entries);
                ui.store_installing = None;
                if ui.screen == Screen::StoreApp {
                    ui.screen = Screen::Store;
                }
                ui.notice = Some(message);
                Action::Redraw { full: false }
            }
            _ => Action::None,
        };
        // Done (back to the home screen) ends the store: the launcher takes the screen again.
        if ui.screen == Screen::Home {
            log("store: done");
            break;
        }
        match action {
            Action::Redraw { full } => display.show(renderer.draw(&ui, &status), &settings, full),
            Action::Device(cmd) => {
                device_command(cmd, &mut ui, &dir, &root, &tx, &status);
                display.show(renderer.draw(&ui, &status), &settings, false);
            }
            _ => {}
        }
    }
}

/// The store's own commands, run off the screen where they take time.
fn device_command(cmd: DeviceCmd, ui: &mut Ui, dir: &Path, root: &Path, tx: &Sender<Ev>, status: &kl_core::Status) {
    let connected = status.wifi_on && status.wifi_state == "connected";
    match cmd {
        DeviceCmd::StoreRefresh => {
            log(&format!("store: refresh requested (wifi on: {}, state: {})", status.wifi_on, status.wifi_state));
            if connected {
                store::start_store_refresh(tx.clone(), dir.to_path_buf());
                ui.notice = Some("Updating the store...".to_string());
            } else {
                ui.store_busy = false;
                ui.notice = Some("Connect to Wi-Fi to update the store.".to_string());
            }
        }
        DeviceCmd::StoreInstall(index) => match ui.store.get(index).map(|e| (e.id.clone(), e.name.clone())) {
            Some((id, name)) if connected => {
                store::start_store_install(tx.clone(), dir.to_path_buf(), id);
                ui.notice = Some(format!("Installing {name}..."));
            }
            Some(_) => {
                ui.store_installing = None;
                ui.notice = Some("Connect to Wi-Fi to install from the store.".to_string());
            }
            None => ui.store_installing = None,
        },
        DeviceCmd::StoreRemove(index) => {
            if let Some((id, name)) = ui.store.get(index).map(|e| (e.id.clone(), e.name.clone())) {
                match kl_store::install::remove(&kl_store::install::Paths::under(root), &id) {
                    Ok(()) => {
                        ui.set_store(store::load_store(dir));
                        ui.screen = Screen::Store;
                        ui.notice = Some(format!("Removed {name} from this Kindle."));
                        log(&format!("store: removed {id}"));
                    }
                    Err(e) => ui.notice = Some(format!("Could not remove {name}: {e}")),
                }
            }
        }
        _ => {}
    }
}
