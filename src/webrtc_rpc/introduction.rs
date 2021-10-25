use crate::console_log;
use crate::webrtc_rpc::error::Error;
use futures::channel::mpsc::channel;
use futures::channel::oneshot;
use futures::select;
use futures::sink::SinkExt;
use futures::stream::{FuturesUnordered, StreamExt};
use js_sys::Reflect;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::{spawn_local, JsFuture};
use web_sys::{MessageEvent, RtcPeerConnection, RtcSdpType, RtcSessionDescriptionInit, WebSocket};
use yenta_types::{Command, Join, Offer, Session};

static INTRODUCER: &str = "ws://localhost:9999";

#[derive(Clone)]
struct State {
    node_id: Arc<String>,
    session_id: Arc<String>,
    peers: Arc<RwLock<HashMap<String, RtcPeerConnection>>>,
}

pub async fn initiate(node_id: &str, session_id: &str) -> Result<(), Error> {
    let state: State = State {
        node_id: Arc::new(node_id.to_string()),
        session_id: Arc::new(session_id.to_string()),
        peers: Arc::new(RwLock::new(HashMap::new())),
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

    let join_info = Join {
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
        Command::Offer(offer) => handle_offer(offer, ws, state).await,
        Command::Answer(answer) => handle_answer(answer, state).await,
        _ => {
            console_log!("COMMAND: {:#?}", command);
            Ok(())
        }
    }
}

async fn handle_session_update(session: Session, ws: WebSocket, state: State) -> Result<(), Error> {
    let mut introductions = session
        .online
        .iter()
        .filter(|&p| {
            let peers = state.peers.read().unwrap();
            p > &*state.node_id && !peers.contains_key(p)
        })
        .map(|p| {
            let peer = p.clone();
            async { introduce(peer, ws.clone(), state.clone()).await }
        })
        .collect::<FuturesUnordered<_>>();
    while let Some(_) = introductions.next().await {
    }
    Ok(())
}

async fn introduce(peer_id: String, ws: WebSocket, state: State) -> Result<(), Error> {
    let pc = RtcPeerConnection::new()?;
    let offer = JsFuture::from(pc.create_offer()).await?;
    let sdp_data = Reflect::get(&offer, &JsValue::from_str("sdp"))?
        .as_string()
        .unwrap();
    let mut desc = RtcSessionDescriptionInit::new(RtcSdpType::Offer);
    desc.sdp(&sdp_data);
    JsFuture::from(pc.set_local_description(&desc)).await?;

    {
        let mut peers = state.peers.write().unwrap();
        peers.insert(peer_id.clone(), pc);
    }

    let cmd = Command::Offer(Offer {
        session_id: state.session_id.to_string(),
        node_id: state.node_id.to_string(),
        target_id: peer_id,
        sdp_data,
    });
    let data = bincode::serialize(&cmd)?;
    ws.send_with_u8_array(&data)?;

    Ok(())
}

async fn handle_offer(offer: Offer, ws: WebSocket, state: State) -> Result<(), Error> {
    console_log!("GOT OFFER FROM {}", offer.node_id);
    let pc = RtcPeerConnection::new()?;
    let mut offer_desc = RtcSessionDescriptionInit::new(RtcSdpType::Offer);
    offer_desc.sdp(&offer.sdp_data);
    JsFuture::from(pc.set_remote_description(&offer_desc)).await?;
    let answer = JsFuture::from(pc.create_answer()).await?;
    let answer_sdp = Reflect::get(&answer, &JsValue::from_str("sdp"))?
        .as_string()
        .unwrap();
    let mut answer_desc = RtcSessionDescriptionInit::new(RtcSdpType::Answer);
    answer_desc.sdp(&answer_sdp);
    JsFuture::from(pc.set_local_description(&answer_desc)).await?;

    let cmd = Command::Answer(Offer {
        session_id: state.session_id.to_string(),
        node_id: state.node_id.to_string(),
        target_id: offer.node_id,
        sdp_data: answer_sdp,
    });
    let data = bincode::serialize(&cmd)?;
    ws.send_with_u8_array(&data)?;

    Ok(())
}

async fn handle_answer(answer: Offer, state: State) -> Result<(), Error> {
    console_log!("GOT ANSWER FROM {}", answer.node_id);
    let peers = state.peers.read().unwrap();
    let pc = peers
        .get(&answer.node_id)
        .ok_or_else(|| Error::StringError(format!("No connection found for {}", answer.node_id)))?;
    let mut desc = RtcSessionDescriptionInit::new(RtcSdpType::Answer);
    desc.sdp(&answer.sdp_data);
    JsFuture::from(pc.set_remote_description(&desc)).await?;

    Ok(())
}
