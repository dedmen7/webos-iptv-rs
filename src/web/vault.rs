use super::io;
use aes_gcm::{
    Aes256Gcm, KeyInit, Nonce,
    aead::{Aead, Payload},
};
use base64::{Engine as _, engine::general_purpose::STANDARD_NO_PAD};
use js_sys::{Array, Object, Reflect, Uint8Array};
use serde::{Serialize, de::DeserializeOwned};
use std::cell::OnceCell;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{AesGcmParams, Crypto, CryptoKey};

const FORMAT: &str = "sealed:v1:";
const COMPRESSED_FORMAT: &str = "sealed:v1z:";
const MAX_LOCAL_RECORD: usize = 32 * 1024 * 1024;
const DEVICE_KEY: &str = "orbis.private-state-key.v1";
const NONCE_LEN: usize = 12;

struct Keys {
    device: CryptoKey,
    local: Aes256Gcm,
}

thread_local! {
    static KEYS: OnceCell<Keys> = const { OnceCell::new() };
}

pub(super) async fn initialize() -> Option<()> {
    let record = match io::read_vault().await? {
        existing if !existing.is_undefined() => existing,
        _ => {
            let device = create_device_key().await?;
            let mut data_key = [0; 32];
            crypto()?
                .get_random_values_with_u8_array(&mut data_key)
                .ok()?;
            let wrapped = encrypt_with_device(&device, DEVICE_KEY, &data_key).await;
            data_key.fill(0);
            let proposed = Array::of2(&device, &wrapped?);
            if io::create_vault(&proposed).await {
                proposed.into()
            } else {
                io::read_vault().await?
            }
        }
    };
    if !Array::is_array(&record) {
        return None;
    }
    let record = Array::from(&record);
    let device: CryptoKey = record.get(0).dyn_into().ok()?;
    if device.extractable() {
        return None;
    }
    let mut data_key = decrypt_with_device(&device, DEVICE_KEY, record.get(1)).await?;
    let local = Aes256Gcm::new_from_slice(&data_key).ok();
    data_key.fill(0);
    KEYS.with(|keys| {
        keys.set(Keys {
            device,
            local: local?,
        })
        .ok()
    })
}

fn crypto() -> Option<Crypto> {
    web_sys::window()?.crypto().ok()
}

async fn create_device_key() -> Option<CryptoKey> {
    let algorithm = Object::new();
    Reflect::set(&algorithm, &"name".into(), &"AES-GCM".into()).ok()?;
    Reflect::set(&algorithm, &"length".into(), &256.into()).ok()?;
    let usages = Array::of2(&"encrypt".into(), &"decrypt".into());
    let promise = crypto()?
        .subtle()
        .generate_key_with_object(&algorithm, false, &usages)
        .ok()?;
    JsFuture::from(promise).await.ok()?.dyn_into().ok()
}

fn nonce() -> Option<[u8; NONCE_LEN]> {
    let mut nonce = [0; NONCE_LEN];
    crypto()?.get_random_values_with_u8_array(&mut nonce).ok()?;
    Some(nonce)
}

fn parameters(iv: &[u8], key: &str) -> AesGcmParams {
    let parameters = AesGcmParams::new_with_u8_array("AES-GCM", &Uint8Array::from(iv));
    parameters.set_additional_data_u8_array(&Uint8Array::from(key.as_bytes()));
    parameters
}

async fn encrypt_with_device(device: &CryptoKey, key: &str, data: &[u8]) -> Option<JsValue> {
    let iv = nonce()?;
    let algorithm = parameters(&iv, key);
    let promise = crypto()?
        .subtle()
        .encrypt_with_object_and_u8_array(&algorithm, device, data)
        .ok()?;
    let ciphertext = JsFuture::from(promise).await.ok()?;
    Some(
        Array::of2(
            &Uint8Array::from(iv.as_slice()),
            &Uint8Array::new(&ciphertext),
        )
        .into(),
    )
}

