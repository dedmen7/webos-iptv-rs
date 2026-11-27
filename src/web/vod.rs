use super::i18n::tr;
use super::{Catalog, Kind, Shared, io, maybe_render, sources};
use crate::catalog::{self, Item, Source};
use futures::{FutureExt, channel::oneshot, future};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, rc::Rc};

const PREVIEW_WAIT_MS: i32 = 120;
const PREVIEW_CATEGORY_ATTEMPTS: usize = 3;
const UPDATE_INTERVAL_MS: f64 = 160.;
const CACHE_READ_GROUP: usize = 8;
const CACHE_WORK_SLICE_MS: f64 = 36.;

pub struct Load {
    pub source: Source,
    pub key: String,
    pub kind: Kind,
    pub revision: u64,
    pub preference: bool,
    pub refresh: bool,
    pub needs_preview: bool,
}

#[derive(Deserialize, Serialize)]
struct CacheIndex {
    generation: String,
    batches: usize,
}

impl CacheIndex {
    fn batch_key(&self, key: &str, index: usize) -> String {
        format!("{key}:batch:{}:{index}", self.generation)
    }

    async fn remove(&self, key: &str) {
        io::clear_catalog_entries((0..self.batches).map(|index| self.batch_key(key, index))).await;
    }
}

pub async fn load(app: &Shared, request: Load) {
    let index_key = format!("{}:batch-index", request.key);
    let cache_index = io::cached_raw(&index_key).shared();
    let categories = sources::vod_categories(&request.source, request.kind)
        .map(|result| result.map(Rc::new))
        .shared();
    let (shown, wait_for_preview) = oneshot::channel();
    let first = async {
        let key = format!("{}:first:{}", request.key, request.preference);
        let mut from_source = false;
        let mut titles = if request.needs_preview {
            io::read_recent::<Vec<Item>>(&key, io::CATALOG_RETENTION_MS)
                .await
                .map(|entry| entry.value)
                .filter(|items| !items.is_empty())
        } else {
            None
        };
        if !request.refresh
            && titles.is_none()
            && request.needs_preview
            && cache_index
                .clone()
                .await
                .as_deref()
                .and_then(|raw| io::decode_recent::<CacheIndex>(raw, io::CATALOG_RETENTION_MS))
                .is_none()
            && active(app, &request)
            && let Ok(categories) = categories.clone().await
        {
            for (id, label) in catalog::preview_categories(&categories, request.preference)
                .take(PREVIEW_CATEGORY_ATTEMPTS)
            {
                if !active(app, &request) {
                    break;
                }
                let mut preview = Vec::new();
                let result = sources::stream_vod(
                    sources::VodFeed {
                        source: &request.source,
                        kind: request.kind,
                        categories: &HashMap::new(),
                        preview: Some(sources::PreviewCategory {
                            id: &id,
                            label: &label,
                            preferred: request.preference,
                            limit: super::PAGE_SIZE,
                        }),
                    },
                    async |items| {
                        preview = items;
                        false
                    },
                )
                .await;
                if result.is_ok() && !preview.is_empty() {
                    titles = Some(preview);
                    from_source = true;
                    break;
                }
            }
        }
        let visible = titles.as_ref().is_some_and(|titles| {
            let mut state = app.borrow_mut();
            if state.revision != request.revision
                || !state.pending.contains(&request.kind)
                || state.model.contains_key(&request.kind)
                || state.settings.preferred != request.preference
            {
                return false;
            }
            state
                .catalog_previews
                .insert(request.kind, Catalog::new(titles.clone()));
            true
        });
        if visible {
            maybe_render(app, request.kind);
        }
        let _ = shown.send(visible);
        if visible
            && from_source
            && let Some(titles) = titles
        {
            io::put(&key, &titles).await;
        }
    };

    let complete = async {
        let gate = future::select(
            Box::pin(wait_for_preview),
            Box::pin(io::delay(PREVIEW_WAIT_MS)),
        );
        let (gate, raw_index) = futures::join!(gate, cache_index.clone());
        if matches!(gate, future::Either::Left((Ok(true), _))) {
            io::after_frame().await;
        }
        let mut updates = Updates::default();
        if !request.refresh
            && request.needs_preview
            && let Some(index) = raw_index
                .as_deref()
                .and_then(|raw| io::decode_recent::<CacheIndex>(raw, io::CATALOG_RETENTION_MS))
        {
            if restore_cache(app, &request, &index.value, &mut updates).await {
                if !request.refresh && index.age_ms <= io::CATALOG_FRESH_MS {
                    finish(app, &request, Ok(()));
                    return;
                }
                {
                    let mut state = app.borrow_mut();
                    if state.revision != request.revision || !state.pending.contains(&request.kind)
                    {
                        return;
                    }
                    install_catalog(&mut state, request.kind);
                }
                maybe_render(app, request.kind);
                io::after_frame().await;
            } else if active(app, &request) {
                app.borrow_mut().vod_partial.remove(&request.kind);
                updates = Updates::default();
                maybe_render(app, request.kind);
            }
        }
        if !active(app, &request) {
            return;
        }
        let had_catalog = app.borrow().model.contains_key(&request.kind);
        let metadata = categories.clone().await.and_then(|data| {
            let category_error = || tr("Categorieën konden niet worden geladen.").to_owned();
            let entries = data.as_array().ok_or_else(category_error)?;
            let mut names = catalog::category_names(&data);
            names.retain(|id, name| !id.is_empty() && !name.is_empty());
            if !entries.is_empty() && names.is_empty() {
                return Err(category_error());
            }
            Ok(names)
        });
        if let Err(error) = &metadata
            && (request.refresh || had_catalog)
        {
            finish(app, &request, Err(error.clone()));
            return;
        }
        if !active(app, &request) {
            return;
        }

        let mut index = CacheIndex {
            generation: format!("{}-{}", js_sys::Date::now() as u64, request.revision),
            batches: 0,
        };
        let mut cacheable = metadata.is_ok();
        let names = metadata.unwrap_or_default();
        let mut previews: [Vec<Item>; 2] = [Vec::new(), Vec::new()];
        let result = sources::stream_vod(
            sources::VodFeed {
                source: &request.source,
                kind: request.kind,
                categories: &names,
                preview: None,
            },
            async |items| {
                if !active(app, &request) {
                    return false;
                }
                if cacheable {
                    let key = index.batch_key(&request.key, index.batches);
                    if io::put(&key, &items).await {
                        index.batches += 1;
                    } else {
                        cacheable = false;
                    }
                }
                if request.refresh {
                    if cacheable {
                        for (preferred, preview) in [false, true].into_iter().zip(&mut previews) {
                            let remaining = super::PAGE_SIZE.saturating_sub(preview.len());
                            preview.extend(
                                items
                                    .iter()
                                    .filter(|item| !preferred || catalog::preferred_content(item))
                                    .take(remaining)
                                    .cloned(),
                            );
                        }
                    }
                    return cacheable;
                }
                publish(app, &request, items, &mut updates)
            },
        )
        .await;

        let saved = cacheable
            && result.is_ok()
            && active(app, &request)
            && io::put(&index_key, &index).await;
        if saved {
            if let Some(previous) =
                raw_index.and_then(|raw| serde_json::from_str::<(f64, CacheIndex)>(&raw).ok())
            {
                previous.1.remove(&request.key).await;
            }
        } else {
            index.remove(&request.key).await;
        }
        if request.refresh {
            if saved {
                for (preferred, preview) in [false, true].into_iter().zip(previews) {
                    if active(app, &request) {
                        let key = format!("{}:first:{preferred}", request.key);
                        io::put(&key, &preview).await;
                    }
                }
            }
            let result = result.and_then(|()| {
                saved
                    .then_some(())
                    .ok_or_else(|| tr("Catalogus kon niet worden opgeslagen.").into())
            });
            finish(app, &request, result);
            return;
        }
        finish(app, &request, result);
        if saved {
            save_previews(app, &request).await;
        }
    };
    futures::join!(first, complete);
}

