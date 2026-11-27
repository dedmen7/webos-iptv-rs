use super::i18n::{message, tr};
use super::vault;
use js_sys::{Date, Function, Promise, Reflect, Uint8Array};
use serde::{Serialize, de::DeserializeOwned};
use std::{cell::RefCell, future::Future};
use wasm_bindgen::{JsCast, JsValue, closure::Closure};
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    AbortController, Blob, Event, IdbDatabase, IdbOpenDbRequest, IdbRequest,
    ReadableStreamDefaultReader, Request, RequestCache, RequestInit, Response,
};

const CATALOG_DB_VERSION: u32 = 4;
const STORE: &str = "catalog";
const POSTERS: &str = "posters";
const VAULT: &str = "vault";
const DEVICE_KEY: &str = "device-key";
const POSTER_ORDER: &str = "iptv-rs.poster-order-v2";
const HOUR_MS: f64 = 60. * 60. * 1000.;
pub const CATALOG_FRESH_MS: f64 = 12. * HOUR_MS;
pub const CATALOG_RETENTION_MS: f64 = 7. * 24. * HOUR_MS;
pub const GUIDE_FRESH_MS: f64 = 3. * HOUR_MS;
pub const GUIDE_RETENTION_MS: f64 = 2. * 24. * HOUR_MS;
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

pub(super) fn remove(key: &str) {
    if let Some(storage) =
        web_sys::window().and_then(|window| window.local_storage().ok().flatten())
    {
        let _ = storage.remove_item(key);
    }
}

pub(super) fn save_private<T: Serialize>(key: &str, value: &T) -> bool {
    vault::save(key, value)
}

pub async fn text(
    url: &str,
    headers: &[(&str, &str)],
    body: Option<&str>,
) -> Result<String, String> {
    request_text(url, RequestOptions { headers, body }).await
}

pub async fn image_blob(url: &str) -> Option<Blob> {
    const MAX_IMAGE_SIZE: u32 = 768_000;
    let mut deadline = Deadline::new().ok()?;
    deadline.reset(8_000);
    let response = fetch(&deadline, url, RequestOptions::default())
        .await
        .ok()?;
    let size = response
        .headers()
        .get("content-length")
        .ok()
        .flatten()
        .and_then(|value| value.parse::<u32>().ok());
    if size.is_some_and(|size| size > MAX_IMAGE_SIZE) {
        return None;
    }
    let blob: Blob = JsFuture::from(response.blob().ok()?)
        .await
        .ok()?
        .dyn_into()
        .ok()?;
    (blob.size() <= f64::from(MAX_IMAGE_SIZE) && blob.type_().starts_with("image/")).then_some(blob)
}

struct Deadline {
    controller: AbortController,
    window: web_sys::Window,
    timer: Option<i32>,
    callback: Closure<dyn FnMut()>,
}

impl Deadline {
    fn new() -> Result<Self, String> {
        let window = web_sys::window().ok_or(tr("Browservenster ontbreekt"))?;
        let controller = AbortController::new().map_err(|_| tr("Verbinding instellen mislukt"))?;
        let cancel = controller.clone();
        let callback = Closure::<dyn FnMut()>::new(move || cancel.abort());
        Ok(Self {
            controller,
            window,
            timer: None,
            callback,
        })
    }

    fn reset(&mut self, milliseconds: i32) {
        if let Some(timer) = self.timer.take() {
            self.window.clear_timeout_with_handle(timer);
        }
        self.timer = self
            .window
            .set_timeout_with_callback_and_timeout_and_arguments_0(
                self.callback.as_ref().unchecked_ref(),
                milliseconds,
            )
            .ok();
    }
}

impl Drop for Deadline {
    fn drop(&mut self) {
        if let Some(timer) = self.timer {
            self.window.clear_timeout_with_handle(timer);
        }
        self.controller.abort();
    }
}

