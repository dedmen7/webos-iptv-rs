use super::{App, PAGE_SIZE, Shared, io};
use crate::catalog::{Catalog, Item, Kind, LiveGroups};
use serde::{Deserialize, Serialize};
use std::{collections::HashSet, rc::Rc};
use wasm_bindgen_futures::spawn_local;

#[derive(Clone, Deserialize, Serialize)]
pub(super) struct Preview {
    groups: Option<LiveGroups>,
    items: Vec<Item>,
    #[serde(default)]
    feed_routes: Option<Vec<Item>>,
}

#[derive(Clone)]
struct Snapshot {
    key: String,
    timestamp: f64,
    preview: Preview,
}

#[derive(Default)]
pub(super) struct Cache {
    current: Option<Rc<Snapshot>>,
    pending: Vec<Rc<Snapshot>>,
    writing: bool,
    refresh_queued: bool,
    retry_current: bool,
}

impl Cache {
    pub(super) fn forget_current(&mut self) {
        self.current = None;
        self.retry_current = false;
    }

    pub(super) fn timestamp(&self) -> Option<f64> {
        self.current.as_ref().map(|snapshot| snapshot.timestamp)
    }

    pub(super) fn feed_changed(&mut self, app: &Shared, epg_id: &str) {
        if self.refresh_queued
            || !(self.retry_current
                || self.current.as_ref().is_some_and(|snapshot| {
                    snapshot
                        .preview
                        .items
                        .iter()
                        .any(|item| item.epg_id.trim().eq_ignore_ascii_case(epg_id.trim()))
                }))
        {
            return;
        }
        self.refresh(app);
    }

    pub(super) fn refresh(&mut self, app: &Shared) {
        if self.refresh_queued {
            return;
        }
        self.refresh_queued = true;
        let app = app.clone();
        spawn_local(async move {
            io::after_frame().await;
            let mut state = app.borrow_mut();
            state.live_preview.refresh_queued = false;
            Self::update_routes(&app, &mut state);
        });
    }

    fn update_routes(app: &Shared, state: &mut App) {
        let Some(snapshot) = &state.live_preview.current else {
            return;
        };
        if snapshot.preview.groups.as_ref() != state.live_groups() {
            return;
        }
        let Some(catalog) = state.model.get(&Kind::Live) else {
            return;
        };
        let routes = catalog.preview_feed_routes(
            &snapshot.preview.items,
            &state.live_feeds,
            &state.source_key,
            io::now(),
        );
        if snapshot.preview.feed_routes.as_ref() == Some(&routes) {
            if state.live_preview.retry_current {
                state.live_preview.save(app, snapshot.clone());
            }
            return;
        }
        let mut next = Rc::clone(snapshot);
        Rc::make_mut(&mut next).preview.feed_routes = Some(routes);
        state.live_preview.save(app, next);
    }

    fn save(&mut self, app: &Shared, snapshot: Rc<Snapshot>) {
        self.current = Some(snapshot.clone());
        self.retry_current = false;
        self.pending.retain(|queued| queued.key != snapshot.key);
        self.pending.push(snapshot);
        if self.writing {
            return;
        }
        self.writing = true;
        let app = app.clone();
        spawn_local(async move {
            loop {
                let snapshot = app.borrow_mut().live_preview.pending.pop();
                let Some(snapshot) = snapshot else {
                    app.borrow_mut().live_preview.writing = false;
                    break;
                };
                if !io::put_at(&snapshot.key, &snapshot.preview, snapshot.timestamp).await {
                    let mut state = app.borrow_mut();
                    let cache = &mut state.live_preview;
                    if cache
                        .current
                        .as_ref()
                        .is_some_and(|current| Rc::ptr_eq(&snapshot, current))
                    {
                        cache.retry_current = true;
                    }
                }
            }
        });
    }
}

pub(super) fn publish(
    app: &Shared,
    key: &str,
    revision: u64,
    entry: io::CacheEntry<Preview>,
) -> bool {
    let mut state = app.borrow_mut();
    if state.revision != revision
        || state.live_groups() != entry.value.groups.as_ref()
        || entry.value.items.is_empty()
    {
        return false;
    }
    let mut catalog = Catalog::new(entry.value.items.clone());
    if let Some(routes) = &entry.value.feed_routes {
        catalog.set_preview_feed_routes(routes.clone());
    }
    state.live_preview.retry_current = false;
    state.live_preview.current = Some(Rc::new(Snapshot {
        key: key.to_owned(),
        timestamp: entry.timestamp_ms,
        preview: entry.value,
    }));
    state.catalog_previews.insert(Kind::Live, catalog);
    drop(state);
    super::maybe_render(app, Kind::Live);
    true
}

impl Preview {
    pub(super) fn has_measured_candidate(&self, state: &App) -> bool {
        state
            .live_feeds
            .has_preview_candidate(&state.source_key, &self.items, io::now())
    }

    pub(super) fn has_routes(&self) -> bool {
        self.feed_routes.is_some()
    }
}

pub(super) fn rebuild(app: &Shared, state: &mut App, key: &str, timestamp: f64) {
    let Some(catalog) = state.model.get(&Kind::Live) else {
        return;
    };
    let mut seen = HashSet::new();
    let items: Vec<_> = catalog
        .visible_items(false)
        .take(PAGE_SIZE)
        .chain(catalog.visible_items(true).take(PAGE_SIZE))
        .filter(|item| seen.insert(item.id.as_str()))
        .cloned()
        .collect();
    if items.is_empty() {
        return;
    }
    let routes =
        catalog.preview_feed_routes(&items, &state.live_feeds, &state.source_key, io::now());
    let preview = Preview {
        groups: state.live_groups().cloned(),
        items,
        feed_routes: Some(routes),
    };
    state.live_preview.save(
        app,
        Rc::new(Snapshot {
            key: key.to_owned(),
            timestamp,
            preview,
        }),
    );
}
