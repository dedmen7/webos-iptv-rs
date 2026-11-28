pub mod catalog;
pub mod guide;
#[cfg(any(target_arch = "wasm32", test))]
mod json_array;
#[cfg(any(target_arch = "wasm32", test))]
mod live_feeds;
#[cfg(any(target_arch = "wasm32", test))]
mod live_routes;
pub mod playlist;
pub mod series;
pub mod subtitles;
pub mod sync;

#[cfg(target_arch = "wasm32")]
mod web;
