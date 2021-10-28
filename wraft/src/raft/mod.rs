use crate::console_log;
use crate::util::sleep;
use crate::webrtc_rpc::introduce;
use crate::webrtc_rpc::transport::{Client, RequestContext, RequestHandler, Error};
use async_trait::async_trait;
use futures::channel::mpsc::channel;
use futures::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
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
    peer_clients: Mutex<HashMap<String, Client<RPCRequest, RPCResponse>>>,
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
                peer_clients: Mutex::new(HashMap::new()),
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
                let mut clients = self.state.peer_clients.lock().unwrap();
                clients.insert(peer_id, peer.client());
            };
            let r = self.clone();
            spawn_local(async move {
                peer.serve(r).await;
            });
            let clients = self.state.peer_clients.lock().unwrap();
            if self.status_is(RaftStatus::Waiting) && clients.len() >= (self.state.cluster_size - 1)
            {
                console_log!("FIRING UP!!!");
                self.update_status(RaftStatus::Running);
                let r = self.clone();
                spawn_local(async move {
                    r.start().await;
                })
            }
        }

        unreachable!()
    }

    fn status_is(&self, status: RaftStatus) -> bool {
        let s = self.state.status.read().unwrap();
        *s == status
    }

    fn update_status(&self, status: RaftStatus) {
        let mut s = self.state.status.write().unwrap();
        *s = status;
    }

    async fn start(&self) {
        loop {
            let mut clients = Vec::<Client<RPCRequest, RPCResponse>>::new();
            {
                let cs = self.state.peer_clients.lock().unwrap();
                for c in cs.values() {
                    clients.push(c.clone());
                }
            }
            for client in clients.iter_mut() {
                console_log!("Sending ping to {}", client.node_id());
                match client.call(RPCRequest::Ping).await {
                    Ok(_) => (),
                    Err(Error::Disconnected) => {
                        let node_id = client.node_id();
                        console_log!("Removing client for {}", node_id);
                        let mut clients = self.state.peer_clients.lock().unwrap();
                        clients.remove(&node_id);
                    }
                    Err(err) => {
                        console_log!("Got error: {}", err);
                    }
                }
                console_log!("Sent ping!");
                sleep(Duration::from_secs(3)).await;
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
