//! The store's own work, off the screen: the catalog's refresh, installing and removing an app, and the store's
//! entries as the screens read them. Everything here is run on a thread, and reports back as an `Ev`.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::mpsc::Sender,
    thread,
};

use kl_core::store::{format_size, StoreEntry, LAUNCHER_ID};
use kl_kindle::{device, log, Ev};
use kl_store::{
    catalog::{valid_app_id, Collection, CollectionIndex, Listing},
    pipeline::Stage,
    record::Record,
    resolve::{self, Device},
    Version,
};
use primitive_net::Policy;

/// The catalog's published site (the store's source of listings, icons and screenshots).
const STORE_CATALOG: &str = "https://doannguyentrong.github.io/kindle-primitive-catalog/v1";

/// The store's catalog files over HTTPS, through the shared download code (only the store's hosts are reached).
struct NetSource;

impl kl_store::refresh::Source for NetSource {
    fn text(&self, url: &str) -> Result<String, String> {
        primitive_net::fetch_text(url, &primitive_net::Policy::store(kl_store::refresh::MAX_INDEX)).map_err(|e| e.to_string())
    }
    fn bytes(&self, url: &str, max: u64) -> Result<Vec<u8>, String> {
        primitive_net::fetch_bytes(url, &primitive_net::Policy::store(max)).map_err(|e| e.to_string())
    }
}

/// Install an app from the store on a thread: download its package for this Kindle, check it, install it, and tell the
/// store and the library. The download is refused when Wi-Fi is not connected (the caller checks).
pub fn start_store_install(tx: Sender<Ev>, dir: PathBuf, mut plan: Vec<StoreEntry>) {
    let Some(target) = plan.pop() else { return };
    thread::spawn(move || {
        let StoreEntry { id, name, .. } = target;
        log(&format!("store: install of {id} started"));
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if id == LAUNCHER_ID && std::env::var_os("KL_LAUNCHER_SELF_UPDATE").as_deref() != Some(std::ffi::OsStr::new("1")) {
                return Err("This launcher needs a manual update before it can update through the store.".to_string());
            }
            let kindle = device::store_device().ok_or("The Kindle's firmware version could not be read.")?;
            for required in plan {
                store_install_app(&dir, &required.id, &kindle, Policy::store, |stage, percent| {
                    let _ = tx.send(Ev::StoreProgress { message: format!("{stage} {}…", required.name), percent });
                }).map_err(|error| format!("Required app {} could not be installed. {error}", required.name))?;
            }
            store_install_app(&dir, &id, &kindle, Policy::store, |stage, percent| {
                let stage = if id == LAUNCHER_ID && stage == "Installing" { "Preparing" } else { stage };
                let _ = tx.send(Ev::StoreProgress { message: format!("{stage} {name}…"), percent });
            })
        }))
        .unwrap_or_else(|_| Err("An internal error occurred.".to_string()));
        let message = match outcome {
            Ok(_) if id == LAUNCHER_ID => "Launcher update is ready. Tap Done to restart the launcher.".to_string(),
            Ok(version) => format!("Installed {name} {version}."),
            Err(e) => format!("Could not install {name}. {e}"),
        };
        log(&format!("store: {message}"));
        let entries = load_store(&dir);
        let _ = tx.send(Ev::StoreInstalled { entries, message });
    });
}

pub fn start_store_update_all(tx: Sender<Ev>, dir: PathBuf, apps: Vec<(String, String)>) {
    thread::spawn(move || {
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let kindle = device::store_device().ok_or("The Kindle's firmware version could not be read.")?;
            let record = record(&dir)?;
            let listings = apps.iter().map(|(id, _)| read_listing(&dir, id)).collect::<Result<Vec<_>, _>>()?;
            let order = update_order(&listings, &record, &kindle)?;
            let mut updated = 0;
            let mut failures = Vec::new();
            for index in order {
                let (id, name) = &apps[index];
                let result = store_install_app(&dir, id, &kindle, Policy::store, |stage, percent| {
                    let _ = tx.send(Ev::StoreProgress { message: format!("{stage} {name}…"), percent });
                });
                match result {
                    Ok(_) => updated += 1,
                    Err(error) => {
                        log(&format!("store: could not update {id}: {error}"));
                        failures.push(format!("{name}. {error}"));
                    }
                }
            }
            let total = apps.len();
            Ok::<_, String>(match failures.first() {
                None => format!("Updated {updated} apps."),
                Some(first) => format!("Updated {updated} of {total} apps. Could not update {first}"),
            })
        })).unwrap_or_else(|_| Err("An internal error occurred.".to_string()));
        let message = outcome.unwrap_or_else(|error| format!("Could not update the apps. {error}"));
        let _ = tx.send(Ev::StoreInstalled { entries: load_store(&dir), message });
    });
}

