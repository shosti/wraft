pub mod raft;
pub mod ringbuf;
pub mod util;
mod webrtc_rpc;
use wasm_bindgen::prelude::*;
mod todo;

// Use `wee_alloc` as the global allocator.
#[global_allocator]
static ALLOC: wee_alloc::WeeAlloc = wee_alloc::WeeAlloc::INIT;

#[wasm_bindgen(start)]
pub async fn start() {
    util::set_panic_hook();

    yew::start_app::<todo::Model>();
}
