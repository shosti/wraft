pub mod errors;
mod persistence;

use crate::console_log;
use crate::util::sleep;
use crate::webrtc_rpc::introduce;
use crate::webrtc_rpc::transport::{Client, RequestContext, RequestHandler};
use async_trait::async_trait;
use errors::Error;
use futures::channel::mpsc::channel;
use futures::prelude::*;
use persistence::{LogCmd, LogEntry, PersistentState};
use rand::distributions::Alphanumeric;
use rand::{thread_rng, Rng};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt::{Debug, Formatter};
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, RwLock};
use std::time::Duration;
use wasm_bindgen_futures::spawn_local;

#[derive(Clone)]
pub struct Raft {
    state: Arc<RaftState>,
}

#[derive(Clone, PartialEq, Debug)]
enum RaftStatus {
    Follower,
}

#[derive(Debug)]
struct RaftState {
    status: RwLock<RaftStatus>,
    node_id: String,
    session_key: String,
    cluster_size: usize,
    peer_clients: HashMap<String, Client<RPCRequest, RPCResponse>>,

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
                status: RwLock::new(RaftStatus::Follower),
                persistent,
                node_id,
                session_key,
                cluster_size,
                peer_clients,
                commit_index: 0.into(),
                last_applied: 0.into(),
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

    async fn run(&self) {
        let window = web_sys::window().expect("should have a window in this context");
        let performance = window
            .performance()
            .expect("performance should be available");
        let mut i = 0;
        loop {
            let s: String = thread_rng()
                .sample_iter(&Alphanumeric)
                .take(5)
                .map(char::from)
                .collect();
            let entry = LogEntry {
                cmd: LogCmd::Set {
                    key: format!("foo-{}", i),
                    data: s.into(),
                },
                term: 1,
            };
            console_log!("Appending entry {:?}", entry);
            let t0 = performance.now();
            self.state.persistent.append_log(entry).unwrap();
            let t1 = performance.now();
            console_log!("Took {:.8} millis", t1-t0);

            sleep(Duration::from_secs(1)).await;

            console_log!("Getting entry...");
            let t2 = performance.now();
            let e = self.state.persistent.get_log(i).unwrap();
            let t3 = performance.now();
            console_log!("Got: {:?} (took {:.8} millis)", e, t3-t2);

            sleep(Duration::from_secs(1)).await;

            i += 1;
        }
    }
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
