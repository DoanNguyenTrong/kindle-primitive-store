use std::{collections::BTreeSet, path::Path};

use kl_core::store::{StoreEntry, LAUNCHER_ID};
use kl_store::{
    record::Record,
    resolve::{self, Device},
    Version,
};

/// Resolves the target and any missing or outdated prerequisites in dependency order.
pub fn installation_plan(dir: &Path, id: &str, device: &Device) -> Result<Vec<StoreEntry>, String> {
    let installed = super::store::record(dir)?;
    let mut planner = Planner {
        dir,
        device,
        installed: &installed,
        visiting: BTreeSet::new(),
        ordered: Vec::new(),
    };
    planner.visit(id, None)?;
    Ok(planner.ordered)
}

struct Planner<'a> {
    dir: &'a Path,
    device: &'a Device,
    installed: &'a Record,
    visiting: BTreeSet<String>,
    ordered: Vec<StoreEntry>,
}

impl Planner<'_> {
    fn visit(&mut self, id: &str, minimum: Option<&Version>) -> Result<(), String> {
        let installed = self.installed.apps.get(id);
        if let Some(minimum) = minimum {
            let installed_version = installed.and_then(|app| Version::parse(&app.version));
            if installed_version.is_some_and(|version| &version >= minimum) {
                return Ok(());
            }
            if let Some(entry) = self.ordered.iter().find(|entry| entry.id == id) {
                if entry
                    .latest
                    .as_deref()
                    .and_then(Version::parse)
                    .is_some_and(|version| &version >= minimum)
                {
                    return Ok(());
                }
                return Err(format!(
                    "The store has no new enough version of {}.",
                    entry.name
                ));
            }
            if id == LAUNCHER_ID {
                return Err(
                    "Update Primitive Launcher and restart it before installing this app."
                        .to_string(),
                );
            }
        }
        if !self.visiting.insert(id.to_string()) {
            return Err(format!("The app requirements form a loop at {id}."));
        }
        let listing = super::store::read_listing(self.dir, id)
            .map_err(|error| format!("Could not find {id} in the saved catalog. {error}"))?;
        if listing.changes.hooks {
            return Err(format!(
                "{} needs custom install steps. This store cannot run them.",
                listing.name
            ));
        }
        let candidate = resolve::pick_version(&listing, self.device)
            .map_err(|error| format!("{} is unavailable. {error}.", listing.name))?;
        let version =
            Version::parse(&candidate.version).ok_or("The catalog has an invalid version.")?;
        if minimum.is_some_and(|minimum| &version < minimum) {
            return Err(format!(
                "The store has no new enough version of {}.",
                listing.name
            ));
        }
        for (required, range) in &candidate.requires {
            let minimum = range
                .strip_prefix(">=")
                .and_then(Version::parse)
                .ok_or_else(|| {
                    format!(
                        "{} has an invalid requirement for {required}.",
                        listing.name
                    )
                })?;
            self.visit(required, Some(&minimum))?;
        }
        self.visiting.remove(id);
        self.ordered
            .push(StoreEntry::for_device(&listing, installed, self.device));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::tests::{listing, Fixture};
    use kl_store::record::Installed;

    fn device() -> Device {
        Device {
            arch: "armhf".to_string(),
            firmware: Version::parse("5.18.2").unwrap(),
        }
    }

    #[test]
    fn orders_missing_and_outdated_requirements_and_skips_an_installed_one() {
        let fixture = Fixture::new();
        let mut target = listing("https://example.com/app.zip", "", 100);
        target.versions[0]
            .requires
            .insert("mid-app".into(), ">=1.0.0".into());
        target.versions[0]
            .requires
            .insert("already-there".into(), ">=1.0.0".into());
        let mut middle = target.clone();
        middle.id = "mid-app".into();
        middle.versions[0].requires = [("bottom-app".into(), ">=1.0.0".into())].into();
        let mut bottom = target.clone();
        bottom.id = "bottom-app".into();
        bottom.versions[0].requires.clear();
        for listing in [&target, &middle, &bottom] {
            fixture.write_listing(listing);
        }
        let mut record = Record::new();
        for (id, version) in [("mid-app", "0.9.0"), ("already-there", "1.0.0")] {
            record.apps.insert(
                id.into(),
                Installed {
                    id: id.into(),
                    version: version.into(),
                    ..Installed::default()
                },
            );
        }
        kl_store::record::save(&fixture.launcher.join("store/installed.json"), &record).unwrap();
        let order = installation_plan(&fixture.launcher, &target.id, &device()).unwrap();
        let ids: Vec<_> = order.iter().map(|entry| entry.id.as_str()).collect();
        assert_eq!(ids, ["bottom-app", "mid-app", "test-app"]);
    }

    #[test]
    fn reports_cycles_and_incompatible_prerequisites() {
        let fixture = Fixture::new();
        let mut target = listing("https://example.com/app.zip", "", 100);
        target.versions[0]
            .requires
            .insert("needed-app".into(), ">=1.0.0".into());
        let mut needed = target.clone();
        needed.id = "needed-app".into();
        needed.versions[0].requires = [(target.id.clone(), ">=1.0.0".into())].into();
        fixture.write_listing(&target);
        fixture.write_listing(&needed);
        assert!(installation_plan(&fixture.launcher, &target.id, &device())
            .unwrap_err()
            .contains("loop"));
        needed.versions[0].assets.clear();
        fixture.write_listing(&needed);
        assert!(installation_plan(&fixture.launcher, &target.id, &device())
            .unwrap_err()
            .contains("armhf"));
    }
}
