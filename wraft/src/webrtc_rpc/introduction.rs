use crate::console_log;
use crate::util::sleep;
use crate::webrtc_rpc::error::Error;
use crate::webrtc_rpc::transport::PeerTransport;
use futures::channel::mpsc::{channel, Receiver, Sender};
use futures::channel::oneshot;
use futures::select;
use futures::sink::SinkExt;
use futures::stream::{FuturesUnordered, StreamExt};
use js_sys::Reflect;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};
use std::time::Duration;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::{spawn_local, JsFuture};
use web_sys::{
    Event, MessageEvent, RtcDataChannel, RtcDataChannelEvent, RtcDataChannelState,
    RtcIceCandidateInit, RtcPeerConnection, RtcPeerConnectionIceEvent, RtcSdpType,
    RtcSessionDescriptionInit, WebSocket,
};
use webrtc_introducer_types::{Command, IceCandidate, Join, Offer, Session};

static INTRODUCER: &str = "wss://webrtc-introducer.herokuapp.com";
static ACK: &str = "ACK";

type PeerInfo = (u64, RtcPeerConnection, RtcDataChannel);

#[derive(Clone)]
struct State {
    node_id: u64,
    session_id: u128,
    peers: Arc<RwLock<HashMap<u64, RtcPeerConnection>>>,
    online: Arc<RwLock<HashSet<u64>>>,
    peer_tx: Sender<PeerInfo>,
}

impl State {
    pub fn new(node_id: u64, session_id: u128, peer_tx: Sender<PeerInfo>) -> Self {
        Self {
            node_id,
            session_id,
            peers: Arc::new(RwLock::new(HashMap::new())),
            online: Arc::new(RwLock::new(HashSet::new())),
            peer_tx,
        }
    }

    pub fn insert_peer(&self, peer_id: u64, pc: RtcPeerConnection) {
        let mut peers = self.peers.write().unwrap();
        let mut online = self.online.write().unwrap();
        peers.insert(peer_id, pc);
        online.insert(peer_id);
    }

    pub async fn send_peer(&self, peer_id: u64, dc: RtcDataChannel) -> Result<(), Error> {
        let mut tx = self.peer_tx.clone();
        let pc;
        {
            let mut peers = self.peers.write().unwrap();
            if let Some(p) = peers.remove(&peer_id) {
                pc = p;
            } else {
                panic!("peer {:#} sent twice", peer_id);
            }
        }
        tx.send((peer_id, pc, dc)).await?;

        Ok(())
    }
}

pub async fn initiate(
    node_id: u64,
    session_id: u128,
    mut peers: Sender<PeerTransport>,
) -> Result<(), Error> {
    let (peer_tx, mut peer_rx) = channel::<PeerInfo>(10);
    let state = State::new(node_id, session_id, peer_tx);

    let (ws, mut errors_rx) = init_ws(&state).await?;
    wait_for_ws_opened(ws.clone()).await;
    send_join_command(node_id, session_id, &ws)?;

    loop {
        select! {
            p = peer_rx.next() => {
                let (done_tx, done_rx) = oneshot::channel();
                let (peer_id, dc, pc) = p.unwrap();
                let peer = PeerTransport::new(peer_id, dc, pc, done_tx);

                let online = state.online.clone();
                spawn_local(async move {
                    done_rx.await.expect("peer transport done channel dropped");
                    let mut o = online.write().unwrap();
                    o.remove(&peer_id);
                });
                peers.send(peer).await?;
            }
            err = errors_rx.next() => return Err(err.unwrap()),
        }
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
        Command::IceCandidate(candidate) => handle_ice_candidate(candidate, state).await,
        _ => unreachable!(),
    }
}

async fn handle_session_update(session: Session, ws: WebSocket, state: State) -> Result<(), Error> {
    console_log!("SESSION UPDATE, ONLINE: {:?}", session.online);
    let mut introductions = session
        .online
        .iter()
        .filter(|&p| {
            let online = state.online.read().unwrap();
            p > &state.node_id && !online.contains(p)
        })
        .map(|peer| async { introduce(*peer, ws.clone(), state.clone()).await })
        .collect::<FuturesUnordered<_>>();
    while introductions.next().await.is_some() {}
    Ok(())
}

