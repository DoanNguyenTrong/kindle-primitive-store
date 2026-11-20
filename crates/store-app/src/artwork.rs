//! Prepare visible artwork off the event loop, then preload icons near the viewport.

use std::{
    collections::HashSet,
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    sync::mpsc::{channel, Receiver, Sender},
    thread,
    time::{Duration, Instant},
};

use kl_core::{store::StoreTab, Screen, Ui, Usb};
use kl_kindle::Ev;
use kl_render::{
    image, store_icon_key, store_shot_key, GrayImage, Renderer, STORE_FEATURE_ICON, STORE_PRODUCT_ICON,
    STORE_ROW_ICON, STORE_SHOT_H, STORE_SHOT_W,
};
use kl_store::{catalog::valid_app_id, refresh::MAX_PICTURE};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Asset {
    Icon(String, u32),
    Shot(String, usize),
}

impl Asset {
    fn renderer_key(&self) -> String {
        match self {
            Self::Icon(id, size) => store_icon_key(id, *size),
            Self::Shot(id, index) => store_shot_key(id, *index),
        }
    }

    fn load(&self, cache: &Path) -> Option<GrayImage> {
        let (id, path) = match self {
            Self::Icon(id, _) => (id, cache.join("icons").join(format!("{id}.png"))),
            Self::Shot(id, index) => (id, cache.join("shots").join(format!("{id}-{index}.png"))),
        };
        if !valid_app_id(id) {
            return None;
        }
        let mut png = Vec::new();
        File::open(path).ok()?.take(MAX_PICTURE + 1).read_to_end(&mut png).ok()?;
        if png.len() as u64 > MAX_PICTURE {
            return None;
        }
        match self {
            Self::Icon(_, size) => image::icon_tile(&png, *size).ok(),
            Self::Shot(..) => image::picture_tile(&png, STORE_SHOT_W, STORE_SHOT_H).ok(),
        }
    }
}

#[derive(Default, PartialEq, Eq)]
struct PageArtwork {
    visible: Vec<Asset>,
    nearby: Vec<Asset>,
}

impl PageArtwork {
    fn for_ui(ui: &Ui) -> Self {
        if ui.usb == Usb::Drive {
            return Self::default();
        }
        let mut page = match ui.screen {
            Screen::Store => Self::for_list(ui),
            Screen::StoreApp => Self::for_product(ui),
            _ => return Self::default(),
        };
        page.nearby.retain(|asset| !page.visible.contains(asset));
        page
    }

    fn for_list(ui: &Ui) -> Self {
        let featured = if ui.store_tab == StoreTab::Today { ui.store_featured() } else { None };
        let icon = |index: usize| {
            let size = if Some(index) == featured { STORE_FEATURE_ICON } else { STORE_ROW_ICON };
            Asset::Icon(ui.store[index].id.clone(), size)
        };
        let visible = ui.store_visible_entries().into_iter().map(|(_, index)| icon(index)).collect();
        let nearby = ui.store_nearby_entries().into_iter().map(icon).collect();
        Self { visible, nearby }
    }

    fn for_product(ui: &Ui) -> Self {
        let Some(entry) = ui.store_open.and_then(|index| ui.store.get(index)) else { return Self::default() };
        let list = Self::for_list(ui);
        let mut visible = Vec::new();
        let mut nearby = Vec::new();
        let icon = Asset::Icon(entry.id.clone(), STORE_PRODUCT_ICON);
        if ui.layout.store_hero(ui.store_scroll).y >= 170 {
            visible.push(icon);
        } else {
            nearby.push(icon);
        }

        if ui.store_shot_index < entry.screenshots {
            let shot = ui.layout.store_shot(ui.store_scroll);
            let selected = Asset::Shot(entry.id.clone(), ui.store_shot_index);
            if shot.y < ui.layout.height - 120 && shot.bottom() > 170 {
                visible.push(selected);
            } else {
                nearby.push(selected);
            }
            nearby.extend((0..entry.screenshots).filter(|&index| index != ui.store_shot_index)
                .map(|index| Asset::Shot(entry.id.clone(), index)));
        }
        nearby.extend(list.visible.into_iter().chain(list.nearby));
        Self { visible, nearby }
    }

    fn all(&self) -> impl Iterator<Item = &Asset> {
        self.visible.iter().chain(&self.nearby)
    }
}

struct Job {
    cache: PathBuf,
    revision: u64,
    assets: Vec<Asset>,
}

struct Finished {
    revision: u64,
    tiles: Vec<(Asset, Option<GrayImage>)>,
}

pub struct Artwork {
    cache: Option<PathBuf>,
    revision: u64,
    page: PageArtwork,
    preload_after: Instant,
    attempted: HashSet<Asset>,
    in_flight: bool,
    worker: Sender<Job>,
    finished: Receiver<Finished>,
}

impl Artwork {
    pub fn new(cache: Option<PathBuf>, events: Sender<Ev>) -> Self {
        let (worker, requests) = channel::<Job>();
        let (ready, finished) = channel();
        thread::spawn(move || {
            while let Ok(job) = requests.recv() {
                let tiles = job.assets.into_iter().map(|asset| {
                    let tile = asset.load(&job.cache);
                    (asset, tile)
                }).collect();
                if ready.send(Finished { revision: job.revision, tiles }).is_err() || events.send(Ev::Tick).is_err() {
                    break;
                }
            }
        });
        Self {
            cache,
            revision: 0,
            page: PageArtwork::default(),
            preload_after: Instant::now(),
            attempted: HashSet::new(),
            in_flight: false,
            worker,
            finished,
        }
    }

