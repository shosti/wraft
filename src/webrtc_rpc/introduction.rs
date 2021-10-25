use crate::console_log;
use crate::webrtc_rpc::error::Error;
use futures::channel::mpsc::channel;
use futures::channel::oneshot;
use futures::select;
use futures::sink::SinkExt;
use futures::stream::StreamExt;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::spawn_local;
use web_sys::{MessageEvent, WebSocket};
use yenta_types::{Command, JoinInfo};

static INTRODUCER: &str = "ws://localhost:9999";

pub async fn initiate(id: &str, session_id: &str) -> Result<(), Error> {
    let (_done, mut wait_done) = oneshot::channel::<()>();
    let (errors_tx, mut errors_rx) = channel::<Error>(10);
    let ws = WebSocket::new(INTRODUCER)?;
    ws.set_binary_type(web_sys::BinaryType::Arraybuffer);

    let onmessage_ws = ws.clone();
    let onmessage_cb = Closure::wrap(Box::new(move |e: MessageEvent| {
        let ws = onmessage_ws.clone();
        let mut errors = errors_tx.clone();
        spawn_local(async move {
            match handle_message(e, ws).await {
                Ok(()) => (),
                Err(err) => {
                    errors.send(err).await.unwrap();
                }
            }
        });
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
        Some(()) => (),
        None => panic!("Thought this couldn't happen?"),
    }

    let join_info = JoinInfo {
        node_id: id.to_string(),
        session_id: session_id.to_string(),
    };
    let cmd = Command::Join(join_info);
    let data = bincode::serialize(&cmd)?;
    ws.send_with_u8_array(&data)?;
    select! {
        _ = wait_done => Ok(()),
        err = errors_rx.next() => Err(err.unwrap()),
    }
}

async fn handle_message(e: MessageEvent, _ws: WebSocket) -> Result<(), Error> {
    let abuf = e
        .data()
        .dyn_into::<js_sys::ArrayBuffer>()
        .expect("Expected message in binary format");
    let data = js_sys::Uint8Array::new(&abuf).to_vec();
    let msg: Command = bincode::deserialize(&data)?;
    console_log!("MSG: {:#?}", msg);
    Ok(())
}