/// Orders updates whose requirements are satisfied by the installed or previously planned versions.
fn update_order(listings: &[Listing], installed: &Record, device: &Device) -> Result<Vec<usize>, String> {
    use std::collections::BTreeMap;

    let mut available: BTreeMap<&str, Version> = installed.apps.values()
        .filter_map(|app| Version::parse(&app.version).map(|version| (app.id.as_str(), version)))
        .collect();
    let mut pending = listings.iter().enumerate()
        .map(|(i, listing)| resolve::pick_version(listing, device)
            .map(|version| (i, listing.id.as_str(), version))
            .map_err(|error| format!("{}: {error}.", listing.name)))
        .collect::<Result<Vec<_>, _>>()?;
    let mut order = Vec::new();
    while !pending.is_empty() {
        let ready = pending.iter().position(|(_, _, version)| {
            resolve::missing_requirements(&version.requires, &|id| available.get(id).cloned()).is_empty()
        });
        let Some(ready) = ready else {
            let names = pending.iter().map(|(i, _, _)| listings[*i].name.as_str()).collect::<Vec<_>>();
            return Err(format!("Check the app requirements for {}.", names.join(", ")));
        };
        let (i, id, version) = pending.remove(ready);
        let version = Version::parse(&version.version).ok_or("An update has an invalid version number.")?;
        available.insert(id, version);
        order.push(i);
    }
    Ok(order)
}

pub(crate) fn record(dir: &Path) -> Result<Record, String> {
    kl_store::record::load(&dir.join("store/installed.json"))
        .map_err(|e| format!("The store could not read the list of installed apps. {e}"))
}

pub(crate) fn read_listing(dir: &Path, id: &str) -> Result<Listing, String> {
    if !valid_app_id(id) {
        return Err("The store listing has an invalid app name.".to_string());
    }
    let listing_path = dir.join("store/cache/apps").join(format!("{id}.json"));
    let listing_text = fs::read_to_string(listing_path).map_err(|_| "Refresh the store to find this app.".to_string())?;
    let listing: Listing = serde_json::from_str(&listing_text).map_err(|e| e.to_string())?;
    if listing.schema != 1 || listing.id != id {
        return Err("The store listing does not match this app.".to_string());
    }
    Ok(listing)
}

fn store_install_app(
    dir: &Path,
    id: &str,
    device: &Device,
    policy: impl FnOnce(u64) -> Policy,
    mut progress: impl FnMut(&str, Option<u8>),
) -> Result<String, String> {
    let listing = read_listing(dir, id)?;
    if listing.changes.hooks {
        return Err("This app needs custom install steps. This store cannot run them.".to_string());
    }
    let version = kl_store::resolve::pick_version(&listing, device).map_err(|e| e.to_string())?;
    let asset = version.assets.get(&device.arch).ok_or("No package is available for this Kindle.")?;
    let installed = record(dir)?;
    if let Some((required, version, _)) = resolve::missing_requirements(&version.requires, &|id| {
        installed.apps.get(id).and_then(|app| Version::parse(&app.version))
    }).first() {
        return Err(format!("Install {required} {version} first."));
    }
    let root = dir.parent().unwrap_or(Path::new("/mnt/us"));
    let paths = kl_store::install::Paths::under(root);
    let available = device::available_storage_bytes(root).map_err(|_| "Free space on this Kindle could not be checked.")?;
    check_space(available, asset.size)?;
    let downloads = dir.join("store/downloads");
    fs::create_dir_all(&downloads).map_err(|e| e.to_string())?;
    let zip = downloads.join(format!("{id}.zip"));
    progress("Downloading", Some(0));
    let mut last_step = 0;
    // The byte count in the checked catalog remains available if the server omits Content-Length.
    primitive_net::download(&asset.url, &zip, &policy(asset.size.saturating_add(1)), |bytes, _| {
        let step = (bytes.saturating_mul(10) / asset.size).min(10) as u8;
        if step > last_step {
            last_step = step;
            progress("Downloading", Some(step * 10));
        }
    }).map_err(|e| match e {
        primitive_net::NetError::Timeout => {
            "The download took too long. Check Wi-Fi and try again.".to_string()
        }
        _ => e.to_string(),
    })?;
    let installed_at = std::process::Command::new("date")
        .arg("+%Y-%m-%d")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    kl_store::pipeline::install_downloaded_with_progress(&paths, &listing, device, &zip, &asset.url, &installed_at, |stage| {
        progress(match stage {
            Stage::Checking => "Checking",
            Stage::Installing => "Installing",
        }, None);
    })
}