async fn save_previews(app: &Shared, request: &Load) {
    io::after_frame().await;
    let previews = {
        let state = app.borrow();
        let Some(catalog) = state
            .model
            .get(&request.kind)
            .filter(|_| state.revision == request.revision)
        else {
            return;
        };
        [false, true].map(|preferred| {
            let indices = catalog.search("", "", preferred, super::PAGE_SIZE).0;
            let first: Vec<_> = indices
                .into_iter()
                .map(|index| catalog.items[index].clone())
                .collect();
            (format!("{}:first:{preferred}", request.key), first)
        })
    };
    for (key, items) in previews {
        if app.borrow().revision != request.revision {
            return;
        }
        io::put(&key, &items).await;
    }
}

async fn restore_cache(
    app: &Shared,
    request: &Load,
    index: &CacheIndex,
    updates: &mut Updates,
) -> bool {
    if index.batches == 0 {
        return false;
    }
    for first in (0..index.batches).step_by(CACHE_READ_GROUP) {
        if !active(app, request) {
            return false;
        }
        let end = first.saturating_add(CACHE_READ_GROUP).min(index.batches);
        let keys = (first..end).map(|batch| index.batch_key(&request.key, batch));
        let Some(entries) = io::cached_raw_group(keys).await else {
            return false;
        };
        let mut work_started = js_sys::Date::now();
        for (offset, pending) in entries.into_iter().enumerate() {
            let raw = pending.await;
            if !active(app, request) {
                return false;
            }
            let Some(entry) = raw
                .as_deref()
                .and_then(|raw| io::decode_recent::<Vec<Item>>(raw, io::CATALOG_RETENTION_MS))
            else {
                return false;
            };
            if !publish(app, request, entry.value, updates) {
                return false;
            }
            if first + offset + 1 < end && js_sys::Date::now() - work_started >= CACHE_WORK_SLICE_MS
            {
                io::delay(0).await;
                work_started = js_sys::Date::now();
            }
        }
    }
    true
}

