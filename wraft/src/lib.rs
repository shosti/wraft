pub mod raft;
pub mod util;
mod webrtc_rpc;

use crate::util::Interval;
use futures::prelude::*;
use raft::Raft;
use rand::distributions::Alphanumeric;
use rand::{thread_rng, Rng};
use std::time::Duration;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::spawn_local;
use web_sys::{
    Document, Event, HtmlButtonElement, HtmlElement, HtmlFormElement, HtmlInputElement, Window,
};

const SESSION_KEY_LEN: usize = 20;

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

        let hn = hn_start.clone();
        let session_key = generate_session_key();

        spawn_local(run_raft(hn, session_key, 3));
    }) as Box<dyn FnMut(Event)>);
    start_button.set_onclick(Some(start.as_ref().unchecked_ref()));

    let join_elem = document
        .get_element_by_id("join-form")
        .expect("No join form found");
    let join_form = join_elem
        .dyn_ref::<HtmlFormElement>()
        .expect("#join-form should be a form");
    let hn_join = hostname.clone();
    let join = Closure::wrap(Box::new(move |ev: Event| {
        ev.prevent_default();

        let hn = hn_join.clone();
        let session_key = get_join_session_key();
        if session_key.len() != SESSION_KEY_LEN {
            panic!("BAD SESSION KEY");
        }

        spawn_local(run_raft(hn, session_key, 3));
    }) as Box<dyn FnMut(Event)>);
    join_form.set_onsubmit(Some(join.as_ref().unchecked_ref()));

    future::pending::<()>().await;
    unreachable!();
}

async fn run_raft(node_id: String, session_key: String, cluster_size: usize) {
    let document = get_document();
    let session_key_elem = document
        .get_element_by_id("session-key")
        .expect("#session-key not found");
    let sk = session_key_elem.dyn_ref::<HtmlElement>().unwrap();
    sk.set_inner_html(format!("<h2>Session key: {}</h2>", session_key).as_str());
    hide_start_form();

    let raft_elem = document.get_element_by_id("raft").expect("#raft not found");
    let raft_content = raft_elem.dyn_ref::<HtmlElement>().unwrap();

    let raft = Raft::new(node_id, session_key, cluster_size);

    let mut interval = Interval::new(Duration::from_secs(1));

    while let Some(()) = interval.next().await {
        let content = format!("<pre>{:#?}</pre>", raft);
        raft_content.set_inner_html(&content);
    }
}

fn generate_session_key() -> String {
    thread_rng()
        .sample_iter(&Alphanumeric)
        .take(20)
        .map(char::from)
        .collect()
}

fn get_join_session_key() -> String {
    let elem = get_document()
        .get_element_by_id("join-session-key")
        .expect("join-session-key input not found");
    elem.dyn_ref::<HtmlInputElement>()
        .expect("join-session-key should be an input element")
        .value()
}

fn hide_start_form() {
    let elem = get_document()
        .get_element_by_id("start")
        .expect("start section not found");
    elem.dyn_ref::<HtmlElement>()
        .expect("start section should be an HTML element")
        .set_hidden(true);
}

fn get_window() -> Window {
    web_sys::window().expect("no global window")
}

fn get_document() -> Document {
    get_window().document().expect("no global document exists")
}
