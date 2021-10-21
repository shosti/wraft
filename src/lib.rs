mod utils;

use std::time::Duration;
use wasm_bindgen::prelude::*;
use web_sys::{console, Element};

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

#[wasm_bindgen(start)]
pub async fn start() {
    utils::set_panic_hook();
    let document = web_sys::window()
        .expect("no global window")
        .document()
        .expect("window should have document");
    let elem = document
        .get_element_by_id("content")
        .expect("No content found");
    let mut raft = Raft { elem };
    raft.run().await;
}
