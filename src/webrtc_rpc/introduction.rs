use crate::console_log;
use crate::webrtc_rpc::client::{Client, Peer};
use crate::webrtc_rpc::error::Error;
use futures::channel::mpsc::{channel, Sender};
use futures::select;
use futures::sink::SinkExt;
use futures::stream::{FuturesUnordered, StreamExt};
use js_sys::Reflect;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::{spawn_local, JsFuture};
use web_sys::{
    MessageEvent, RtcDataChannelEvent, RtcDataChannelState, RtcIceCandidateInit, RtcPeerConnection,
    RtcPeerConnectionIceEvent, RtcSdpType, RtcSessionDescriptionInit, WebSocket,
};
use yenta_types::{Command, IceCandidate, Join, Offer, Session};

static INTRODUCER: &str = "ws://localhost:9999";
static ACK: &str = "ACK";

#[derive(Clone)]
struct State {
    node_id: Arc<String>,
    session_id: Arc<String>,
    peers: Arc<RwLock<HashMap<String, RtcPeerConnection>>>,
    peer_tx: Sender<Peer>,
    _callbacks: Arc<Mutex<Callbacks>>,
}

#[derive(Default)]
struct Callbacks {
    ice: Option<Closure<dyn FnMut(RtcPeerConnectionIceEvent)>>,
    data_channel: Option<Closure<dyn FnMut(RtcDataChannelEvent)>>,
}

pub async fn initiate(node_id: &str, session_id: &str, size: usize) -> Result<Client, Error> {
    let (peer_tx, mut peer_rx) = channel::<Peer>(10);
    let state: State = State {
        node_id: Arc::new(node_id.to_string()),
        session_id: Arc::new(session_id.to_string()),
        peers: Arc::new(RwLock::new(HashMap::new())),
        peer_tx,
        _callbacks: Arc::new(Mutex::new(Callbacks::default())),
    };
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

    let cmd = Command::Join(Join {
        node_id: node_id.to_string(),
        session_id: session_id.to_string(),
    });
    send_command(ws, &cmd)?;

    let mut peers = HashMap::new();
    while peers.len() < (size - 1) {
        select! {
            p = peer_rx.next() => {
                let peer = p.unwrap();
                peers.insert(peer.node_id.clone(), peer);
            }
            err = errors_rx.next() => return Err(err.unwrap()),
        }
    }
    console_log!("WE DID IT!!!");
    Ok(Client::new(peers))
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
        Command::IceCandidate(candidate) => handle_ice_candidate(candidate, state).await,
        _ => unreachable!(),
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
    while introductions.next().await.is_some() {}
    Ok(())
}

async fn introduce(peer_id: String, ws: WebSocket, state: State) -> Result<(), Error> {
    let (done_tx, mut done_rx) = channel::<()>(1);
    let pc = new_peer_connection(peer_id.clone(), ws.clone(), state.clone())?;

    let dc = pc.create_data_channel(
        format!("data-{}-{}-{}", state.session_id, state.node_id, peer_id).as_str(),
    );
    let data_cb = Closure::wrap(Box::new(move |ev: MessageEvent| {
        match ev.data().as_string() {
            Some(msg) if msg == ACK => {
                // When we get a message from the peer, we know the data channel is
                // ready!
                let mut tx = done_tx.clone();
                spawn_local(async move {
                    tx.send(()).await.unwrap();
                });
            }
            msg => {
                panic!("Unexpected message on data stream: {:?}", msg)
            }
        }
    }) as Box<dyn FnMut(MessageEvent)>);
    dc.set_onmessage(Some(data_cb.as_ref().unchecked_ref()));

    let offer = JsFuture::from(pc.create_offer()).await?;
    let sdp_data = Reflect::get(&offer, &JsValue::from_str("sdp"))?
        .as_string()
        .unwrap();
    let mut desc = RtcSessionDescriptionInit::new(RtcSdpType::Offer);
    desc.sdp(&sdp_data);
    JsFuture::from(pc.set_local_description(&desc)).await?;

    {
        let mut peers = state.peers.write().unwrap();
        peers.insert(peer_id.clone(), pc.clone());
    }

    let cmd = Command::Offer(Offer {
        session_id: state.session_id.to_string(),
        node_id: state.node_id.to_string(),
        target_id: peer_id.to_string(),
        sdp_data,
    });
    send_command(ws, &cmd)?;

    if !done_rx.next().await.is_some() {
        unreachable!()
    }
    match dc.ready_state() {
        RtcDataChannelState::Open => {
            let mut tx = state.peer_tx.clone();
            dc.set_onmessage(None);
            let peer = Peer::new(peer_id, pc, dc);
            tx.send(peer).await?;
            Ok(())
        }
        state => {
            let msg = format!(
                "Data channel for {} should be open but is {:?}",
                peer_id, state
            );
            Err(Error::String(msg))
        }
    }
}

