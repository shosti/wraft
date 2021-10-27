use crate::console_log;
use crate::util::sleep;
use crate::webrtc_rpc::introduce;
use crate::webrtc_rpc::transport::{Client, RequestContext, RequestHandler};
use async_trait::async_trait;
use futures::channel::mpsc::channel;
use futures::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Duration;
use wasm_bindgen_futures::spawn_local;

#[derive(Clone)]
pub struct Raft {
    state: Arc<RaftState>,
}

#[derive(Clone, PartialEq)]
enum RaftStatus {
    Waiting,
    Running,
}

struct RaftState {
    status: RwLock<RaftStatus>,
    node_id: String,
    session_key: String,
    cluster_size: usize,
    peer_clients: RwLock<HashMap<String, Client<RPCRequest, RPCResponse>>>,
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
    pub fn new(node_id: String, session_key: String, cluster_size: usize) -> Self {
        Self {
            state: Arc::new(RaftState {
                status: RwLock::new(RaftStatus::Waiting),
                node_id,
                session_key,
                cluster_size,
                peer_clients: RwLock::new(HashMap::new()),
            }),
        }
    }

    pub async fn run(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        console_log!("RUNNING!");
        let (peers_tx, mut peers_rx) = channel(10);

        let node_id = self.state.node_id.clone();
        let session_key = self.state.session_key.clone();
        spawn_local(async move {
            introduce::<RPCRequest, RPCResponse>(&node_id, &session_key, peers_tx)
                .await
                .unwrap();
        });

        while let Some(mut peer) = peers_rx.next().await {
            console_log!("Got client: {}", peer.node_id());
            {
                let peer_id = peer.node_id();
                let mut clients = self.state.peer_clients.write().unwrap();
                clients.insert(peer_id, peer.client());
            };
            let r = self.clone();
            spawn_local(async move {
                peer.serve(r).await;
            });
            let clients = self.state.peer_clients.read().unwrap();
            let status;
            {
                status = self.state.status.read().unwrap();
            }
            if *status == RaftStatus::Waiting && clients.len() >= (self.state.cluster_size - 1) {
                console_log!("FIRING UP!!!");
                {
                    let mut status = self.state.status.write().unwrap();
                    *status = RaftStatus::Running;
                }
                let r = self.clone();
                spawn_local(async move {
                    r.start().await;
                })
            }
        }

        unreachable!()
    }

    async fn start(&self) {
        loop {
            let mut clients = Vec::<Client<RPCRequest, RPCResponse>>::new();
            {
                let cs = self.state.peer_clients.read().unwrap();
                for c in cs.values() {
                    clients.push(c.clone());
                }
            }
            for client in clients.iter_mut() {
                console_log!("Sending ping to {}", client.node_id);
                client.call(RPCRequest::Ping).await.unwrap();
                console_log!("Sent ping!");
                sleep(Duration::from_secs(3)).await.unwrap();
            }
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
