use crate::console_log;
use crate::webrtc_rpc::client::Peer;
use crate::webrtc_rpc::introduce;
use futures::channel::mpsc::channel;
use futures::future::Ready;
use futures::prelude::*;
use serde::{Deserialize, Serialize};
use tarpc::context::Context;
use tarpc::server::{BaseChannel, Channel};
use tarpc::{ClientMessage, Response};
use wasm_bindgen_futures::spawn_local;

#[tarpc::service]
pub trait RaftRPC {
    async fn ping(p: RpcMessage) -> RpcMessage;
}

pub struct Raft {
    node_id: String,
    session_key: String,
}

#[derive(Clone)]
pub struct RaftRPCServer;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub enum RpcMessage {
    Ping,
    Pong,
}

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
            introduce::<ClientMessage<RaftRPCRequest>, Response<RaftRPCResponse>>(
                &node_id,
                &session_key,
                peers_tx,
            )
            .await
            .unwrap();
        });

        peers_rx.for_each_concurrent(10, serve_raft_rpc).await;

        future::pending::<()>().await;
        unreachable!()
    }
}

async fn serve_raft_rpc(peer: Peer<ClientMessage<RaftRPCRequest>, Response<RaftRPCResponse>>) {
    console_log!("SERVING FOR {}", peer.node_id);
    let ch = peer.channels();
    let mut requests = BaseChannel::with_defaults(ch).requests();
    while let Some(req) = requests.next().await {
        match req {
            Ok(req) => spawn_local(async move {
                req.execute(RaftRPCServer.serve()).await;
            }),
            Err(err) => panic!("Request error: {:#?}", err),
        }
    }

    future::pending::<()>().await;
}

impl RaftRPC for RaftRPCServer {
    type PingFut = Ready<RpcMessage>;

    fn ping(self, cx: Context, req: RpcMessage) -> Self::PingFut {
        console_log!("GOT REQUEST, CONTEXT: {:#?} REQUEST: {:#?}", cx, req);
        future::ready(RpcMessage::Pong)
    }
}