fn active(app: &Shared, request: &Load) -> bool {
    let state = app.borrow();
    state.revision == request.revision && state.pending.contains(&request.kind)
}

#[derive(Default)]
struct Updates {
    last: f64,
    dirty: bool,
}

fn publish(app: &Shared, request: &Load, items: Vec<Item>, updates: &mut Updates) -> bool {
    let mut state = app.borrow_mut();
    if state.revision != request.revision || !state.pending.contains(&request.kind) {
        return false;
    }
    let kind = request.kind;
    let browsing = state.view.kind() == Some(kind)
        && state.detail.is_none()
        && state.playback.is_none()
        && !state.model.contains_key(&kind);
    let can_render = browsing && (state.query.is_empty() || focused_title(&state).is_none());
    let preferred = state.settings.preferred;
    let query = state.query.clone();
    let filter = state.catalog_filter.clone();
    let preview_visible = state.catalog_previews.contains_key(&kind);
    let catalog = state.vod_partial.entry(kind).or_default();
    let before = catalog.items.len();
    let first_page_needed = !preview_visible
        && query.is_empty()
        && filter.is_all()
        && catalog
            .visible_items(preferred)
            .take(super::PAGE_SIZE)
            .count()
            < super::PAGE_SIZE;
    catalog.append(items);
    if browsing && !updates.dirty {
        updates.dirty = first_page_needed
            || ((!query.is_empty() || !filter.is_all())
                && catalog.has_match_from(&query, filter.borrowed(), preferred, before));
    }
    let now = js_sys::Date::now();
    let render = can_render && updates.dirty && now - updates.last >= UPDATE_INTERVAL_MS;
    drop(state);
    if render {
        updates.dirty = false;
        updates.last = now;
        maybe_render(app, kind);
    }
    true
}

fn finish(app: &Shared, request: &Load, result: Result<(), String>) {
    let mut state = app.borrow_mut();
    if state.revision != request.revision || !state.pending.remove(&request.kind) {
        return;
    }
    let kind = request.kind;
    state.finish_source_refresh(kind, &result);
    if request.refresh {
        if result.is_ok() {
            state.model.remove(&kind);
            state.catalog_previews.remove(&kind);
        }
        return;
    }
    match result {
        Ok(()) => install_catalog(&mut state, kind),
        Err(error) => {
            if state.model.contains_key(&kind) {
                state.vod_partial.remove(&kind);
            } else {
                state.failures.insert(kind, error);
            }
        }
    }
    drop(state);
    maybe_render(app, kind);
}

fn install_catalog(state: &mut super::App, kind: Kind) {
    let mut catalog = state.vod_partial.remove(&kind).unwrap_or_default();
    if let Some(preview) = state.catalog_previews.remove(&kind) {
        catalog.keep_preview_order(&preview.items);
    }
    state.model.insert(kind, catalog);
    state.sync_browse_filter(kind);
    state.restore_catalog_page_for_focus(kind);
}

fn focused_title(state: &super::App) -> Option<String> {
    let active = state.document.active_element()?;
    if active.get_attribute("data-cmd").as_deref() != Some("item") {
        return None;
    }
    active.get_attribute("data-value")
}
