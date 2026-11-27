use super::io;
use futures::{
    FutureExt,
    channel::oneshot,
    future::{Either, LocalBoxFuture, Shared, select},
};
use js_sys::Array;
use std::{
    cell::RefCell,
    collections::{HashMap, VecDeque},
};
use wasm_bindgen::{JsCast, closure::Closure};
use wasm_bindgen_futures::spawn_local;
use web_sys::{
    Blob, Element, Event, HtmlImageElement, IntersectionObserver, IntersectionObserverEntry,
    IntersectionObserverInit,
};

thread_local! {
    static OBSERVER: RefCell<Option<IntersectionObserver>> = const { RefCell::new(None) };
    static LOADING: RefCell<HashMap<String, PosterLoad>> = RefCell::new(HashMap::new());
    static RECENT: RefCell<RecentPosters> = RefCell::new(RecentPosters::default());
}

type PosterLoad = Shared<LocalBoxFuture<'static, Option<Blob>>>;

#[derive(Default)]
struct RecentPosters {
    images: VecDeque<(String, Blob)>,
    bytes: f64,
}

impl RecentPosters {
    fn get(&mut self, key: &str) -> Option<Blob> {
        let index = self.images.iter().position(|(url, _)| url == key)?;
        let (key, image) = self.images.remove(index)?;
        self.images.push_back((key, image.clone()));
        Some(image)
    }

    fn insert(&mut self, key: String, image: Blob) {
        const MAX_MEMORY_BYTES: f64 = 6. * 1024. * 1024.;
        const MAX_IMAGES: usize = 32;
        let size = image.size();
        if size > MAX_MEMORY_BYTES {
            return;
        }
        while self.images.len() >= MAX_IMAGES || self.bytes + size > MAX_MEMORY_BYTES {
            if let Some((_, evicted)) = self.images.pop_front() {
                self.bytes -= evicted.size();
            } else {
                break;
            }
        }
        self.bytes += size;
        self.images.push_back((key, image));
    }
}

pub fn mount(image: &Element, url: &url::Url, fallback: Option<&url::Url>) {
    let _ = image.set_attribute("data-poster", url.as_str());
    if let Some(fallback) = fallback {
        let _ = image.set_attribute("data-poster-fallback", fallback.as_str());
    }
    let observed = OBSERVER.with(|observer| {
        let mut observer = observer.borrow_mut();
        if observer.is_none() {
            *observer = make_observer();
        }
        observer
            .as_ref()
            .map(|observer| observer.observe(image))
            .is_some()
    });
    if !observed && let Some(image) = image.dyn_ref::<HtmlImageElement>() {
        spawn_local(load(image.clone(), url.as_str().to_owned(), false));
    }
}

pub fn discard_under(area: &Element) {
    OBSERVER.with(|observer| {
        let observer = observer.borrow();
        let Some(observer) = observer.as_ref() else {
            return;
        };
        let Ok(images) = area.query_selector_all("img[data-poster]") else {
            return;
        };
        for index in 0..images.length() {
            if let Some(image) = images
                .item(index)
                .and_then(|node| node.dyn_into::<Element>().ok())
            {
                observer.unobserve(&image);
            }
        }
    });
}

fn make_observer() -> Option<IntersectionObserver> {
    let callback = Closure::<dyn FnMut(Array, IntersectionObserver)>::new(
        |entries: Array, observer: IntersectionObserver| {
            for entry in entries.iter() {
                let Ok(entry) = entry.dyn_into::<IntersectionObserverEntry>() else {
                    continue;
                };
                if !entry.is_intersecting() && entry.intersection_ratio() <= 0. {
                    continue;
                }
                let target = entry.target();
                observer.unobserve(&target);
                let Some(url) = target.get_attribute("data-poster") else {
                    continue;
                };
                let Ok(image) = target.dyn_into::<HtmlImageElement>() else {
                    continue;
                };
                spawn_local(load(image, url, true));
            }
        },
    );
    let options = IntersectionObserverInit::new();
    options.set_root_margin("240px");
    let observer =
        IntersectionObserver::new_with_options(callback.as_ref().unchecked_ref(), &options).ok()?;
    callback.forget();
    Some(observer)
}

async fn load(image: HtmlImageElement, url: String, cache: bool) {
    let fallback = image.get_attribute("data-poster-fallback");
    if load_candidate(&image, &url, cache, fallback.is_none()).await {
        return;
    }
    if let Some(fallback) = fallback
        && load_candidate(&image, &fallback, cache, true).await
    {
        return;
    }
    image.set_hidden(true);
}

async fn load_candidate(
    image: &HtmlImageElement,
    url: &str,
    cache: bool,
    retry_direct: bool,
) -> bool {
    if !image.is_connected() {
        return false;
    }
    if cache
        && let Some(blob) = resolve(url).await
        && let Ok(object_url) = web_sys::Url::create_object_url_with_blob(&blob)
    {
        let loaded = display(image, &object_url).await;
        let _ = web_sys::Url::revoke_object_url(&object_url);
        if loaded || !retry_direct {
            return loaded;
        }
    }
    display(image, url).await
}

async fn display(image: &HtmlImageElement, src: &str) -> bool {
    if !image.is_connected() {
        return false;
    }
    let (sender, receiver) = oneshot::channel();
    let mut sender = Some(sender);
    let settled = Closure::<dyn FnMut(Event)>::new(move |event: Event| {
        if let Some(sender) = sender.take() {
            let _ = sender.send(event.type_() == "load");
        }
    });
    image.set_onload(Some(settled.as_ref().unchecked_ref()));
    image.set_onerror(Some(settled.as_ref().unchecked_ref()));
    image.set_src(src);
    let result = select(receiver, Box::pin(io::delay(5_000))).await;
    image.set_onload(None);
    image.set_onerror(None);
    let loaded = matches!(result, Either::Left((Ok(true), _)));
    if !loaded {
        let _ = image.remove_attribute("src");
    }
    loaded
}

async fn resolve(url: &str) -> Option<Blob> {
    if let Some(blob) = RECENT.with(|recent| recent.borrow_mut().get(url)) {
        return Some(blob);
    }
    let loading = LOADING.with(|loading| {
        let mut loading = loading.borrow_mut();
        if let Some(existing) = loading.get(url) {
            return existing.clone();
        }
        let url = url.to_owned();
        let lookup = url.clone();
        let future = async move {
            let blob = if let Some(saved) = io::poster(&lookup).await {
                Some(saved)
            } else {
                let fetched = io::image_blob(&lookup).await;
                if let Some(image) = fetched.clone() {
                    let key = lookup.clone();
                    spawn_local(async move {
                        io::store_poster(&key, &image).await;
                    });
                }
                fetched
            };
            if let Some(image) = blob.clone() {
                RECENT.with(|recent| recent.borrow_mut().insert(lookup.clone(), image));
            }
            LOADING.with(|loading| loading.borrow_mut().remove(&lookup));
            blob
        }
        .boxed_local()
        .shared();
        loading.insert(url, future.clone());
        future
    });
    loading.await
}
