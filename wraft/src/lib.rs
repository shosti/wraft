pub mod raft;
pub mod ringbuf;
pub mod util;
mod webrtc_rpc;

use crate::util::sleep;
use futures::prelude::*;
use futures::select;
use raft::Raft;
use rand::{thread_rng, Rng};
use std::time::Duration;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::spawn_local;
use web_sys::{
    Document, Event, HtmlButtonElement, HtmlElement, HtmlFormElement, HtmlInputElement, Window,
};

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
        let session_key = get_join_session_key().unwrap();

        spawn_local(run_raft(hn, session_key, 3));
    }) as Box<dyn FnMut(Event)>);
    join_form.set_onsubmit(Some(join.as_ref().unchecked_ref()));

    future::pending::<()>().await;
    unreachable!();
}

async fn run_raft(hostname: String, session_key: u128, cluster_size: usize) {
    let document = get_document();
    let session_key_elem = document
        .get_element_by_id("session-key")
        .expect("#session-key not found");
    let sk = session_key_elem.dyn_ref::<HtmlElement>().unwrap();
    sk.set_inner_html(format!("<h2>Session key: {:032x}</h2>", session_key).as_str());
    hide_start_form();

    let raft = Raft::initiate(&hostname, session_key, cluster_size)
        .await
        .unwrap();

    setup_controls(raft);

    future::pending::<()>().await;
    unreachable!();
}

fn setup_controls(raft: Raft<String>) {
    let document = get_document();
    let set_form_elem = document.get_element_by_id("set-form").unwrap();
    let set_form = set_form_elem.dyn_ref::<HtmlFormElement>().unwrap();

    let set_raft = raft.clone();
    let set = Closure::wrap(Box::new(move |ev: Event| {
        ev.prevent_default();

        let r = set_raft.clone();
        spawn_local(async move {
            let document = get_document();
            let set_key_elem = document.get_element_by_id("set-key").unwrap();
            let set_key = set_key_elem.dyn_ref::<HtmlInputElement>().unwrap();
            let key = set_key.value();

            let set_val_elem = document.get_element_by_id("set-value").unwrap();
            let set_val = set_val_elem.dyn_ref::<HtmlInputElement>().unwrap();
            let val = set_val.value();

            if key.is_empty() || val.is_empty() {
                return;
            }
            let _ = r.set(key, val).await;
        });
    }) as Box<dyn FnMut(Event)>);
    set_form.set_onsubmit(Some(set.as_ref().unchecked_ref()));
    set.forget();

    let get_form_elem = document.get_element_by_id("get-form").unwrap();
    let get_form = get_form_elem.dyn_ref::<HtmlFormElement>().unwrap();

    let get_raft = raft.clone();
    let get = Closure::wrap(Box::new(move |ev: Event| {
        ev.prevent_default();

        let r = get_raft.clone();
        spawn_local(async move {
            let document = get_document();
            let get_key_elem = document.get_element_by_id("get-key").unwrap();
            let get_key = get_key_elem.dyn_ref::<HtmlInputElement>().unwrap();
            let key = get_key.value();

            if key.is_empty() {
                return;
            }
            let val_elem = document.get_element_by_id("get-value").unwrap();
            let val = val_elem.dyn_ref::<HtmlElement>().unwrap();
            match r.get(key).await {
                Ok(Some(ref v)) => val.set_inner_text(v),
                Ok(None) => val.set_inner_text("NOT FOUND"),
                Err(err) => console_log!("Error: {:?}", err),
            }
        });
    }) as Box<dyn FnMut(Event)>);
    get_form.set_onsubmit(Some(get.as_ref().unchecked_ref()));
    get.forget();

    let bench_elem = document.get_element_by_id("benchmark").unwrap();
    let bench_form = bench_elem.dyn_ref::<HtmlFormElement>().unwrap();

    let bench = Closure::wrap(Box::new(move |ev: Event| {
        ev.prevent_default();

        let perf = get_window().performance().unwrap();
        let r = raft.clone();
        spawn_local(async move {
            console_log!("BEGINNING BENCHMARK");
            let mut stop = sleep(Duration::from_secs(10));
            let t0 = perf.now();
            let mut n = 0;
            let mut errs = 0;
            loop {
                select! {
                    _ = stop => {
                        break;
                    }
                    default => {
                        if let Ok(()) = r.set("iter".to_string(), n.to_string()).await {
                            n += 1;
                        } else {
                            errs += 1;
                        }
                    }
                }
            }
            let t1 = perf.now();
            let elapsed = t1 - t0;
            console_log!(
                "{} writes in {} milliseconds ({} writes/sec, {} errors)",
                n,
                elapsed,
                (n as f64) / ((elapsed as f64) / 1000.0),
                errs,
            );
        });
    }) as Box<dyn FnMut(Event)>);
    bench_form.set_onsubmit(Some(bench.as_ref().unchecked_ref()));
    bench.forget();
}

fn generate_session_key() -> u128 {
    thread_rng().gen()
}

fn get_join_session_key() -> Result<u128, std::num::ParseIntError> {
    let elem = get_document()
        .get_element_by_id("join-session-key")
        .expect("join-session-key input not found");
    let val = elem
        .dyn_ref::<HtmlInputElement>()
        .expect("join-session-key should be an input element")
        .value();
    u128::from_str_radix(&val, 16)
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
