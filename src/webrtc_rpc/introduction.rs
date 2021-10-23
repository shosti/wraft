use crate::console_log;
use futures::channel::mpsc::channel;
use futures::sink::SinkExt;
use futures::stream::StreamExt;
use serde::{Deserialize, Serialize};
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::spawn_local;
use web_sys::{MessageEvent, WebSocket};

static INTRODUCER: &str = "ws://localhost:9999";

#[derive(Serialize, Deserialize, Debug, Clone)]
struct Command {
    command_type: CommandType,
    source_id: String,
    session_id: String,
    sdp_data: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
enum CommandType {
    Join,
    Offer,
    Answer,
    NewIceCandidate,
}

pub async fn initiate(id: String, session_id: String) -> Result<(), JsValue> {
    let ws = WebSocket::new(INTRODUCER)?;
    ws.set_binary_type(web_sys::BinaryType::Arraybuffer);

    let onmessage_cb = Closure::wrap(Box::new(move |e: MessageEvent| {
        console_log!("E: {:#?}", e);
    }) as Box<dyn FnMut(MessageEvent)>);
    ws.set_onmessage(Some(onmessage_cb.as_ref().unchecked_ref()));

    let (opened_tx, mut opened_rx) = channel::<()>(1);
    let onopen_cb = Closure::wrap(Box::new(move || {
        let mut tx = opened_tx.clone();
        spawn_local(async move {
            tx.send(()).await.unwrap();
        });
    }) as Box<dyn FnMut()>);
    ws.set_onopen(Some(onopen_cb.as_ref().unchecked_ref()));

    match opened_rx.next().await {
        Some(()) => console_log!("Got a big fat nothing!"),
        None => panic!("Thought this couldn't happen?"),
    }

    let join_message = Command {
        command_type: CommandType::Join,
        source_id: id,
        session_id,
        sdp_data: None,
    };
    let data = bincode::serialize(&join_message).unwrap();
    ws.send_with_u8_array(&data)?;
    Ok(())
}

pub async fn join(_id: String, _session_id: String) -> Result<(), JsValue> {
    console_log!("JOIN!");
    Ok(())
}
