//! Store: the app store for the Kindle, as its own program. The launcher opens it from the library, the way
//! it opens Primitive Browser. It draws on the e-ink panel with the stock framework stopped. Done ends it, and the
//! launcher is back on the grid. Its catalog, installed apps and records are kept beside the launcher's (in
//! `/mnt/us/launcher/store`), so an app installed through the store is the same app the launcher shows.

mod crash_log;
mod requirements;
mod storage_lease;
mod store;

use std::{
    fs,
    path::{Path, PathBuf},
    process,
    sync::{atomic::Ordering, mpsc::channel, mpsc::Sender},
    thread,
    time::{Duration, Instant, SystemTime},
};

use kl_core::store::StoreEntry;
use kl_core::store_open;
use kl_core::ui::StoreRequirementReview;
use kl_core::{Action, DeviceCmd, Input, Layout, Rotation, Screen, Settings, Status, Ui, Usb};
use kl_kindle::{device, display::Display, input, log, profile, Ev};
use kl_render::Renderer;
use storage_lease::StorageLease;

const W: u32 = profile::ACTIVE.screen_w;
const H: u32 = profile::ACTIVE.screen_h;
const HOUR: Duration = Duration::from_secs(60 * 60);
const CATALOG_AGE: Duration = Duration::from_secs(24 * 60 * 60);

fn env_dir(name: &str, default: &str) -> PathBuf {
    PathBuf::from(std::env::var(name).unwrap_or_else(|_| default.to_string()))
}

fn font_paths(dir: &Path, run: &Path, managed: bool) -> Vec<String> {
    let names = ["NotoSans-Regular.ttf", "NotoSans-Bold.ttf"];
    let temporary = run.join("fonts");
    // Font loading may retain a mapping for the life of the renderer.
    let base = if managed || temporary.join(names[0]).exists() { temporary } else { dir.join("fonts") };
    names.iter().map(|n| base.join(n).to_string_lossy().into_owned()).collect()
}

fn set_orientation(run: &Path, rotation: Rotation) -> std::io::Result<()> {
    input::TURN.store(rotation.turn(), Ordering::Relaxed);
    let tmp = run.join("orientation.tmp");
    fs::write(&tmp, rotation.letter())?;
    fs::rename(tmp, run.join("orientation"))
}

fn catalog_is_stale(dir: &Path, now: SystemTime) -> bool {
    store::cache_directory(dir)
        .and_then(|cache| fs::metadata(cache.join("index.json")).and_then(|meta| meta.modified()).ok())
        .is_none_or(|modified| now.duration_since(modified).is_ok_and(|elapsed| elapsed >= CATALOG_AGE))
}

fn load_catalog_pictures(renderer: &mut kl_render::Renderer, dir: &Path) {
    if let Some(cache) = store::cache_directory(dir) {
        kl_render::load_store_pictures(renderer, &cache);
    } else {
        renderer.clear_store_artwork();
    }
}

fn connected(status: &Status) -> bool {
    status.wifi_on && status.wifi_state == "connected" && status.usb != Usb::Drive
}

fn begin_write(storage: &mut StorageLease, ui: &mut Ui) -> bool {
    match storage.begin_write() {
        Ok(()) => true,
        Err(error) => {
            ui.notice = Some(error);
            false
        }
    }
}

fn release_write(storage: &mut StorageLease) {
    if let Err(error) = storage.finish_write() {
        log(&format!("store: cannot release Kindle storage: {error}"));
    }
}

fn maybe_refresh(ui: &mut Ui, dir: &Path, tx: &Sender<Ev>, settings: &Settings, status: &Status, storage: &mut StorageLease) -> bool {
    if !settings.store_daily_check || ui.store_busy || ui.store_installing.is_some() || ui.store_batch_running
        || !connected(status) || !catalog_is_stale(dir, SystemTime::now())
    {
        return false;
    }
    if begin_write(storage, ui) {
        ui.store_busy = true;
        ui.notice = Some("Updating the store…".to_string());
        store::start_store_refresh(tx.clone(), dir.to_path_buf());
    }
    true
}