fn new_peer_connection(
    peer_id: String,
    ws: WebSocket,
    state: State,
) -> Result<RtcPeerConnection, Error> {
    let pc = RtcPeerConnection::new()?;
    let session_id = state.session_id.to_string();
    let node_id = state.node_id.to_string();
    let ice_cb = Closure::wrap(Box::new(move |ev: RtcPeerConnectionIceEvent| {
        if let Some(candidate) = ev.candidate() {
            let cmd = Command::IceCandidate(IceCandidate {
                session_id: session_id.clone(),
                node_id: node_id.clone(),
                target_id: peer_id.to_string(),
                candidate: candidate.candidate(),
                sdp_mid: candidate.sdp_mid(),
            });
            send_command(ws.clone(), &cmd).unwrap();
        }
    }) as Box<dyn FnMut(RtcPeerConnectionIceEvent)>);
    pc.set_onicecandidate(Some(ice_cb.as_ref().unchecked_ref()));
    {
        let mut cbs = state._callbacks.lock().unwrap();
        cbs.ice = Some(ice_cb);
    }

    Ok(pc)
}

fn send_command(ws: WebSocket, command: &Command) -> Result<(), Error> {
    let data = bincode::serialize(command)?;
    ws.send_with_u8_array(&data)?;
    Ok(())
}

async fn handle_offer(offer: Offer, ws: WebSocket, state: State) -> Result<(), Error> {
    let peer_id = offer.node_id;
    let pc = new_peer_connection(peer_id.clone(), ws.clone(), state.clone())?;
    {
        let mut peers = state.peers.write().unwrap();
        peers.insert(peer_id.clone(), pc.clone());
    }

    let pc1 = pc.clone();
    let s1 = state.clone();
    let pid1 = peer_id.clone();
    let data_cb = Closure::wrap(Box::new(move |ev: RtcDataChannelEvent| {
        let pc2 = pc1.clone();
        let s2 = s1.clone();
        let pid2 = pid1.clone();

        let dc = ev.channel();
        dc.send_with_str(ACK).unwrap();
        let dc_state = dc.ready_state();
        assert_eq!(dc_state, RtcDataChannelState::Open);
        pc2.set_ondatachannel(None);
        spawn_local(async move {
            let mut tx = s2.clone().peer_tx.clone();
            let peer = Peer::new(pid2.clone(), pc2.clone(), dc);
            tx.send(peer).await.unwrap();
        });
    }) as Box<dyn FnMut(RtcDataChannelEvent)>);
    pc.set_ondatachannel(Some(data_cb.as_ref().unchecked_ref()));
    {
        let mut cbs = state._callbacks.lock().unwrap();
        cbs.data_channel = Some(data_cb);
    }

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
        target_id: peer_id.clone(),
        sdp_data: answer_sdp,
    });
    let data = bincode::serialize(&cmd)?;
    ws.send_with_u8_array(&data)?;

    Ok(())
}

async fn handle_answer(answer: Offer, state: State) -> Result<(), Error> {
    let peers = state.peers.read().unwrap();
    let pc = peers
        .get(&answer.node_id)
        .ok_or_else(|| Error::String(format!("No connection found for {}", &answer.node_id)))?;
    let mut desc = RtcSessionDescriptionInit::new(RtcSdpType::Answer);
    desc.sdp(&answer.sdp_data);
    JsFuture::from(pc.set_remote_description(&desc)).await?;

    Ok(())
}

async fn handle_ice_candidate(candidate: IceCandidate, state: State) -> Result<(), Error> {
    let add_ice_promise;
    {
        let peers = state.peers.read().unwrap();
        let pc = peers.get(&candidate.node_id).ok_or_else(|| {
            Error::String(format!("No connection found for {}", candidate.node_id))
        })?;
        let mut cand = RtcIceCandidateInit::new(&candidate.candidate);
        if let Some(sdp_mid) = candidate.sdp_mid {
            cand.sdp_mid(Some(&sdp_mid));
        } else {
            cand.sdp_mid(None);
        }
        add_ice_promise = pc.add_ice_candidate_with_opt_rtc_ice_candidate_init(Some(&cand));
    }
    JsFuture::from(add_ice_promise).await?;
    Ok(())
}
