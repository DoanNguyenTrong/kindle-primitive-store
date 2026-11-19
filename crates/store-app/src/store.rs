//! The store's own work, off the screen: the catalog's refresh, installing and removing an app, and the store's
//! entries as the screens read them. Everything here is run on a thread, and reports back as an `Ev`.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::mpsc::Sender,
    thread,
};

use kl_kindle::{device, log, Ev};

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
pub fn start_store_install(tx: Sender<Ev>, dir: PathBuf, id: String) {
    thread::spawn(move || {
        log(&format!("store: install of {id} started"));
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| store_install_app(&dir, &id)))
            .unwrap_or_else(|_| Err("an internal error".to_string()));
        let message = match outcome {
            Ok(version) => format!("Installed {id} {version}."),
            Err(e) => format!("Could not install {id}: {e}"),
        };
        log(&format!("store: {message}"));
        let entries = load_store(&dir);
        let _ = tx.send(Ev::StoreInstalled { entries, message });
    });
}

fn store_install_app(dir: &Path, id: &str) -> Result<String, String> {
    let listing_text = fs::read_to_string(dir.join("store/cache/apps").join(format!("{id}.json")))
        .map_err(|_| "the app is not in the catalog the store has".to_string())?;
    let listing: kl_store::catalog::Listing = serde_json::from_str(&listing_text).map_err(|e| e.to_string())?;
    let device = device::store_device().ok_or("the Kindle's firmware version cannot be read")?;
    let version = kl_store::resolve::pick_version(&listing, &device).map_err(|e| e.to_string())?;
    let asset = version.assets.get(&device.arch).ok_or("no package for this Kindle")?;
    let root = dir.parent().unwrap_or(Path::new("/mnt/us")).to_path_buf();
    let paths = kl_store::install::Paths::under(&root);
    let downloads = dir.join("store/downloads");
    fs::create_dir_all(&downloads).map_err(|e| e.to_string())?;
    let zip = downloads.join(format!("{id}-{}-{}.zip", version.version, device.arch));
    let policy = primitive_net::Policy::store(asset.size + 1);
    primitive_net::download(&asset.url, &zip, &policy, |_, _| {}).map_err(|e| e.to_string())?;
    let installed_at = std::process::Command::new("date")
        .arg("+%Y-%m-%d")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    kl_store::pipeline::install_downloaded(&paths, &listing, &device, &zip, &asset.url, &installed_at)
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

/// Fonts: the copy in KL_RUN (tmpfs, made by the supervisor) when present, else KL_DIR.
/// The config (apps.toml), the apps (in documents), the KUAL extensions, and the problems found reading them. The
/// boot copy and /mnt/us read the same files; the directory decides which.
/// The store's entries: one per listing in the catalog cache (`store/cache/apps/*.json`), with what is installed.
pub fn load_store(dir: &Path) -> Vec<kl_core::store::StoreEntry> {
    let record = kl_store::record::load(&dir.join("store/installed.json")).unwrap_or_default();
    let Ok(read) = fs::read_dir(dir.join("store/cache/apps")) else {
        return Vec::new();
    };
    let mut entries: Vec<kl_core::store::StoreEntry> = read
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| fs::read_to_string(e.path()).ok())
        .filter_map(|text| serde_json::from_str::<kl_store::catalog::Listing>(&text).ok())
        .map(|listing| kl_core::store::StoreEntry::from_listing(&listing, record.apps.get(&listing.id)))
        .collect();
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    entries
}

