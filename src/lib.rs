pub mod util;
mod webrtc_rpc;

use futures::prelude::*;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{Document, Event, HtmlButtonElement, HtmlElement, HtmlInputElement, Window};
use webrtc_rpc::introduce;
use wasm_bindgen_futures::spawn_local;

#[derive(Clone, Debug)]
enum RpcRequest {}

#[derive(Clone, Debug)]
enum RpcResponse {}

// Use `wee_alloc` as the global allocator.
#[global_allocator]
static ALLOC: wee_alloc::WeeAlloc = wee_alloc::WeeAlloc::INIT;

// struct Raft {
//     elem: Element,
// }

// impl Raft {
//     pub async fn run(&mut self) {
//         console::log_1(&"YO!".into());
//         self.elem.set_inner_html("<h1>YOOOOO</h1>");
//         utils::sleep(Duration::from_secs(3)).await.unwrap();
//         self.elem.set_inner_html("<h1>HAHAHA</h1>");
//     }
// }

#[wasm_bindgen(start)]
pub async fn start() {
    util::set_panic_hook();
    let hostname = get_window().location().hostname().expect("no hostname");
    let document = get_document();

    let start_button_elem = document
        .get_element_by_id("start-button")
        .expect("No start button found");
    let start_button = start_button_elem
        .dyn_ref::<HtmlButtonElement>()
        .expect("#start-button should be a button element");
    let hn_start = hostname.clone();
    let start = Closure::wrap(Box::new(move |ev: Event| {
        let hn = hn_start.clone();
        ev.prevent_default();
        hide_start_form();
        spawn_local(async move {
            introduce::<RpcRequest, RpcResponse>(hn.clone(), get_session_key()).await.unwrap();
        });
    }) as Box<dyn FnMut(Event)>);
    start_button.set_onclick(Some(start.as_ref().unchecked_ref()));

    let forever = future::pending();
    let () = forever.await;
    unreachable!();
}

fn get_session_key() -> String {
    let elem = get_document()
        .get_element_by_id("session-key")
        .expect("session-key input not found");
    elem.dyn_ref::<HtmlInputElement>()
        .expect("session-key should be an input element")
        .value()
}

fn hide_start_form() {
    let elem = get_document()
        .get_element_by_id("start-form")
        .expect("start-form not found");
    elem.dyn_ref::<HtmlElement>()
        .expect("start-form should be an HTML element")
        .set_hidden(true);
}

fn get_window() -> Window {
    web_sys::window().expect("no global window")
}

fn get_document() -> Document {
    get_window().document().expect("no global document exists")
}
