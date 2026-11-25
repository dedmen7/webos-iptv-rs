use super::io;
use js_sys::Array;
use std::cell::RefCell;
use wasm_bindgen::{JsCast, closure::Closure};
use wasm_bindgen_futures::{JsFuture, spawn_local};
use web_sys::{
    Blob, Element, Event, HtmlImageElement, IntersectionObserver, IntersectionObserverEntry,
    IntersectionObserverInit, Response,
};

thread_local! {
    static OBSERVER: RefCell<Option<IntersectionObserver>> = const { RefCell::new(None) };
}

pub fn mount(image: &Element, url: &str) {
    let _ = image.set_attribute("data-poster", url);
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
    if !observed {
        let _ = image.set_attribute("src", url);
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
                if !entry.is_intersecting() {
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
                spawn_local(load(image, url));
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

async fn load(image: HtmlImageElement, url: String) {
    let blob = if let Some(blob) = io::poster(&url).await {
        Some(blob)
    } else if image.is_connected() {
        let blob = fetch(&url).await;
        if let Some(blob) = &blob {
            let key = url.clone();
            let blob = blob.clone();
            spawn_local(async move {
                io::store_poster(&key, &blob).await;
            });
        }
        blob
    } else {
        None
    };
    if !image.is_connected() {
        return;
    }
    let object_url = blob.and_then(|blob| web_sys::Url::create_object_url_with_blob(&blob).ok());
    if let Some(object_url) = object_url {
        let clean_up = object_url.clone();
        let cleanup_image = image.clone();
        let release = Closure::once_into_js(move |_event: Event| {
            cleanup_image.set_onload(None);
            cleanup_image.set_onerror(None);
            let _ = web_sys::Url::revoke_object_url(&clean_up);
        });
        image.set_onload(Some(release.unchecked_ref()));
        image.set_onerror(Some(release.unchecked_ref()));
        image.set_src(&object_url);
    } else {
        image.set_src(&url);
    }
}

async fn fetch(url: &str) -> Option<Blob> {
    let window = web_sys::window()?;
    let response: Response = JsFuture::from(window.fetch_with_str(url))
        .await
        .ok()?
        .dyn_into()
        .ok()?;
    if !response.ok() {
        return None;
    }
    let size = response
        .headers()
        .get("content-length")
        .ok()
        .flatten()
        .and_then(|value| value.parse::<u32>().ok());
    if size.is_some_and(|size| size > 768_000) {
        return None;
    }
    let blob: Blob = JsFuture::from(response.blob().ok()?)
        .await
        .ok()?
        .dyn_into()
        .ok()?;
    (blob.size() <= 768_000. && blob.type_().starts_with("image/")).then_some(blob)
}
