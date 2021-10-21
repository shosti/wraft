mod utils;

use wasm_bindgen::prelude::*;
use web_sys::{console, Element};
use std::time::Duration;

// When the `wee_alloc` feature is enabled, use `wee_alloc` as the global
// allocator.
#[cfg(feature = "wee_alloc")]
#[global_allocator]
static ALLOC: wee_alloc::WeeAlloc = wee_alloc::WeeAlloc::INIT;

struct Raft {
    elem: Element,
}

impl Raft {
    pub async fn run(&mut self) {
        console::log_1(&"YO!".into());
        self.elem.set_inner_html("<h1>YOOOOO</h1>");
        utils::sleep(Duration::from_secs(3)).await.unwrap();
        self.elem.set_inner_html("<h1>HAHAHA</h1>");
    }
}

#[wasm_bindgen]
pub async fn start(elem: Element) {
    utils::set_panic_hook();
    let mut raft = Raft { elem: elem };
    raft.run().await
}
