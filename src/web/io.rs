use js_sys::{Date, Function, Promise};
use serde::{Serialize, de::DeserializeOwned};
use std::cell::RefCell;
use wasm_bindgen::{JsCast, JsValue, closure::Closure};
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    AbortController, Event, IdbDatabase, IdbOpenDbRequest, IdbRequest, Request, RequestInit,
    Response,
};

const STORE: &str = "catalog";
const POSTERS: &str = "posters";
const POSTER_ORDER: &str = "iptv-rs.poster-order";
const CACHE_MS: f64 = 12. * 60. * 60. * 1000.;
thread_local! { static DB: RefCell<Option<IdbDatabase>> = const { RefCell::new(None) }; }

pub fn now() -> i64 {
    (Date::now() / 1000.) as i64
}

pub fn saved<T: DeserializeOwned>(key: &str) -> Option<T> {
    serde_json::from_str(
        &web_sys::window()?
            .local_storage()
            .ok()??
            .get_item(key)
            .ok()??,
    )
    .ok()
}

pub fn save<T: Serialize>(key: &str, value: &T) -> bool {
    let Some(storage) = web_sys::window().and_then(|window| window.local_storage().ok().flatten())
    else {
        return false;
    };
    serde_json::to_string(value)
        .ok()
        .is_some_and(|json| storage.set_item(key, &json).is_ok())
}

pub async fn text(
    url: &str,
    headers: &[(&str, &str)],
    body: Option<&str>,
) -> Result<String, String> {
    let window = web_sys::window().ok_or("Browservenster ontbreekt")?;
    let cancel = AbortController::new().map_err(|_| "Verbinding instellen mislukt")?;
    let options = RequestInit::new();
    options.set_signal(Some(&cancel.signal()));
    if let Some(body) = body {
        options.set_method("POST");
        options.set_body(&JsValue::from_str(body));
    }
    let request =
        Request::new_with_str_and_init(url, &options).map_err(|_| "Ongeldige aanvraag")?;
    for (name, value) in headers {
        request
            .headers()
            .set(name, value)
            .map_err(|_| "Ongeldige aanvraagheader")?;
    }
    let controller = cancel.clone();
    let timeout = Closure::<dyn FnMut()>::new(move || controller.abort());
    let timer = window
        .set_timeout_with_callback_and_timeout_and_arguments_0(
            timeout.as_ref().unchecked_ref(),
            25_000,
        )
        .ok();
    let result = async {
        let response: Response = JsFuture::from(window.fetch_with_request(&request))
            .await
            .map_err(|_| "Bron niet bereikbaar of de browser blokkeert de aanvraag")?
            .dyn_into()
            .map_err(|_| "Ongeldig antwoord")?;
        if !response.ok() {
            return Err(format!("Bron antwoordt met HTTP {}", response.status()));
        }
        let body = response.text().map_err(|_| "Antwoord lezen mislukt")?;
        JsFuture::from(body)
            .await
            .ok()
            .and_then(|value| value.as_string())
            .ok_or_else(|| "Antwoord lezen mislukt".to_owned())
    }
    .await;
    if let Some(timer) = timer {
        window.clear_timeout_with_handle(timer);
    }
    result
}

async fn done(request: &IdbRequest) -> Option<JsValue> {
    let promise = Promise::new(&mut |resolve: Function, reject: Function| {
        request.set_onsuccess(Some(&resolve));
        request.set_onerror(Some(&reject));
    });
    let result = JsFuture::from(promise).await;
    request.set_onsuccess(None);
    request.set_onerror(None);
    result.ok()?;
    request.result().ok()
}

async fn database() -> Option<IdbDatabase> {
    if let Some(db) = DB.with(|value| value.borrow().clone()) {
        return Some(db);
    }
    let request: IdbOpenDbRequest = web_sys::window()?
        .indexed_db()
        .ok()??
        .open_with_u32("iptv-rs", 2)
        .ok()?;
    let opening = request.clone();
    let upgrade = Closure::<dyn FnMut(Event)>::new(move |_| {
        if let Ok(value) = opening.result()
            && let Ok(db) = value.dyn_into::<IdbDatabase>()
        {
            let _ = db.create_object_store(STORE);
            let _ = db.create_object_store(POSTERS);
        }
    });
    request.set_onupgradeneeded(Some(upgrade.as_ref().unchecked_ref()));
    let result = done(&request).await;
    request.set_onupgradeneeded(None);
    let db: IdbDatabase = result?.dyn_into().ok()?;
    DB.with(|value| *value.borrow_mut() = Some(db.clone()));
    Some(db)
}

pub async fn cached<T: DeserializeOwned>(key: &str) -> Option<T> {
    let db = database().await?;
    let store = db
        .transaction_with_str(STORE)
        .ok()?
        .object_store(STORE)
        .ok()?;
    let value = done(&store.get(&JsValue::from_str(key)).ok()?)
        .await?
        .as_string()?;
    let (timestamp, data): (f64, T) = serde_json::from_str(&value).ok()?;
    (Date::now() - timestamp <= CACHE_MS).then_some(data)
}

pub async fn put<T: Serialize>(key: &str, data: &T) {
    let Some(db) = database().await else {
        return;
    };
    let Ok(tx) = db.transaction_with_str_and_mode(STORE, web_sys::IdbTransactionMode::Readwrite)
    else {
        return;
    };
    let Ok(store) = tx.object_store(STORE) else {
        return;
    };
    let Ok(value) = serde_json::to_string(&(Date::now(), data)) else {
        return;
    };
    if let Ok(request) = store.put_with_key(&JsValue::from_str(&value), &JsValue::from_str(key)) {
        let _ = done(&request).await;
    }
}

pub async fn poster(url: &str) -> Option<web_sys::Blob> {
    let db = database().await?;
    let store = db
        .transaction_with_str(POSTERS)
        .ok()?
        .object_store(POSTERS)
        .ok()?;
    done(&store.get(&JsValue::from_str(url)).ok()?)
        .await?
        .dyn_into()
        .ok()
}

pub async fn store_poster(url: &str, image: &web_sys::Blob) {
    let Some(db) = database().await else {
        return;
    };
    let Ok(tx) = db.transaction_with_str_and_mode(POSTERS, web_sys::IdbTransactionMode::Readwrite)
    else {
        return;
    };
    let Ok(store) = tx.object_store(POSTERS) else {
        return;
    };
    let mut order = saved::<Vec<String>>(POSTER_ORDER).unwrap_or_default();
    order.retain(|key| key != url);
    const MAX_POSTERS: usize = 160;
    if order.len() >= MAX_POSTERS {
        for oldest in order.drain(..=order.len() - MAX_POSTERS) {
            let _ = store.delete(&JsValue::from_str(&oldest));
        }
    }
    if let Ok(request) = store.put_with_key(image, &JsValue::from_str(url)) {
        order.push(url.to_owned());
        save(POSTER_ORDER, &order);
        let _ = done(&request).await;
    }
}