pub struct CatalogBody {
    deadline: Deadline,
    reader: ReadableStreamDefaultReader,
}

impl CatalogBody {
    pub async fn open(url: &str) -> Result<Self, String> {
        let mut deadline = Deadline::new()?;
        deadline.reset(45_000);
        let response = fetch(&deadline, url, RequestOptions::default()).await?;
        let reader = response
            .body()
            .ok_or(tr("Antwoord lezen mislukt"))?
            .get_reader()
            .dyn_into::<ReadableStreamDefaultReader>()
            .map_err(|_| tr("Antwoord lezen mislukt"))?;
        Ok(Self { deadline, reader })
    }

    pub async fn next(&mut self) -> Result<Option<Vec<u8>>, String> {
        self.deadline.reset(15_000);
        let chunk = JsFuture::from(self.reader.read())
            .await
            .map_err(|_| tr("Antwoord lezen mislukt"))?;
        let done = Reflect::get(&chunk, &JsValue::from_str("done"))
            .map_err(|_| tr("Antwoord lezen mislukt"))?;
        if done.as_bool() == Some(true) {
            return Ok(None);
        }
        let bytes = Reflect::get(&chunk, &JsValue::from_str("value"))
            .ok()
            .and_then(|value| value.dyn_into::<Uint8Array>().ok())
            .ok_or(tr("Antwoord lezen mislukt"))?;
        Ok(Some(bytes.to_vec()))
    }
}

#[derive(Default)]
struct RequestOptions<'a> {
    headers: &'a [(&'a str, &'a str)],
    body: Option<&'a str>,
}

async fn request_text(url: &str, options: RequestOptions<'_>) -> Result<String, String> {
    let mut deadline = Deadline::new()?;
    deadline.reset(25_000);
    let response = fetch(&deadline, url, options).await?;
    let body = response.text().map_err(|_| tr("Antwoord lezen mislukt"))?;
    JsFuture::from(body)
        .await
        .ok()
        .and_then(|value| value.as_string())
        .ok_or_else(|| tr("Antwoord lezen mislukt").to_owned())
}

async fn fetch(
    deadline: &Deadline,
    url: &str,
    request_options: RequestOptions<'_>,
) -> Result<Response, String> {
    let options = RequestInit::new();
    options.set_signal(Some(&deadline.controller.signal()));
    options.set_cache(RequestCache::NoStore);
    if let Some(body) = request_options.body {
        options.set_method("POST");
        options.set_body(&JsValue::from_str(body));
    }
    let request =
        Request::new_with_str_and_init(url, &options).map_err(|_| tr("Ongeldige aanvraag"))?;
    for (name, value) in request_options.headers {
        request
            .headers()
            .set(name, value)
            .map_err(|_| tr("Ongeldige aanvraagheader"))?;
    }
    let response: Response = JsFuture::from(deadline.window.fetch_with_request(&request))
        .await
        .map_err(|_| tr("Bron niet bereikbaar of de browser blokkeert de aanvraag"))?
        .dyn_into()
        .map_err(|_| tr("Ongeldig antwoord"))?;
    if !response.ok() {
        return Err(message(
            "Bron antwoordt met HTTP {status}",
            &[("status", &response.status().to_string())],
        ));
    }
    Ok(response)
}

fn request_completion(request: &IdbRequest) -> JsFuture {
    JsFuture::from(Promise::new(&mut |resolve: Function, reject: Function| {
        request.set_onsuccess(Some(&resolve));
        request.set_onerror(Some(&reject));
    }))
}

async fn request_result(request: &IdbRequest, completion: JsFuture) -> Option<JsValue> {
    let result = completion.await;
    request.set_onsuccess(None);
    request.set_onerror(None);
    result.ok()?;
    request.result().ok()
}

async fn done(request: &IdbRequest) -> Option<JsValue> {
    request_result(request, request_completion(request)).await
}

