use crate::console_log;
use crate::webrtc_rpc::introduce;
use futures::future::Ready;
use futures::prelude::*;
use serde::{Deserialize, Serialize};
use tarpc::context::Context;
use tarpc::server;
use tarpc::{ClientMessage, Response};
use wasm_bindgen_futures::spawn_local;

#[tarpc::service]
trait RaftRPC {
    async fn ping(p: RpcMessage) -> RpcMessage;
}

pub struct Raft {
    node_id: String,
    session_key: String,
}

#[derive(Clone)]
struct RaftRPCServer {}

#[derive(Clone, Debug, Deserialize, Serialize)]
enum RpcMessage {
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

    pub async fn run(&mut self) -> anyhow::Result<()> {
        console_log!("RUNNING!");
        let mut rtc_client = introduce::<ClientMessage<RpcMessage>, Response<RpcMessage>>(
            &self.node_id,
            &self.session_key,
        )
        .await
        .unwrap();
        console_log!("WE'RE ALL INTRODUCED!");

        for peer in rtc_client.peers.values_mut() {
            let server = server::BaseChannel::with_defaults(peer);
            println!("SERVER: {:#?}", server);
        }

        future::pending::<()>().await;
        unreachable!()
    }
}

impl RaftRPC for RaftRPCServer {
    type PingFut = Ready<RpcMessage>;

    fn ping(self, cx: Context, req: RpcMessage) -> Self::PingFut {
        console_log!("GOT REQUEST, CONTEXT: {:#?} REQUEST: {:#?}", cx, req);
        future::ready(RpcMessage::Pong)
    }
}