fn main() {
    let dir = env_dir("KL_DIR", "/mnt/us/launcher");
    let run = env_dir("KL_RUN", "/var/tmp/kl-store");
    let documents = env_dir("KL_DOCUMENTS", "/mnt/us/documents");
    let open_supported = std::env::var(store_open::CAPABILITY).as_deref() == Ok("1");
    let mut storage = StorageLease::new(&run, std::env::var(storage_lease::CAPABILITY).as_deref() == Ok("1"));
    let _ = fs::create_dir_all(&run);
    let log_dir = if storage.managed() { &run } else { &dir };
    crash_log::install(&log_dir.join("log-store.txt"));
    log(&format!("store v{} profile {} dir {}", env!("CARGO_PKG_VERSION"), profile::ACTIVE.catalog_name, dir.display()));
    let root = dir.parent().unwrap_or(Path::new("/mnt/us")).to_path_buf();

    if let Err(error) = storage.begin_write() {
        log(&format!("store: cannot start: {error}"));
        process::exit(2);
    }

    // An install that a power cut interrupted is finished or undone before the store reads what is installed.
    let recovery_problem = match kl_store::install::recover_with_hooks(&kl_store::install::Paths::under(&root), &kl_kindle::package_hooks::KindleHookRunner) {
        Ok(Some(what)) => {
            log(&format!("store: {what}"));
            None
        }
        Ok(None) => None,
        Err(e) => {
            log(&format!("store: cannot check an unfinished install: {e}"));
            Some(format!("Store recovery is pending. {e}"))
        }
    };

    let mut settings = Settings::parse(&fs::read_to_string(dir.join("settings.txt")).unwrap_or_default());
    if let Err(e) = set_orientation(&run, settings.rotation) {
        log(&format!("store: cannot write its screen orientation: {e}"));
    }
    let mut renderer = match Renderer::new(W, H, &font_paths(&dir, &run, storage.managed())) {
        Ok(r) => r,
        Err(e) => {
            log(&format!("renderer: {e}"));
            process::exit(2);
        }
    };
    let mut ui = Ui::new(Vec::new(), Layout::new(W, H, 4, 5));
    ui.version = env!("CARGO_PKG_VERSION").to_string();
    ui.store_daily_check = settings.store_daily_check;
    ui.store_collections = store::load_collections(&dir);
    ui.set_store(store::load_store(&dir));
    load_catalog_pictures(&mut renderer, &dir);
    ui.open_store();

    let mut display = Display::open();
    let mut status = device::status(&settings);
    ui.usb = status.usb;
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
        thread::spawn(move || device::power_loop(t));
    }
    if settings.auto_rotation {
        let t = tx.clone();
        let rotation = settings.rotation;
        thread::spawn(move || device::orientation_loop(t, rotation));
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
    release_write(&mut storage);
    maybe_refresh(&mut ui, &dir, &tx, &settings, &status, &mut storage);
    if let Some(problem) = &recovery_problem {
        ui.notice = Some(problem.clone());
    }
    let mut next_check = Instant::now() + HOUR;
    display.show(renderer.draw(&ui, &status), &settings, true);

    while let Ok(ev) = rx.recv() {
        let action = match ev {
            Ev::Input(Input::Wake) => {
                status = device::status(&settings);
                ui.usb = status.usb;
                ui.handle(Input::Wake)
            }
            Ev::Input(input) => {
                if ui.press_feedback(input) {
                    if let Some(rect) = ui.pressed_key {
                        display.show_region(renderer.draw(&ui, &status), rect, &settings);
                    }
                }
                ui.handle(input)
            }
            Ev::Tick if ui.screen == Screen::Sleep => Action::None,
            Ev::Tick => {
                let usb = device::usb_state();
                let timed_refresh = if Instant::now() >= next_check {
                    next_check = Instant::now() + HOUR;
                    status = device::status(&settings);
                    maybe_refresh(&mut ui, &dir, &tx, &settings, &status, &mut storage)
                } else {
                    false
                };
                if ui.usb != usb {
                    ui.usb = usb;
                    status.usb = usb;
                    Action::Redraw { full: true }
                } else if timed_refresh {
                    Action::Redraw { full: false }
                } else {
                    Action::None
                }
            }
            Ev::Turn(rotation) if settings.auto_rotation && settings.rotation != rotation => {
                settings.rotation = rotation;
                status.rotation = rotation;
                if let Err(e) = set_orientation(&run, rotation) {
                    log(&format!("store: cannot write its screen orientation: {e}"));
                }
                Action::Redraw { full: true }
            }
            Ev::StoreRefreshed { entries, message } => {
                load_catalog_pictures(&mut renderer, &dir);
                ui.store_collections = store::load_collections(&dir);
                ui.update_store(entries);
                ui.store_busy = false;
                ui.notice = Some(message);
                release_write(&mut storage);
                Action::Redraw { full: false }
            }
            Ev::StoreInstalled { entries, message } => {
                close_install(&mut ui);
                ui.update_store(entries);
                ui.notice = Some(message);
                release_write(&mut storage);
                Action::Redraw { full: true }
            }
            Ev::StoreProgress { message, percent } => {
                ui.store_progress = Some(message);
                ui.store_progress_percent = percent;
                Action::Redraw { full: false }
            }
            _ => Action::None,
        };
        // Done (back to the home screen) ends the store: the launcher takes the screen again.
        if ui.screen == Screen::Home {
            if storage.writing() {
                ui.screen = Screen::Store;
                ui.notice = Some("Wait for the current Store change to finish.".into());
                display.show(renderer.draw(&ui, &status), &settings, false);
                continue;
            }
            log("store: done");
            break;
        }
        match action {
            Action::Redraw { .. } if ui.screen == Screen::Sleep => {}
            Action::Redraw { full } => display.show(renderer.draw(&ui, &status), &settings, full),
            Action::Device(cmd) => {
                status = device::status(&settings);
                let closed_modal = ui.store_sheet.is_some() || ui.store_batch_confirm;
                if cmd == DeviceCmd::StoreToggleDailyCheck {
                    if begin_write(&mut storage, &mut ui) {
                        let mut updated = settings.clone();
                        updated.store_daily_check = !settings.store_daily_check;
                        match save_settings(&dir, &updated) {
                            Ok(()) => {
                                ui.store_daily_check = updated.store_daily_check;
                                settings = updated;
                            }
                            Err(e) => ui.notice = Some(format!("The store settings could not be saved. {e}")),
                        }
                        release_write(&mut storage);
                    }
                } else if cmd == DeviceCmd::StoreClearCache {
                    if begin_write(&mut storage, &mut ui) {
                        ui.notice = Some(match kl_store::refresh::clear_cache(&dir) {
                            Ok(()) => {
                                ui.store_collections.clear();
                                ui.update_store(store::load_store(&dir));
                                load_catalog_pictures(&mut renderer, &dir);
                                "Cleared the saved store catalog.".to_string()
                            }
                            Err(e) => format!("The saved catalog could not be cleared. {e}"),
                        });
                        release_write(&mut storage);
                    }
                } else if let DeviceCmd::StoreOpen(index) = cmd {
                    if let Some(problem) = &recovery_problem {
                        ui.notice = Some(problem.clone());
                    } else if status.usb == Usb::Drive {
                        ui.notice = Some("Eject the Kindle drive before opening this app.".into());
                    } else if begin_write(&mut storage, &mut ui) {
                        match request_open(ui.store.get(index), &dir, &documents, &run, status.usb, open_supported) {
                            // The supervisor carries the pause through the launcher's next start.
                            Ok(()) => process::exit(store_open::EXIT_OPEN_APP),
                            Err(e) => ui.notice = Some(e),
                        }
                        release_write(&mut storage);
                    }
                } else {
                    device_command(cmd, &mut ui, &dir, &root, &tx, &status, &mut storage);
                }
                let full = closed_modal && ui.store_sheet.is_none() && !ui.store_batch_confirm;
                display.show(renderer.draw(&ui, &status), &settings, full);
            }
            _ => {}
        }
    }
}