fn check_space(available: u64, size: u64) -> Result<(), String> {
    if size == 0 {
        return Err("The store listing does not give a valid download size.".to_string());
    }
    let required = size.saturating_mul(3);
    if available < required {
        return Err(format!("This app needs {} free. This Kindle has {} free.", format_size(required), format_size(available)));
    }
    Ok(())
}

/// Refresh the store's catalog cache on a thread, then send the store's entries and a message. The screen is not held
/// while the files come in.
pub fn start_store_refresh(tx: Sender<Ev>, dir: PathBuf) {
    thread::spawn(move || {
        let cache = dir.join("store/cache");
        log("store: refresh started");
        // A panic must still end the refresh (the button waits for it), so it is caught and reported as an error.
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| kl_store::refresh::refresh(STORE_CATALOG, &cache, &NetSource)))
            .unwrap_or_else(|_| Err("an internal error".to_string()));
        let message = match outcome {
            Ok(report) => {
                log(&format!("store: refreshed {} apps, {} failed, {} removed", report.refreshed, report.failed.len(), report.removed));
                if report.failed.is_empty() {
                    format!("Store updated: {} apps.", report.refreshed)
                } else {
                    format!("Store updated: {} apps. {} could not be updated.", report.refreshed, report.failed.len())
                }
            }
            Err(e) => {
                log(&format!("store: refresh failed: {e}"));
                format!("The store could not be updated: {e}")
            }
        };
        let entries = load_store(&dir);
        let _ = tx.send(Ev::StoreRefreshed { entries, message });
    });
}

/// The store's entries: one per listing in the catalog cache (`store/cache/apps/*.json`), with what is installed.
pub fn load_store(dir: &Path) -> Vec<kl_core::store::StoreEntry> {
    let mut installed = record(dir).unwrap_or_default();
    let device = device::store_device();
    let read = fs::read_dir(dir.join("store/cache/apps"));
    let mut entries: Vec<kl_core::store::StoreEntry> = read
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| fs::read_to_string(e.path()).ok())
        .filter_map(|text| serde_json::from_str::<Listing>(&text).ok())
        .filter(|listing| listing.schema == 1 && valid_app_id(&listing.id))
        .map(|listing| {
            let installed_app = installed.apps.remove(&listing.id);
            match &device {
                Some(device) => StoreEntry::for_device(&listing, installed_app.as_ref(), device),
                None => StoreEntry::from_listing(&listing, installed_app.as_ref()),
            }
        })
        .collect();
    entries.extend(installed.apps.values().map(StoreEntry::from_installed));
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    entries
}

