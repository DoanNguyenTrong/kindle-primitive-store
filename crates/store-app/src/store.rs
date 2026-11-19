//! The store's own work, off the screen: the catalog's refresh, installing and removing an app, and the store's
//! entries as the screens read them. Everything here is run on a thread, and reports back as an `Ev`.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::mpsc::Sender,
    thread,
};

use kl_core::store::format_size;
use kl_kindle::{device, log, Ev};
use kl_store::{catalog::Listing, pipeline::Stage, resolve::Device};
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
pub fn start_store_install(tx: Sender<Ev>, dir: PathBuf, id: String, name: String) {
    thread::spawn(move || {
        log(&format!("store: install of {id} started"));
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let kindle = device::store_device().ok_or("The Kindle's firmware version could not be read.")?;
            store_install_app(&dir, &id, &kindle, Policy::store, |stage, percent| {
                let _ = tx.send(Ev::StoreProgress { message: format!("{stage} {name}…"), percent });
            })
        }))
        .unwrap_or_else(|_| Err("An internal error occurred.".to_string()));
        let message = match outcome {
            Ok(version) => format!("Installed {name} {version}."),
            Err(e) => format!("Could not install {name}. {e}"),
        };
        log(&format!("store: {message}"));
        let entries = load_store(&dir);
        let _ = tx.send(Ev::StoreInstalled { entries, message });
    });
}

fn store_install_app(
    dir: &Path,
    id: &str,
    device: &Device,
    policy: impl FnOnce(u64) -> Policy,
    mut progress: impl FnMut(&str, Option<u8>),
) -> Result<String, String> {
    if !safe_id(id) {
        return Err("The store listing has an invalid app name.".to_string());
    }
    let listing_path = dir.join("store/cache/apps").join(format!("{id}.json"));
    let listing_text = fs::read_to_string(listing_path)
        .map_err(|_| "Refresh the store to find this app.".to_string())?;
    let listing: Listing = serde_json::from_str(&listing_text).map_err(|e| e.to_string())?;
    if listing.schema != 1 || listing.id != id {
        return Err("The store listing does not match this app.".to_string());
    }
    let version = kl_store::resolve::pick_version(&listing, device).map_err(|e| e.to_string())?;
    let asset = version.assets.get(&device.arch).ok_or("No package is available for this Kindle.")?;
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
        primitive_net::NetError::Transport(detail)
            if detail.to_lowercase().contains("timed out") || detail.to_lowercase().contains("timeout") => {
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

fn safe_id(id: &str) -> bool {
    (2..=40).contains(&id.len()) && id.as_bytes()[0] != b'-'
        && id.bytes().all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
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
    let record = kl_store::record::load(&dir.join("store/installed.json")).unwrap_or_default();
    let Ok(read) = fs::read_dir(dir.join("store/cache/apps")) else {
        return Vec::new();
    };
    let mut entries: Vec<kl_core::store::StoreEntry> = read
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| fs::read_to_string(e.path()).ok())
        .filter_map(|text| serde_json::from_str::<Listing>(&text).ok())
        .filter(|listing| listing.schema == 1 && safe_id(&listing.id))
        .map(|listing| kl_core::store::StoreEntry::from_listing(&listing, record.apps.get(&listing.id)))
        .collect();
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    entries
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

        fn write_listing(&self, listing: &Listing) {
            fs::write(
                self.launcher.join("store/cache/apps").join(format!("{ID}.json")),
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