fn request_open(entry: Option<&StoreEntry>, dir: &Path, documents: &Path, run: &Path, usb: Usb, supported: bool) -> Result<(), String> {
    let entry = entry.filter(|entry| entry.can_open()).ok_or("This app has no launcher entry.")?;
    if usb == Usb::Drive {
        return Err("Eject the Kindle drive before opening this app.".into());
    }
    if !supported {
        return Err("Open this app from the launcher grid.".into());
    }
    store_open::installed_entry(dir, documents, &entry.id)
        .and_then(|_| store_open::request(run, &entry.id))
        .map_err(|e| format!("Could not open {}. {e}", entry.name))
}

#[derive(Debug, PartialEq, Eq)]
enum Effect<'a> {
    None,
    Refresh,
    Install(&'a StoreEntry),
    UpdateAll,
    Remove(&'a StoreEntry),
}

fn store_command<'a>(cmd: DeviceCmd, status: &Status, entries: &'a [StoreEntry]) -> (Option<String>, Effect<'a>) {
    match cmd {
        DeviceCmd::StoreRefresh if connected(status) => (Some("Updating the store…".to_string()), Effect::Refresh),
        DeviceCmd::StoreRefresh => (Some("Connect to Wi-Fi to update the store.".to_string()), Effect::None),
        DeviceCmd::StoreInstall(index) => match entries.get(index) {
            Some(entry) if !entry.can_install() => (Some(entry.status_line()), Effect::None),
            Some(entry) if connected(status) => (Some(format!("Preparing {}…", entry.name)), Effect::Install(entry)),
            Some(_) => (Some("Connect to Wi-Fi to install from the store.".to_string()), Effect::None),
            None => (Some("This app could not be found in the store.".to_string()), Effect::None),
        },
        DeviceCmd::StoreUpdateAll if connected(status) => (Some("Preparing updates…".to_string()), Effect::UpdateAll),
        DeviceCmd::StoreUpdateAll => (Some("Connect to Wi-Fi to update apps.".to_string()), Effect::None),
        DeviceCmd::StoreRemove(index) => match entries.get(index).filter(|entry| entry.can_remove()) {
            Some(entry) => (None, Effect::Remove(entry)),
            None => (Some("This app cannot be removed from the store.".to_string()), Effect::None),
        },
        _ => (None, Effect::None),
    }
}

