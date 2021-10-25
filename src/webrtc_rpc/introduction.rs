use crate::console_log;
use crate::webrtc_rpc::error::Error;
use futures::channel::mpsc::channel;
use futures::channel::oneshot;
use futures::select;
use futures::sink::SinkExt;
use futures::stream::StreamExt;
use std::collections::HashMap;
use std::sync::Arc;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::spawn_local;
use web_sys::{MessageEvent, RtcPeerConnection, WebSocket};
use yenta_types::{Command, JoinInfo, SessionInfo};

static INTRODUCER: &str = "ws://localhost:9999";

#[derive(Clone)]
struct State {
    node_id: Arc<String>,
    peers: Arc<HashMap<String, RtcPeerConnection>>,
}

pub async fn initiate(node_id: &str, session_id: &str) -> Result<(), Error> {
    let state: State = State {
        node_id: Arc::new(node_id.to_string()),
        peers: Arc::new(HashMap::new()),
    };

    let (_done, mut wait_done) = oneshot::channel::<()>();
    let (errors_tx, mut errors_rx) = channel::<Error>(10);
    let ws = WebSocket::new(INTRODUCER)?;
    ws.set_binary_type(web_sys::BinaryType::Arraybuffer);

    let onmessage_ws = ws.clone();
    let onmessage_cb = Closure::wrap(Box::new(move |e: MessageEvent| {
        let ws = onmessage_ws.clone();
        let mut errors = errors_tx.clone();
        let s = state.clone();
        spawn_local(async move {
            match handle_message(e, ws, s.clone()).await {
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
        node_id: node_id.to_string(),
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

async fn handle_message(e: MessageEvent, ws: WebSocket, state: State) -> Result<(), Error> {
    let abuf = e
        .data()
        .dyn_into::<js_sys::ArrayBuffer>()
        .expect("Expected message in binary format");
    let data = js_sys::Uint8Array::new(&abuf).to_vec();
    let command: Command = bincode::deserialize(&data)?;

    match command {
        Command::SessionStatus(session) => handle_session_update(session, ws, state).await,
        _ => {
            console_log!("COMMAND: {:#?}", command);
            Ok(())
        }
    }
}

async fn handle_session_update(session: SessionInfo, _ws: WebSocket, state: State) -> Result<(), Error> {
    for p in session.online {
        if p > *state.node_id && !state.peers.contains_key(&p) {
            console_log!("GOING TO CONNECT TO {}", p);
        }
    }
    Ok(())
}