pub fn load_collections(dir: &Path) -> Vec<Collection> {
    fs::read(dir.join("store/cache/collections.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<CollectionIndex>(&bytes).ok())
        .filter(|index| index.schema == 1)
        .map(|index| index.collections)
        .unwrap_or_default()
}

pub fn clear_cache(dir: &Path) -> std::io::Result<()> {
    match fs::remove_dir_all(dir.join("store/cache")) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::TcpListener,
        sync::atomic::{AtomicU64, Ordering},
        time::Duration,
    };

    static NEXT_DIR: AtomicU64 = AtomicU64::new(0);
    const ID: &str = "test-app";

    pub(crate) struct Fixture {
        root: PathBuf,
        pub(crate) launcher: PathBuf,
    }

    impl Fixture {
        pub(crate) fn new() -> Self {
            let n = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir().join(format!("primitive-store-{}-{n}", std::process::id()));
            let launcher = root.join("launcher");
            fs::create_dir_all(launcher.join("store/cache/apps")).unwrap();
            Self { root, launcher }
        }

        pub(crate) fn write_listing(&self, listing: &Listing) {
            fs::write(
                self.launcher.join("store/cache/apps").join(format!("{}.json", listing.id)),
                serde_json::to_vec(listing).unwrap(),
            ).unwrap();
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    pub(crate) fn listing(url: &str, sha: &str, size: u64) -> Listing {
        serde_json::from_value(serde_json::json!({
            "schema": 1, "id": ID, "name": "Test App", "subtitle": "A fixture", "developer": {"name": "Primitive AI"},
            "categories": ["tools"], "description": "Checks Store installation.", "repo": "https://example.com",
            "licence": "MIT", "source": "hosted", "icon": "icon.png", "changes": {
                "network": false, "stops_kindle_ui": false, "starts_at_boot": false, "root_filesystem": false, "hooks": false
            },
            "versions": [{
                "version": "1.0.0", "date": "2026-11-19", "notes": "Initial version.", "firmware": {"min": "5.16.3"},
                "assets": {"armhf": {"url": url, "sha256": sha, "size": size}}
            }]
        })).unwrap()
    }

    #[test]
    fn checks_required_space_including_working_copies() {
        assert!(check_space(2_999, 1_000).unwrap_err().contains("3 KB free"));
        assert!(check_space(3_000, 1_000).is_ok());
        assert!(check_space(3_000, 0).is_err());
        assert!(check_space(3_000, u64::MAX).is_err());
    }

    #[test]
    fn loading_catalog_combines_listings_with_the_install_record() {
        let fixture = Fixture::new();
        fixture.write_listing(&listing("https://example.com/app.zip", "", 100));
        let paths = kl_store::install::Paths::under(&fixture.root);
        fs::write(&paths.record, format!(r#"{{"schema":1,"apps":{{"{ID}":{{
            "id":"{ID}","dir":"{ID}","version":"0.9.0","arch":"armhf","installed_at":"2026-11-18",
            "source_url":"","sha256":"","files":[],"requires":{{}}
        }}}}}}"#)).unwrap();
        let entries = load_store(&fixture.launcher);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].action_label(), "Update");
        assert!(entries[0].can_remove());
        clear_cache(&fixture.launcher).unwrap();
        clear_cache(&fixture.launcher).unwrap();
        let preserved = load_store(&fixture.launcher);
        assert_eq!(preserved[0].action_label(), "Installed");
        assert!(preserved[0].can_remove());
    }

    #[test]
    fn plans_prerequisites_before_updates_that_need_them() {
        use kl_store::record::Installed;

        let mut app = listing("https://example.com/app.zip", "", 100);
        app.versions[0].requires.insert("required-app".to_string(), ">=1.0.0".to_string());
        let mut dependency = listing("https://example.com/dependency.zip", "", 100);
        dependency.id = "required-app".to_string();
        let mut installed = Record::new();
        installed.apps.insert(dependency.id.clone(), Installed {
            id: dependency.id.clone(), version: "0.9.0".to_string(), ..Installed::default()
        });
        let device = Device { arch: "armhf".into(), firmware: Version::parse("5.18.2").unwrap() };
        assert!(update_order(std::slice::from_ref(&app), &installed, &device).is_err());
        assert_eq!(update_order(&[app, dependency], &installed, &device).unwrap(), vec![1, 0]);
    }

    #[test]
    fn install_downloads_checks_records_and_removes_a_fixture_package() {
        let fixture = Fixture::new();
        let zip_path = fixture.root.join("fixture.zip");
        let mut zip = zip::ZipWriter::new(fs::File::create(&zip_path).unwrap());
        let options = zip::write::SimpleFileOptions::default();
        zip.start_file("test-app-1.0.0/manifest.json", options).unwrap();
        zip.write_all(br#"{"schema":1,"id":"test-app","dir":"test-app","version":"1.0.0","arch":"armhf",
            "firmware":{"min":"5.16.3"},"requires":{},"files":["test-app/app.bin"],"hooks":[]}"#).unwrap();
        zip.start_file("test-app-1.0.0/test-app/app.bin", options).unwrap();
        zip.write_all(b"fixture application bytes").unwrap();
        zip.finish().unwrap();
        let (sha, size) = kl_store::verify::sha256_file(&zip_path).unwrap();

        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let url = format!("http://{}/app.zip", listener.local_addr().unwrap());
        fixture.write_listing(&listing(&url, &sha, size));
        let body = fs::read(zip_path).unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                stream.read_exact(&mut byte).unwrap();
                request.push(byte[0]);
            }
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
            stream.write_all(&body).unwrap();
        });

        let device = Device { arch: "armhf".into(), firmware: kl_store::Version::parse("5.18.2").unwrap() };
        let mut progress = Vec::new();
        let installed = store_install_app(&fixture.launcher, ID, &device, |max| Policy {
            allowed_hosts: vec!["127.0.0.1".to_string()],
            max_bytes: max,
            timeout: Duration::from_secs(3),
            max_redirects: 0,
            allow_plain_http: true,
        }, |stage, step| progress.push((stage.to_string(), step))).unwrap();
        server.join().unwrap();
        assert_eq!(installed, "1.0.0");
        assert_eq!(progress.first().unwrap(), &("Downloading".to_string(), Some(0)));
        assert_eq!(progress[progress.len() - 3..], [
            ("Downloading".to_string(), Some(100)), ("Checking".to_string(), None), ("Installing".to_string(), None),
        ]);
        assert_eq!(fs::read(fixture.root.join("test-app/app.bin")).unwrap(), b"fixture application bytes");
        assert!(!fixture.launcher.join("store/downloads/test-app.zip").exists());
        let paths = kl_store::install::Paths::under(&fixture.root);
        assert_eq!(load_store(&fixture.launcher)[0].action_label(), "Installed");
        kl_store::install::remove(&paths, ID).unwrap();
        assert!(!fixture.root.join("test-app/app.bin").exists());
        assert_eq!(load_store(&fixture.launcher)[0].action_label(), "Get");
    }
}

