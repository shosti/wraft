pub mod raft;
pub mod util;
mod webrtc_rpc;

use crate::util::{sleep_fused, Interval};
use futures::prelude::*;
use futures::select;
use raft::Raft;
use rand::distributions::Alphanumeric;
use rand::{thread_rng, Rng};
use std::time::Duration;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::spawn_local;
use web_sys::{Document, Event, HtmlButtonElement, HtmlElement, Window};

// Use `wee_alloc` as the global allocator.
#[global_allocator]
static ALLOC: wee_alloc::WeeAlloc = wee_alloc::WeeAlloc::INIT;

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
        ev.prevent_default();

        let content_elem = document.get_element_by_id("content").unwrap();
        let content = content_elem.dyn_ref::<HtmlElement>().unwrap();
        let hn = hn_start.clone();
        let session_key = generate_session_key();
        content.set_inner_html(format!("<h2>Session key: {}</h2>", session_key).as_str());
        hide_start_form();
        spawn_local(async move {
            let mut _raft = Raft::new(hn.clone(), session_key, 3);
            // raft.run().await.unwrap();
        });
    }) as Box<dyn FnMut(Event)>);
    start_button.set_onclick(Some(start.as_ref().unchecked_ref()));

    let mut interval = Interval::new(Duration::from_millis(100));
    let mut timeout = sleep_fused(Duration::from_secs(5));
    loop {
        select! {
            _ = interval.next() => {
                console_log!("YO");
            }
            _ = timeout => {
                break;
            }
        }
    }
    console_log!("DONE");
}

fn generate_session_key() -> String {
    thread_rng()
        .sample_iter(&Alphanumeric)
        .take(20)
        .map(char::from)
        .collect()
}

// fn get_session_key() -> String {
//     let elem = get_document()
//         .get_element_by_id("session-key")
//         .expect("session-key input not found");
//     elem.dyn_ref::<HtmlInputElement>()
//         .expect("session-key should be an input element")
//         .value()
// }

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