fn close_install(ui: &mut Ui) {
    ui.store_installing = None;
    ui.store_removing = false;
    ui.store_sheet = None;
    ui.store_requirements = None;
    ui.store_progress = None;
    ui.store_progress_percent = None;
    ui.store_batch_confirm = false;
    ui.store_batch_running = false;
}

fn save_settings(dir: &Path, settings: &Settings) -> std::io::Result<()> {
    use std::io::Write;
    let pending = dir.join("settings.store.tmp");
    let mut file = fs::File::create(&pending)?;
    file.write_all(settings.to_text().as_bytes())?;
    file.sync_all()?;
    fs::rename(pending, dir.join("settings.txt"))
}

/// The store's own commands, run off the screen where they take time.
fn device_command(cmd: DeviceCmd, ui: &mut Ui, dir: &Path, root: &Path, tx: &Sender<Ev>, status: &Status, storage: &mut StorageLease) {
    let (notice, effect) = store_command(cmd, status, &ui.store);
    match effect {
        Effect::Refresh => {
            if !begin_write(storage, ui) {
                ui.store_busy = false;
                return;
            }
            store::start_store_refresh(tx.clone(), dir.to_path_buf());
        }
        Effect::Install(entry) => {
            let planned = device::store_device()
                .ok_or_else(|| "The Kindle's firmware version could not be read.".to_string())
                .and_then(|device| requirements::installation_plan(dir, &entry.id, &device));
            match planned {
                Ok(plan) => {
                    let (_, required) = plan.split_last().expect("a plan includes the requested app");
                    let reviewed = ui.store_requirements.as_ref().is_some_and(|review| review.apps == required);
                    if required.is_empty() || reviewed {
                        if begin_write(storage, ui) {
                            ui.store_progress = notice;
                            store::start_store_install(tx.clone(), dir.to_path_buf(), plan);
                        } else {
                            close_install(ui);
                        }
                    } else {
                        ui.store_requirements = Some(StoreRequirementReview { apps: required.to_vec(), page: 0 });
                        ui.store_installing = None;
                    }
                }
                Err(error) => {
                    close_install(ui);
                    ui.notice = Some(error);
                }
            }
            return;
        }
        Effect::UpdateAll => {
            if !begin_write(storage, ui) {
                close_install(ui);
                return;
            }
            let updates = ui.store_pending_updates().map(|entry| (entry.id.clone(), entry.name.clone())).collect();
            ui.store_progress = notice;
            store::start_store_update_all(tx.clone(), dir.to_path_buf(), updates);
            return;
        }
        Effect::Remove(entry) => {
            let id = entry.id.clone();
            let name = entry.name.clone();
            if !begin_write(storage, ui) {
                return;
            }
            ui.store_installing = Some(id.clone());
            ui.store_removing = true;
            ui.notice = Some(format!("Removing {name}…"));
            store::start_store_remove(tx.clone(), dir.to_path_buf(), root.to_path_buf(), id, name);
            return;
        }
        Effect::None if cmd == DeviceCmd::StoreRefresh => ui.store_busy = false,
        Effect::None if matches!(cmd, DeviceCmd::StoreInstall(_) | DeviceCmd::StoreUpdateAll) => close_install(ui),
        Effect::None => {}
    }
    if cmd == DeviceCmd::StoreRefresh {
        log(&format!("store: refresh requested (wifi on: {}, state: {})", status.wifi_on, status.wifi_state));
    }
    ui.notice = notice;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn product_open_requires_the_supervisor_and_an_installed_entry() {
        let fixture = store::tests::Fixture::new();
        let root = fixture.launcher.parent().unwrap();
        let documents = root.join("documents");
        let run = root.join("run");
        fs::create_dir(&documents).unwrap();
        fs::create_dir(&run).unwrap();
        let listing = store::tests::listing("https://example.com/app.zip", "", 100);
        let installed = kl_store::record::Installed {
            id: listing.id.clone(), version: "1.0.0".into(), files: vec!["documents/Sample App.sh".into()],
            ..kl_store::record::Installed::default()
        };
        let app = StoreEntry::from_listing(&listing, Some(&installed));
        let mut record = kl_store::record::Record::new();
        record.apps.insert(installed.id.clone(), installed);
        kl_store::record::save(&fixture.launcher.join("store/installed.json"), &record).unwrap();

        assert!(request_open(Some(&app), &fixture.launcher, &documents, &run, Usb::Unplugged, true).is_err());
        fs::write(documents.join("Sample App.sh"), "#!/bin/sh\n").unwrap();
        assert!(request_open(Some(&app), &fixture.launcher, &documents, &run, Usb::Unplugged, false).is_err());
        assert!(request_open(Some(&app), &fixture.launcher, &documents, &run, Usb::Drive, true).is_err());
        request_open(Some(&app), &fixture.launcher, &documents, &run, Usb::Unplugged, true).unwrap();
        assert_eq!(store_open::take_request(&run).unwrap(), Some(listing.id));
    }

    #[test]
    fn refresh_and_install_respect_connection_and_missing_selection() {
        let mut status = Status::default();
        let listing = store::tests::listing("https://example.com/app.zip", "", 100);
        let entries = [StoreEntry::from_listing(&listing, None)];
        assert_eq!(store_command(DeviceCmd::StoreRefresh, &status, &entries).1, Effect::None);
        assert_eq!(store_command(DeviceCmd::StoreInstall(0), &status, &entries).1, Effect::None);
        status.wifi_on = true;
        status.wifi_state = "connected".into();
        assert_eq!(store_command(DeviceCmd::StoreRefresh, &status, &entries).1, Effect::Refresh);
        assert_eq!(store_command(DeviceCmd::StoreInstall(0), &status, &entries).1, Effect::Install(&entries[0]));
        assert_eq!(store_command(DeviceCmd::StoreInstall(99), &status, &entries).1, Effect::None);
    }

    #[test]
    fn remove_needs_an_installed_app_that_can_be_removed() {
        use kl_core::store::{Status as InstallStatus, STORE_ID};

        let status = Status::default();
        let listing = store::tests::listing("https://example.com/app.zip", "", 100);
        let mut entries = [StoreEntry::from_listing(&listing, None)];
        assert_eq!(store_command(DeviceCmd::StoreRemove(0), &status, &entries).1, Effect::None);
        entries[0].status = InstallStatus::Installed("1.0.0".into());
        assert_eq!(store_command(DeviceCmd::StoreRemove(0), &status, &entries).1, Effect::Remove(&entries[0]));
        entries[0].id = STORE_ID.to_string();
        assert_eq!(store_command(DeviceCmd::StoreRemove(0), &status, &entries).1, Effect::None);
    }

    #[test]
    fn a_missing_or_daily_old_catalog_is_stale_and_a_fresh_one_is_not() {
        let fixture = store::tests::Fixture::new();
        assert!(catalog_is_stale(&fixture.launcher, SystemTime::now()));
        fs::write(fixture.launcher.join("store/cache/index.json"), b"{}").unwrap();
        let now = SystemTime::now();
        assert!(!catalog_is_stale(&fixture.launcher, now));
        assert!(catalog_is_stale(&fixture.launcher, now + CATALOG_AGE));
    }
}
