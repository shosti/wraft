use crate::console_log;
use crate::webrtc_rpc::error::Error;
use crate::webrtc_rpc::transport::PeerTransport;
use futures::channel::mpsc::{channel, Receiver, Sender};
use futures::select;
use futures::sink::SinkExt;
use futures::stream::{FuturesUnordered, StreamExt};
use js_sys::Reflect;
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::collections::HashMap;
use std::fmt::Debug;
use std::sync::{Arc, RwLock};
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::{spawn_local, JsFuture};
use web_sys::{
    MessageEvent, RtcDataChannel, RtcDataChannelEvent, RtcDataChannelState, RtcIceCandidateInit,
    RtcIceConnectionState, RtcPeerConnection, RtcPeerConnectionIceEvent, RtcSdpType,
    RtcSessionDescriptionInit, WebSocket,
};
use webrtc_introducer_types::{Command, IceCandidate, Join, Offer, Session};

static INTRODUCER: &str = "wss://webrtc-introducer.herokuapp.com";
static ACK: &str = "ACK";

type PeerInfo = (String, RtcPeerConnection, RtcDataChannel);

#[derive(Clone)]
struct State {
    node_id: Arc<String>,
    session_id: Arc<String>,
    peers: Arc<RwLock<HashMap<String, RtcPeerConnection>>>,
    peer_tx: Sender<PeerInfo>,
}

impl State {
    pub fn new(node_id: &str, session_id: &str, peer_tx: Sender<PeerInfo>) -> Self {
        Self {
            node_id: Arc::new(node_id.to_string()),
            session_id: Arc::new(session_id.to_string()),
            peers: Arc::new(RwLock::new(HashMap::new())),
            peer_tx,
        }
    }

    pub fn insert_peer(&self, peer_id: &str, pc: RtcPeerConnection) {
        let mut peers = self.peers.write().unwrap();
        peers.insert(peer_id.to_string(), pc);
    }

    pub async fn send_peer(
        &self,
        peer_id: &str,
        pc: RtcPeerConnection,
        dc: RtcDataChannel,
    ) -> Result<(), Error> {
        let mut tx = self.peer_tx.clone();
        tx.send((peer_id.to_string(), pc, dc)).await?;

        Ok(())
    }
}

