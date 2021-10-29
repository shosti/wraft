pub mod errors;

use crate::console_log;
use crate::util::sleep;
use crate::webrtc_rpc::introduce;
use crate::webrtc_rpc::transport::{Client, RequestContext, RequestHandler};
use async_trait::async_trait;
use errors::Error;
use futures::channel::mpsc::channel;
use futures::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt::{Debug, Formatter};
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
            // TODO: error handling
            let peer = peers_rx.next().await.unwrap();
            console_log!("Got client: {}", peer.node_id());

            let peer_id = peer.node_id().clone();
            peer_clients.insert(peer_id, peer.client());
            peers.push(peer);
        }

        console_log!("FIRING UP!!!");
        let raft = Self {
            state: Arc::new(RaftState {
                status: RwLock::new(RaftStatus::Follower),
                node_id,
                session_key,
                cluster_size,
                peer_clients,
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
        loop {
            for (node_id, client) in self.state.peer_clients.iter() {
                console_log!("Sending ping to {}", node_id);
                match client.call(RPCRequest::Ping).await {
                    Ok(_) => (),
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

impl Debug for Raft {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result<(), std::fmt::Error> {
        write!(f, "Raft (id: {})\n", self.state.node_id)?;
        Ok(())
    }
}
