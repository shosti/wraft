use crate::console_log;
use crate::webrtc_rpc::transport::PeerTransport;
use crate::webrtc_rpc::introduce;
use futures::channel::mpsc::channel;
use futures::prelude::*;
use serde::{Deserialize, Serialize};
use wasm_bindgen_futures::spawn_local;

pub struct Raft {
    node_id: String,
    session_key: String,
}

#[derive(Serialize, Deserialize, Debug)]
enum RPCRequest {}

#[derive(Serialize, Deserialize, Debug)]
enum RPCResponse {}

impl Raft {
    pub fn new(node_id: String, session_key: String) -> Self {
        Raft {
            node_id,
            session_key,
        }
    }

    pub async fn run(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        console_log!("RUNNING!");
        let (peers_tx, peers_rx) = channel(10);

        let node_id = self.node_id.clone();
        let session_key = self.session_key.clone();
        spawn_local(async move {
            introduce::<RPCRequest, RPCResponse>(
                &node_id,
                &session_key,
                peers_tx,
            )
            .await
            .unwrap();
        });

        peers_rx.for_each_concurrent(10, handle_peer).await;

        future::pending::<()>().await;
        unreachable!()
    }
}

async fn handle_peer(peer: PeerTransport<RPCRequest, RPCResponse>) {
    println!("PEER: {:#?}", peer);

    future::pending::<()>().await;
}