async fn decrypt_with_device(device: &CryptoKey, key: &str, sealed: JsValue) -> Option<Vec<u8>> {
    if !Array::is_array(&sealed) {
        return None;
    }
    let sealed = Array::from(&sealed);
    let iv: Uint8Array = sealed.get(0).dyn_into().ok()?;
    let ciphertext: Uint8Array = sealed.get(1).dyn_into().ok()?;
    if iv.length() != NONCE_LEN as u32 || ciphertext.length() < 16 {
        return None;
    }
    let algorithm = parameters(&iv.to_vec(), key);
    let promise = crypto()?
        .subtle()
        .decrypt_with_object_and_js_u8_array(&algorithm, device, &ciphertext)
        .ok()?;
    Some(Uint8Array::new(&JsFuture::from(promise).await.ok()?).to_vec())
}

pub(super) async fn seal_cache(key: &str, value: &str) -> Option<JsValue> {
    let device = KEYS.with(|keys| keys.get().map(|keys| keys.device.clone()))?;
    encrypt_with_device(&device, key, value.as_bytes()).await
}

pub(super) async fn open_cache(key: &str, value: JsValue) -> Option<String> {
    let device = KEYS.with(|keys| keys.get().map(|keys| keys.device.clone()))?;
    String::from_utf8(decrypt_with_device(&device, key, value).await?).ok()
}

pub(super) async fn fingerprint(value: &str) -> Option<String> {
    let promise = crypto()?
        .subtle()
        .digest_with_str_and_u8_array("SHA-256", value.as_bytes())
        .ok()?;
    let digest = Uint8Array::new(&JsFuture::from(promise).await.ok()?);
    Some(STANDARD_NO_PAD.encode(digest.to_vec()))
}

pub(super) fn save<T: Serialize>(key: &str, value: &T) -> bool {
    let result = (|| {
        let storage = web_sys::window()?.local_storage().ok()??;
        let bytes = serde_json::to_vec(value).ok()?;
        let encoded = encode_local(key, FORMAT, &bytes)?;
        if storage.set_item(key, &encoded).is_ok() {
            return Some(());
        }
        if bytes.len() > MAX_LOCAL_RECORD {
            return None;
        }
        let compressed = miniz_oxide::deflate::compress_to_vec(&bytes, 1);
        let encoded = encode_local(key, COMPRESSED_FORMAT, &compressed)?;
        storage.set_item(key, &encoded).ok()
    })();
    result.is_some()
}

fn encode_local(key: &str, format: &str, bytes: &[u8]) -> Option<String> {
    let iv = nonce()?;
    let payload = Payload {
        msg: bytes,
        aad: key.as_bytes(),
    };
    let encrypted = KEYS.with(|keys| {
        keys.get()?
            .local
            .encrypt(Nonce::from_slice(&iv), payload)
            .ok()
    })?;
    let mut combined = Vec::with_capacity(iv.len() + encrypted.len());
    combined.extend_from_slice(&iv);
    combined.extend(encrypted);
    Some(format.to_owned() + &STANDARD_NO_PAD.encode(combined))
}

pub(super) fn load<T: DeserializeOwned + Serialize>(key: &str) -> Result<Option<T>, ()> {
    let storage = web_sys::window()
        .ok_or(())?
        .local_storage()
        .map_err(|_| ())?
        .ok_or(())?;
    let Some(value) = storage.get_item(key).map_err(|_| ())? else {
        return Ok(None);
    };
    let compressed = value.starts_with(COMPRESSED_FORMAT);
    if let Some(encoded) = value
        .strip_prefix(FORMAT)
        .or_else(|| value.strip_prefix(COMPRESSED_FORMAT))
    {
        let bytes = STANDARD_NO_PAD.decode(encoded).map_err(|_| ())?;
        let (iv, encrypted) = bytes.split_at_checked(NONCE_LEN).ok_or(())?;
        let payload = Payload {
            msg: encrypted,
            aad: key.as_bytes(),
        };
        let data = KEYS
            .with(|keys| {
                keys.get()?
                    .local
                    .decrypt(Nonce::from_slice(iv), payload)
                    .ok()
            })
            .ok_or(())?;
        let data = if compressed {
            miniz_oxide::inflate::decompress_to_vec_with_limit(&data, MAX_LOCAL_RECORD)
                .map_err(|_| ())?
        } else {
            data
        };
        serde_json::from_slice(&data).map(Some).map_err(|_| ())
    } else {
        let parsed: T = serde_json::from_str(&value).map_err(|_| ())?;
        if !save(key, &parsed) {
            return Err(());
        }
        Ok(Some(parsed))
    }
}