async fn introduce(peer_id: u64, ws: WebSocket, state: State) -> Result<(), Error> {
    let pc = new_peer_connection(peer_id, ws.clone(), &state)?;
    let dc = pc.create_data_channel(
        format!(
            "data-{:#x}-{:#x}-{:#x}",
            state.session_id, state.node_id, peer_id
        )
        .as_str(),
    );
    let sdp_data = local_description(&pc).await?;
    state.insert_peer(peer_id, pc);

    let (done_tx, mut done_rx) = futures::channel::oneshot::channel::<()>();
    let dc_clone = dc.clone();
    spawn_local(async move {
        wait_for_dc_initiated(dc_clone).await;
        done_tx.send(()).unwrap();
    });

    loop {
        send_offer(peer_id, &sdp_data, &ws, &state)?;
        let mut retry = sleep(Duration::from_secs(5));
        select! {
            res = done_rx => {
                res.unwrap();
                break;
            }
            _ = retry => {
                console_log!("retrying offer to {}", peer_id);
            }
        }
    }
    assert_eq!(dc.ready_state(), RtcDataChannelState::Open);

    state.send_peer(peer_id, dc).await?;

    Ok(())
}

fn new_peer_connection(
    peer_id: u64,
    ws: WebSocket,
    state: &State,
) -> Result<RtcPeerConnection, Error> {
    let pc = RtcPeerConnection::new()?;
    let session_id = state.session_id;
    let node_id = state.node_id;
    let ice_cb = Closure::wrap(Box::new(move |ev: RtcPeerConnectionIceEvent| {
        if let Some(candidate) = ev.candidate() {
            let cmd = Command::IceCandidate(IceCandidate {
                session_id,
                node_id,
                target_id: peer_id,
                candidate: candidate.candidate(),
                sdp_mid: candidate.sdp_mid(),
            });
            send_command(ws.clone().as_ref(), &cmd).unwrap();
        }
    }) as Box<dyn FnMut(RtcPeerConnectionIceEvent)>);
    pc.set_onicecandidate(Some(ice_cb.as_ref().unchecked_ref()));
    ice_cb.forget();

    Ok(pc)
}

fn send_command(ws: &WebSocket, command: &Command) -> Result<(), Error> {
    let data = bincode::serialize(command)?;
    ws.send_with_u8_array(&data)?;
    Ok(())
}

async fn handle_offer(offer: Offer, ws: WebSocket, state: State) -> Result<(), Error> {
    let peer_id = offer.node_id;
    let pc = new_peer_connection(peer_id, ws.clone(), &state)?;
    state.insert_peer(peer_id, pc.clone());

    let (dc_tx, dc_rx) = futures::channel::oneshot::channel::<RtcDataChannel>();

    let pc_clone = pc.clone();
    spawn_local(async move {
        let dc = wait_for_data_channel(pc_clone).await;
        dc_tx.send(dc).unwrap();
    });

    let answer_sdp = remote_description(&offer.sdp_data, &pc).await?;
    send_answer(peer_id, &answer_sdp, &ws, &state)?;

    let dc = dc_rx.await.unwrap();
    state.send_peer(peer_id, dc).await?;

    Ok(())
}

async fn handle_answer(answer: Offer, state: State) -> Result<(), Error> {
    let pc;
    {
        let peers = state.peers.read().unwrap();
        let pc_opt = peers.get(&answer.node_id);
        if let Some(p) = pc_opt {
            pc = p.clone();
        } else {
            return Err(Error::AlreadyInitialized(answer.node_id));
        }
    }
    let mut desc = RtcSessionDescriptionInit::new(RtcSdpType::Answer);
    desc.sdp(&answer.sdp_data);
    JsFuture::from(pc.set_remote_description(&desc)).await?;

    Ok(())
}

async fn handle_ice_candidate(candidate: IceCandidate, state: State) -> Result<(), Error> {
    let add_ice_promise;
    {
        let peers = state.peers.read().unwrap();
        let pc_opt = peers.get(&candidate.node_id);
        if let Some(pc) = pc_opt {
            let mut cand = RtcIceCandidateInit::new(&candidate.candidate);
            if let Some(sdp_mid) = candidate.sdp_mid {
                cand.sdp_mid(Some(&sdp_mid));
            } else {
                cand.sdp_mid(None);
            }
            add_ice_promise = pc.add_ice_candidate_with_opt_rtc_ice_candidate_init(Some(&cand));
        } else {
            // Can't really be an error because it happens "normally" sometimes
            console_log!(
                "got ICE candidate for {} but it's already initialized",
                candidate.node_id
            );
            return Ok(());
        }
    }
    JsFuture::from(add_ice_promise).await?;
    Ok(())
}

async fn wait_for_ws_opened(ws: WebSocket) {
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
        None => unreachable!(),
    }
    ws.set_onopen(None);
}

