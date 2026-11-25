pub mod catalog;
pub mod guide;
pub mod playlist;
pub mod subtitles;

#[cfg(target_arch = "wasm32")]
mod web;