async fn database() -> Option<IdbDatabase> {
    if let Some(db) = DB.with(|value| value.borrow().clone()) {
        return Some(db);
    }
    let request: IdbOpenDbRequest = web_sys::window()?
        .indexed_db()
        .ok()??
        .open_with_u32("iptv-rs", CATALOG_DB_VERSION)
        .ok()?;
    let opening = request.clone();
    let upgrade = Closure::<dyn FnMut(Event)>::new(move |_| {
        if let Ok(value) = opening.result()
            && let Ok(db) = value.dyn_into::<IdbDatabase>()
        {
            let stores = db.object_store_names();
            for name in [STORE, POSTERS, VAULT] {
                if !stores.contains(name) {
                    let _ = db.create_object_store(name);
                } else if name != VAULT
                    && let Some(tx) = opening.transaction()
                    && let Ok(store) = tx.object_store(name)
                {
                    let _ = store.clear();
                }
            }
        }
    });
    request.set_onupgradeneeded(Some(upgrade.as_ref().unchecked_ref()));
    let completion = JsFuture::from(Promise::new(&mut |resolve, reject| {
        request.set_onsuccess(Some(&resolve));
        request.set_onerror(Some(&reject));
        request.set_onblocked(Some(&reject));
    }));
    let result = request_result(&request, completion).await;
    request.set_onblocked(None);
    request.set_onupgradeneeded(None);
    let db: IdbDatabase = result?.dyn_into().ok()?;
    let stores = db.object_store_names();
    if ![STORE, POSTERS, VAULT]
        .into_iter()
        .all(|name| stores.contains(name))
    {
        db.close();
        return None;
    }
    DB.with(|value| *value.borrow_mut() = Some(db.clone()));
    Some(db)
}

pub(super) async fn read_vault() -> Option<JsValue> {
    let db = database().await?;
    let tx = db.transaction_with_str(VAULT).ok()?;
    let store = tx.object_store(VAULT).ok()?;
    done(&store.get(&DEVICE_KEY.into()).ok()?).await
}

pub(super) async fn create_vault(record: &JsValue) -> bool {
    let Some(db) = database().await else {
        return false;
    };
    let Ok(tx) = db.transaction_with_str_and_mode(VAULT, web_sys::IdbTransactionMode::Readwrite)
    else {
        return false;
    };
    let Ok(store) = tx.object_store(VAULT) else {
        return false;
    };
    if store.add_with_key(record, &DEVICE_KEY.into()).is_err() {
        return false;
    }
    transaction_committed(&tx).await
}

async fn transaction_committed(tx: &web_sys::IdbTransaction) -> bool {
    let completion = Promise::new(&mut |resolve, reject| {
        tx.set_oncomplete(Some(&resolve));
        tx.set_onabort(Some(&reject));
    });
    let committed = JsFuture::from(completion).await.is_ok();
    tx.set_oncomplete(None);
    tx.set_onabort(None);
    committed
}

pub async fn cached_raw(key: &str) -> Option<String> {
    let db = database().await?;
    let store = db
        .transaction_with_str(STORE)
        .ok()?
        .object_store(STORE)
        .ok()?;
    let value = done(&store.get(&JsValue::from_str(key)).ok()?).await?;
    vault::open_cache(key, value).await
}

pub(super) async fn cached_raw_group(
    keys: impl Iterator<Item = String>,
) -> Option<Vec<impl Future<Output = Option<String>>>> {
    let db = database().await?;
    let store = db
        .transaction_with_str(STORE)
        .ok()?
        .object_store(STORE)
        .ok()?;
    let requests = keys
        .map(|key| {
            store
                .get(&JsValue::from_str(&key))
                .map(|request| (key, request))
        })
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    Some(
        requests
            .into_iter()
            .map(|(key, request)| {
                let completion = request_completion(&request);
                async move {
                    let value = request_result(&request, completion).await?;
                    vault::open_cache(&key, value).await
                }
            })
            .collect(),
    )
}