async fn init_ws(state: &State) -> Result<(WebSocket, Receiver<Error>), Error> {
    let ws = WebSocket::new(INTRODUCER)?;
    ws.set_binary_type(web_sys::BinaryType::Arraybuffer);

    let (errors_tx, errors_rx) = channel::<Error>(10);

    let w0 = ws.clone();
    let message_state = state.clone();
    let message_cb = Closure::wrap(Box::new(move |e: MessageEvent| {
        let mut errors = errors_tx.clone();
        let s = message_state.clone();
        let w1 = w0.clone();
        spawn_local(async move {
            match handle_message(e, w1.clone(), s.clone()).await {
                Ok(()) => (),
                Err(err) => {
                    errors.send(err).await.unwrap();
                }
            }
        });
    }) as Box<dyn FnMut(MessageEvent)>);

    ws.set_onmessage(Some(message_cb.as_ref().unchecked_ref()));
    message_cb.forget();

    let onerror_cb = Closure::wrap(Box::new(move |ev: Event| {
        console_log!("WEBSOCKET ERROR: {:?}", ev);
    }) as Box<dyn FnMut(Event)>);
    ws.set_onerror(Some(onerror_cb.as_ref().unchecked_ref()));
    onerror_cb.forget();

    Ok((ws, errors_rx))
}

async fn wait_for_dc_initiated(dc: RtcDataChannel) {
    let (done_tx, mut done_rx) = channel::<()>(1);

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
            msg => panic!("Unexpected message on data stream: {:#?}", msg),
        }
    }) as Box<dyn FnMut(MessageEvent)>);
    dc.set_onmessage(Some(data_cb.as_ref().unchecked_ref()));

    if done_rx.next().await.is_none() {
        unreachable!();
    }
    dc.set_onmessage(None);
}

async fn local_description(pc: &RtcPeerConnection) -> Result<String, Error> {
    let offer = JsFuture::from(pc.create_offer()).await?;
    let sdp_data = Reflect::get(&offer, &JsValue::from_str("sdp"))?
        .as_string()
        .unwrap();
    let mut desc = RtcSessionDescriptionInit::new(RtcSdpType::Offer);
    desc.sdp(&sdp_data);
    JsFuture::from(pc.set_local_description(&desc)).await?;

    Ok(sdp_data)
}

async fn remote_description(sdp_data: &str, pc: &RtcPeerConnection) -> Result<String, Error> {
    let mut offer_desc = RtcSessionDescriptionInit::new(RtcSdpType::Offer);
    offer_desc.sdp(sdp_data);
    JsFuture::from(pc.set_remote_description(&offer_desc)).await?;
    let answer = JsFuture::from(pc.create_answer()).await?;
    let answer_sdp = Reflect::get(&answer, &JsValue::from_str("sdp"))?
        .as_string()
        .unwrap();
    let mut answer_desc = RtcSessionDescriptionInit::new(RtcSdpType::Answer);
    answer_desc.sdp(&answer_sdp);
    JsFuture::from(pc.set_local_description(&answer_desc)).await?;

    Ok(answer_sdp)
}

fn send_offer(
    peer_id: u64,
    sdp_data: &str,
    ws: &WebSocket,
    state: &State,
) -> Result<(), Error> {
    let cmd = Command::Offer(Offer {
        session_id: state.session_id,
        node_id: state.node_id,
        target_id: peer_id,
        sdp_data: sdp_data.to_string(),
    });
    send_command(ws, &cmd)
}

fn send_answer(
    peer_id: u64,
    answer_sdp: &str,
    ws: &WebSocket,
    state: &State,
) -> Result<(), Error> {
    let cmd = Command::Answer(Offer {
        session_id: state.session_id,
        node_id: state.node_id,
        target_id: peer_id,
        sdp_data: answer_sdp.to_string(),
    });
    send_command(ws, &cmd)
}

fn send_join_command(node_id: u64, session_id: u128, ws: &WebSocket) -> Result<(), Error> {
    let cmd = Command::Join(Join {
        node_id,
        session_id,
    });
    send_command(ws, &cmd)
}

async fn wait_for_data_channel(pc: RtcPeerConnection) -> RtcDataChannel {
    let (done_tx, mut done_rx) = channel::<RtcDataChannel>(1);

    let data_cb = Closure::wrap(Box::new(move |ev: RtcDataChannelEvent| {
        let dc = ev.channel();
        dc.send_with_str(ACK).unwrap();

        assert_eq!(dc.ready_state(), RtcDataChannelState::Open);

        let mut tx = done_tx.clone();
        spawn_local(async move {
            tx.send(dc).await.unwrap();
        });
    }) as Box<dyn FnMut(RtcDataChannelEvent)>);
    pc.set_ondatachannel(Some(data_cb.as_ref().unchecked_ref()));

    if let Some(dc) = done_rx.next().await {
        pc.set_ondatachannel(None);
        dc
    } else {
        panic!("No channel received")
    }
}
