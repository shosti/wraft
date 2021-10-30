pub mod errors;
mod persistence;

use crate::console_log;
use crate::util::sleep_fused;
use crate::webrtc_rpc::introduce;
use crate::webrtc_rpc::transport::{Client, RequestContext, RequestHandler};
use async_trait::async_trait;
use errors::Error;
use futures::channel::mpsc::{channel, Receiver, Sender};
use futures::prelude::*;
use futures::select;
use persistence::PersistentState;
use rand::{thread_rng, Rng};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt::{Debug, Formatter};
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;
use wasm_bindgen_futures::spawn_local;

#[derive(Clone)]
pub struct Raft {
    state: Arc<RaftState>,
}

#[derive(Clone, PartialEq, Debug)]
enum RaftStatus {
    Follower { leader_id: Option<String> },
}

#[derive(Debug)]
struct RaftState {
    status: RwLock<RaftStatus>,
    node_id: String,
    session_key: String,
    cluster_size: usize,
    peer_clients: HashMap<String, Client<RPCRequest, RPCResponse>>,
    heartbeat_tx: Mutex<Option<Sender<String>>>,

    // Volatile state
    commit_index: AtomicU64,
    last_applied: AtomicU64,

    // Persistent state
    persistent: PersistentState,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
enum RPCRequest {
    Ping,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
enum RPCResponse {
    Pong,
}

impl Raft {
    pub async fn initiate(
        node_id: String,
        session_key: String,
        cluster_size: usize,
    ) -> Result<Self, Error> {
        let (peers_tx, mut peers_rx) = channel(10);
        let mut peers = Vec::new();
        let mut peer_clients = HashMap::new();

        spawn_local(introduce::<RPCRequest, RPCResponse>(
            node_id.clone(),
            session_key.clone(),
            peers_tx,
        ));

        let target_size = cluster_size - 1;
        while peers.len() < target_size {
            let peer = peers_rx.next().await.ok_or(Error::NotEnoughPeers())?;
            console_log!("Got client: {}", peer.node_id());

            let peer_id = peer.node_id().clone();
            peer_clients.insert(peer_id, peer.client());
            peers.push(peer);
        }

        let persistent = PersistentState::new(&session_key);
        console_log!("FIRING UP!!!");
        let raft = Self {
            state: Arc::new(RaftState {
                status: RwLock::new(RaftStatus::Follower { leader_id: None }),
                persistent,
                node_id,
                session_key,
                cluster_size,
                peer_clients,
                commit_index: 0.into(),
                last_applied: 0.into(),
                heartbeat_tx: Mutex::new(None),
            }),
        };

        while let Some(mut peer) = peers.pop() {
            let r = raft.clone();
            spawn_local(async move {
                peer.serve(r).await;
            });
        }
        let r = raft.clone();
        spawn_local(async move {
            r.run().await;
        });

        Ok(raft)
    }

    // fn status_is(&self, status: RaftStatus) -> bool {
    //     let s = self.state.status.read().unwrap();
    //     *s == status
    // }

    // fn update_status(&self, status: RaftStatus) {
    //     let mut s = self.state.status.write().unwrap();
    //     *s = status;
    // }

    fn get_status(&self) -> RaftStatus {
        let s = self.state.status.read().unwrap();
        s.clone()
    }

    async fn run(&self) {
        loop {
            match self.get_status() {
                RaftStatus::Follower { .. } => self.be_follower().await,
            }
        }
    }

    async fn be_follower(&self) {
        console_log!("BEING A FOLLOWER");
        let mut hb_rx = self.get_heartbeat();

        loop {
            select! {
                _ = sleep_fused(election_timeout()) => {
                    println!("Calling election!");
                    return;
                }
                res = hb_rx.next() => {
                    let leader = res.unwrap();
                    console_log!("GOT A HEARTBEAT FROM {}", leader);
                    // self.set_status(RaftStatus::Follower(Some(leader))).await
                }
            }
        }
    }

    fn get_heartbeat(&self) -> Receiver<String> {
        let (tx, rx) = channel(10);
        let mut hb_tx = self.state.heartbeat_tx.lock().unwrap();
        *hb_tx = Some(tx);

        rx
    }
}

fn election_timeout() -> Duration {
    let delay = thread_rng().gen_range(150..300);
    Duration::from_millis(delay)
}

#[async_trait]
impl RequestHandler<RPCRequest, RPCResponse> for Raft {
    async fn handle(&self, req: RPCRequest, cx: RequestContext) -> RPCResponse {
        console_log!("Got {:?} from {}", req, cx.source_node_id);
        RPCResponse::Pong
    }
}

impl Debug for Raft {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result<(), std::fmt::Error> {
        write!(f, "Raft (id: {})", self.state.node_id)?;
        Ok(())
    }
}