pub struct CacheEntry<T> {
    pub timestamp_ms: f64,
    pub age_ms: f64,
    pub value: T,
}

pub fn decode_recent<T: DeserializeOwned>(value: &str, max_age: f64) -> Option<CacheEntry<T>> {
    let (clock, payload) = value
        .strip_prefix('[')?
        .strip_suffix(']')?
        .split_once(',')?;
    let timestamp = clock.parse::<f64>().ok()?;
    let age_ms = (Date::now() - timestamp).max(0.);
    if !timestamp.is_finite() || age_ms > max_age {
        return None;
    }
    Some(CacheEntry {
        timestamp_ms: timestamp,
        age_ms,
        value: serde_json::from_str(payload).ok()?,
    })
}

pub async fn read_recent<T: DeserializeOwned>(key: &str, max_age: f64) -> Option<CacheEntry<T>> {
    decode_recent(&cached_raw(key).await?, max_age)
}

pub async fn put<T: Serialize + ?Sized>(key: &str, data: &T) -> bool {
    put_at(key, data, Date::now()).await
}

pub async fn put_at<T: Serialize + ?Sized>(key: &str, data: &T, timestamp_ms: f64) -> bool {
    let Ok(value) = serde_json::to_string(&(timestamp_ms, data)) else {
        return false;
    };
    let Some(sealed) = vault::seal_cache(key, &value).await else {
        return false;
    };
    let Some(db) = database().await else {
        return false;
    };
    let Ok(tx) = db.transaction_with_str_and_mode(STORE, web_sys::IdbTransactionMode::Readwrite)
    else {
        return false;
    };
    let Ok(store) = tx.object_store(STORE) else {
        return false;
    };
    if store
        .put_with_key(&sealed, &JsValue::from_str(key))
        .is_err()
    {
        return false;
    }
    transaction_committed(&tx).await
}

pub async fn clear_catalog_entries(keys: impl Iterator<Item = String>) {
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
    for key in keys {
        let _ = store.delete(&JsValue::from_str(&key));
    }
}

pub async fn poster(url: &str) -> Option<web_sys::Blob> {
    let key = vault::fingerprint(url).await?;
    let db = database().await?;
    let store = db
        .transaction_with_str(POSTERS)
        .ok()?
        .object_store(POSTERS)
        .ok()?;
    done(&store.get(&JsValue::from_str(&key)).ok()?)
        .await?
        .dyn_into()
        .ok()
}

pub async fn store_poster(url: &str, image: &web_sys::Blob) {
    let Some(key) = vault::fingerprint(url).await else {
        return;
    };
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
    order.retain(|saved_key| saved_key != &key);
    const MAX_POSTERS: usize = 160;
    if order.len() >= MAX_POSTERS {
        for oldest in order.drain(..=order.len() - MAX_POSTERS) {
            let _ = store.delete(&JsValue::from_str(&oldest));
        }
    }
    if let Ok(request) = store.put_with_key(image, &JsValue::from_str(&key)) {
        order.push(key);
        save(POSTER_ORDER, &order);
        let _ = done(&request).await;
    }
}

pub async fn delay(ms: i32) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let promise = Promise::new(&mut |resolve, _reject| {
        if window
            .set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms)
            .is_err()
        {
            let _ = resolve.call0(&JsValue::UNDEFINED);
        }
    });
    let _ = JsFuture::from(promise).await;
}

pub async fn after_frame() {
    let Some(window) = web_sys::window() else {
        return;
    };
    let promise = Promise::new(&mut |resolve, _reject| {
        if window.request_animation_frame(&resolve).is_err() {
            let _ = resolve.call0(&JsValue::UNDEFINED);
        }
    });
    let frame = async {
        let _ = JsFuture::from(promise).await;
        delay(0).await;
    };
    let _ = futures::future::select(Box::pin(frame), Box::pin(delay(40))).await;
}