    pub fn replace_cache(&mut self, cache: Option<PathBuf>, artwork_changed: bool, renderer: &mut Renderer) {
        if self.cache != cache || artwork_changed {
            self.revision += 1;
            self.cache = cache;
        }
        if artwork_changed || self.cache.is_none() {
            self.attempted.clear();
            renderer.clear_store_artwork();
        }
    }

    pub fn update_page(&mut self, ui: &Ui, renderer: &mut Renderer) {
        // Preserve the decoded page while the Kindle is asleep or its drive is exported.
        if ui.screen == Screen::Sleep || ui.usb == Usb::Drive {
            return;
        }
        let next = PageArtwork::for_ui(ui);
        if self.page != next {
            // Let the panel settle before preparing off-screen art, especially during rapid scrolling.
            self.preload_after = Instant::now() + Duration::from_millis(450);
            self.page = next;
        }
        let wanted: HashSet<_> = self.page.all().collect();
        self.attempted.retain(|asset| {
            if wanted.contains(asset) {
                true
            } else {
                renderer.forget_icon(&asset.renderer_key());
                false
            }
        });
        self.schedule();
    }

    pub fn collect(&mut self, renderer: &mut Renderer, can_schedule: bool) -> bool {
        let mut visible_changed = false;
        if let Ok(result) = self.finished.try_recv() {
            self.in_flight = false;
            if result.revision == self.revision {
                for (asset, tile) in result.tiles {
                    let visible = self.page.visible.contains(&asset);
                    if visible || self.page.nearby.contains(&asset) {
                        if let Some(tile) = tile {
                            renderer.set_icon(&asset.renderer_key(), tile);
                            visible_changed |= visible;
                        }
                        self.attempted.insert(asset);
                    }
                }
            }
        }
        if visible_changed {
            self.preload_after = Instant::now() + Duration::from_millis(450);
        }
        if can_schedule {
            self.schedule();
        }
        visible_changed
    }

    fn schedule(&mut self) {
        let Some(cache) = &self.cache else { return };
        if self.in_flight {
            return;
        }
        let mut assets: Vec<_> = self.page.visible.iter().filter(|asset| !self.attempted.contains(*asset)).cloned().collect();
        if assets.is_empty() && Instant::now() >= self.preload_after {
            // Keep speculative work to one tile so visible icons take priority after a scroll.
            assets.extend(self.page.nearby.iter().filter(|asset| !self.attempted.contains(*asset)).take(1).cloned());
        }
        if !assets.is_empty() {
            self.in_flight = self.worker.send(Job { cache: cache.clone(), revision: self.revision, assets }).is_ok();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kl_core::{store::StoreEntry, Layout};

    fn catalog() -> Ui {
        let mut ui = Ui::new(Vec::new(), Layout::new(1264, 1680, 4, 5));
        let listing = crate::store::tests::listing("https://example.com/app.zip", "", 100);
        let mut entries = Vec::new();
        for index in 0..100 {
            let mut entry = StoreEntry::from_listing(&listing, None);
            entry.id = format!("app-{index:02}");
            entry.screenshots = 2;
            entries.push(entry);
        }
        ui.set_store(entries);
        ui.open_store();
        ui
    }

    #[test]
    fn today_preloads_nearby_app_icons() {
        let ui = catalog();
        let page = PageArtwork::for_ui(&ui);
        assert_eq!(page.visible, [
            Asset::Icon("app-00".into(), STORE_FEATURE_ICON),
            Asset::Icon("app-01".into(), STORE_ROW_ICON),
            Asset::Icon("app-02".into(), STORE_ROW_ICON),
        ]);
        assert!(page.nearby.contains(&Asset::Icon("app-03".into(), STORE_ROW_ICON)));
        assert!(page.all().count() < 30);
        assert!(page.all().all(|asset| matches!(asset, Asset::Icon(..))));
    }

    #[test]
    fn list_artwork_is_bounded_near_the_scroll_position_and_respects_search() {
        let mut ui = catalog();
        ui.store_tab = StoreTab::Apps;
        let row_height = ui.layout.store_list_row(0).h;
        ui.store_list_scroll = 20 * row_height + 30;
        let page = PageArtwork::for_ui(&ui);
        assert_eq!(page.visible[0], Asset::Icon("app-20".into(), STORE_ROW_ICON));
        assert!(page.nearby.contains(&Asset::Icon("app-19".into(), STORE_ROW_ICON)));
        assert!(page.nearby.contains(&Asset::Icon("app-31".into(), STORE_ROW_ICON)));
        assert!(page.all().count() < 30);
        assert!(page.nearby.iter().all(|asset| !page.visible.contains(asset)));

        ui.store_tab = StoreTab::Search;
        ui.store_list_scroll = 0;
        ui.store_query.text = "needle".chars().collect();
        ui.store[90].subtitle = "a needle".into();
        ui.store[91].subtitle = "another needle".into();
        let page = PageArtwork::for_ui(&ui);
        assert_eq!(page.visible, [
            Asset::Icon("app-90".into(), STORE_ROW_ICON),
            Asset::Icon("app-91".into(), STORE_ROW_ICON),
        ]);
        assert!(page.nearby.is_empty());
    }

    #[test]
    fn only_the_open_product_warms_screenshots() {
        let mut ui = catalog();
        ui.screen = Screen::StoreApp;
        ui.store_open = Some(80);
        let page = PageArtwork::for_ui(&ui);
        assert!(page.visible.contains(&Asset::Icon("app-80".into(), STORE_PRODUCT_ICON)));
        let shots: Vec<_> = page.all().filter(|asset| matches!(asset, Asset::Shot(..))).cloned().collect();
        assert_eq!(shots, [Asset::Shot("app-80".into(), 0), Asset::Shot("app-80".into(), 1)]);
    }
}