pub async fn initiate<Req, Resp>(
    node_id: &str,
    session_id: &str,
    mut peers: Sender<PeerTransport<Req, Resp>>,
) -> Result<(), Error>
where
    Req: Serialize + DeserializeOwned + Debug + 'static,
    Resp: Serialize + DeserializeOwned + Debug + 'static,
{
    let (peer_tx, mut peer_rx) = channel::<PeerInfo>(10);
    let state = State::new(node_id, session_id, peer_tx);

    let (ws, mut errors_rx) = init_ws(state).await?;
    wait_for_ws_opened(ws.clone()).await;
    send_join_command(node_id, session_id, ws).await?;

    loop {
        select! {
            p = peer_rx.next() => {
                let (peer_id, dc, pc) = p.unwrap();
                let peer = PeerTransport::new(peer_id.clone(), dc, pc);
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
    let pc = new_peer_connection(peer_id.clone(), ws.clone(), state.clone())?;
    state.insert_peer(&peer_id, pc.clone());

    let dc = pc.create_data_channel(
        format!("data-{}-{}-{}", state.session_id, state.node_id, peer_id).as_str(),
    );

    let (done_tx, done_rx) = futures::channel::oneshot::channel::<()>();
    let dc_clone = dc.clone();
    spawn_local(async move {
        wait_for_dc_initiated(dc_clone).await;
        done_tx.send(()).unwrap();
    });

    let sdp_data = local_description(&pc).await?;
    send_offer(&peer_id, &sdp_data, ws, state.clone()).await?;

    done_rx.await.unwrap();
    assert_eq!(dc.ready_state(), RtcDataChannelState::Open);

    state.send_peer(&peer_id, pc, dc).await?;

    Ok(())
}

fn new_peer_connection(
    peer_id: String,
    ws: WebSocket,
    state: State,
) -> Result<RtcPeerConnection, Error> {
    let pc = RtcPeerConnection::new()?;
    let session_id = state.session_id.to_string();
    let node_id = state.node_id.to_string();
    let pid = peer_id.clone();
    let ice_cb = Closure::wrap(Box::new(move |ev: RtcPeerConnectionIceEvent| {
        if let Some(candidate) = ev.candidate() {
            let cmd = Command::IceCandidate(IceCandidate {
                session_id: session_id.clone(),
                node_id: node_id.clone(),
                target_id: pid.clone(),
                candidate: candidate.candidate(),
                sdp_mid: candidate.sdp_mid(),
            });
            send_command(ws.clone(), &cmd).unwrap();
        }
    }) as Box<dyn FnMut(RtcPeerConnectionIceEvent)>);
    pc.set_onicecandidate(Some(ice_cb.as_ref().unchecked_ref()));
    ice_cb.forget();

    let cs_pc = pc.clone();
    let cs_cb = Closure::wrap(Box::new(move || {
        let ice_state = cs_pc.ice_connection_state();
        if ice_state == RtcIceConnectionState::Failed
            || ice_state == RtcIceConnectionState::Disconnected
            || ice_state == RtcIceConnectionState::Closed
        {
            console_log!("Lost ICE connection for {}", peer_id);
            let mut peers = state.peers.write().unwrap();
            peers.remove(&peer_id);
        }
    }) as Box<dyn FnMut()>);
    pc.set_oniceconnectionstatechange(Some(cs_cb.as_ref().unchecked_ref()));
    cs_cb.forget();

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
    state.insert_peer(&peer_id, pc.clone());

    let (dc_tx, dc_rx) = futures::channel::oneshot::channel::<RtcDataChannel>();

    let pc_clone = pc.clone();
    spawn_local(async move {
        let dc = wait_for_data_channel(pc_clone).await;
        dc_tx.send(dc).unwrap();
    });

    let answer_sdp = remote_description(&offer.sdp_data, &pc).await?;
    send_answer(&peer_id, &answer_sdp, ws, state.clone()).await?;

    let dc = dc_rx.await.unwrap();
    state.send_peer(&peer_id, pc, dc).await?;

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

async fn init_ws(state: State) -> Result<(WebSocket, Receiver<Error>), Error> {
    let ws = WebSocket::new(INTRODUCER)?;
    ws.set_binary_type(web_sys::BinaryType::Arraybuffer);

    let (errors_tx, errors_rx) = channel::<Error>(10);

    let w0 = ws.clone();
    let message_cb = Closure::wrap(Box::new(move |e: MessageEvent| {
        let mut errors = errors_tx.clone();
        let s = state.clone();
        let w1 = w0.clone();
        spawn_local(async move {
            match handle_message(e, w1.clone(), s.clone()).await {
                Ok(()) => (),
                Err(err) => {
                    errors.send(err).await.unwrap();
                }
            }
        })
    }) as Box<dyn FnMut(MessageEvent)>);

    ws.set_onmessage(Some(message_cb.as_ref().unchecked_ref()));
    message_cb.forget();

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
        unreachable!()
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

async fn send_offer(
    peer_id: &str,
    sdp_data: &str,
    ws: WebSocket,
    state: State,
) -> Result<(), Error> {
    let cmd = Command::Offer(Offer {
        session_id: state.session_id.to_string(),
        node_id: state.node_id.to_string(),
        target_id: peer_id.to_string(),
        sdp_data: sdp_data.to_string(),
    });
    send_command(ws, &cmd)
}

async fn send_answer(
    peer_id: &str,
    answer_sdp: &str,
    ws: WebSocket,
    state: State,
) -> Result<(), Error> {
    let cmd = Command::Answer(Offer {
        session_id: state.session_id.to_string(),
        node_id: state.node_id.to_string(),
        target_id: peer_id.to_string(),
        sdp_data: answer_sdp.to_string(),
    });
    send_command(ws, &cmd)
}

async fn send_join_command(node_id: &str, session_id: &str, ws: WebSocket) -> Result<(), Error> {
    let cmd = Command::Join(Join {
        node_id: node_id.to_string(),
        session_id: session_id.to_string(),
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
