pub mod client;
pub mod errors;
mod rpc_server;
mod storage;
mod worker;

use self::worker::WorkerBuilder;
use crate::console_log;
use crate::webrtc_rpc::introduce;
use crate::webrtc_rpc::transport;
use base64::write::EncoderStringWriter;
use errors::{ClientError, Error};
use futures::channel::mpsc::unbounded;
use futures::channel::mpsc::{channel, Sender, UnboundedReceiver};
use futures::channel::oneshot;
use futures::stream::StreamExt;
use futures::Stream;
use rpc_server::RpcServer;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::fmt::Debug;
use std::hash::{Hash, Hasher};
use std::io::Cursor;
use wasm_bindgen_futures::spawn_local;

pub type LogIndex = u64;
pub type TermIndex = u64;
pub type NodeId = u64;
pub type RpcMessage<Cmd> = (
    RpcRequest<Cmd>,
    oneshot::Sender<Result<RpcResponse<Cmd>, transport::Error>>,
);
pub type ClientMessage<Cmd> = (
    ClientRequest<Cmd>,
    oneshot::Sender<Result<ClientResponse<Cmd>, ClientError>>,
);

#[derive(Debug)]
pub struct Raft<Cmd> {
    client_tx: Sender<ClientMessage<Cmd>>,
    state_machine_rx: UnboundedReceiver<Cmd>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum RpcRequest<Cmd> {
    AppendEntries(AppendEntriesRequest<Cmd>),
    RequestVote(RequestVoteRequest),
    ForwardClientRequest(ClientRequest<Cmd>),
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum RpcResponse<Cmd> {
    AppendEntries(AppendEntriesResponse),
    RequestVote(RequestVoteResponse),
    ForwardClientRequest(Result<ClientResponse<Cmd>, ClientError>),
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum ClientRequest<Cmd> {
    Apply(Cmd),
    Debug,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum ClientResponse<Cmd> {
    Ack,
    Get(Option<Cmd>),
    GetCurrentState(HashMap<String, Cmd>),
    Debug(Box<RaftStateDump>),
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct AppendEntriesRequest<Cmd> {
    term: TermIndex,
    leader_id: NodeId,
    prev_log_index: LogIndex,
    prev_log_term: TermIndex,
    entries: Vec<LogEntry<Cmd>>,
    leader_commit: LogIndex,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct RequestVoteRequest {
    term: TermIndex,
    candidate: NodeId,
    last_log_index: LogIndex,
    last_log_term: TermIndex,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct RequestVoteResponse {
    term: TermIndex,
    vote_granted: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct AppendEntriesResponse {
    term: TermIndex,
    success: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct LogEntry<Cmd> {
    pub cmd: Cmd,
    pub term: TermIndex,
    pub idx: LogIndex,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct RaftStateDump {
    state: String,
    leader_id: Option<NodeId>,
    node_id: NodeId,
    session_key: u128,
    cluster_size: usize,
    peers: Vec<NodeId>,
    online_peers: Vec<NodeId>,
    voted_for: Option<NodeId>,
    current_term: TermIndex,
    last_log_index: LogIndex,

    commit_index: LogIndex,
    last_applied: LogIndex,
}

impl<Cmd> Raft<Cmd>
where
    Cmd: Command
{
    pub async fn start(
        hostname: &str,
        session_key: u128,
        cluster_size: usize,
    ) -> Result<Self, Error> {
        let (peers_tx, mut peers_rx) = channel(10);
        let node_id = Self::generate_node_id(hostname, session_key);

        spawn_local(introduce(node_id, session_key, peers_tx));

        let (rpc_tx, rpc_rx) = channel(100);
        let rpc_server = RpcServer::new(rpc_tx);

        let mut peer_clients = HashMap::new();
        let peers;
        if let Some(p) = Self::load_peer_configuration(session_key) {
            // We're rejoining a cluster that's already running
            peers = p;
        } else {
            // We've got to wait for cluster to bootstrap
            let mut servers = Vec::new();
            let target_size = cluster_size - 1;
            while servers.len() < target_size {
                let peer = peers_rx.next().await.ok_or(Error::NotEnoughPeers)?;
                console_log!("Got client: {}", peer.node_id());

                let peer_id = peer.node_id();
                let (client, server) = peer.start();
                peer_clients.insert(peer_id, client);
                servers.push(server);
            }

            while let Some(mut server) = servers.pop() {
                let s = rpc_server.clone();
                spawn_local(async move {
                    server.serve(s).await;
                });
            }
            peers = peer_clients.keys().cloned().collect();
            Self::store_peer_configuration(session_key, &peers);
        };

        let (state_machine_tx, state_machine_rx) = unbounded();

        let client_tx = WorkerBuilder {
            node_id,
            session_key,
            peers,
            rpc_rx,
            peers_rx,
            peer_clients,
            rpc_server,
            state_machine_tx,
        }
        .start();

        Ok(Self {
            client_tx,
            state_machine_rx,
        })
    }

    pub fn client(&self) -> client::Client<Cmd> {
        client::Client::new(self.client_tx.clone())
    }

    fn generate_node_id(hostname: &str, session_key: u128) -> NodeId {
        let mut h = DefaultHasher::new();
        hostname.hash(&mut h);
        session_key.hash(&mut h);
        h.finish()
    }

    fn load_peer_configuration(session_key: u128) -> Option<Vec<NodeId>> {
        let key = Self::peer_configuration_key(session_key);
        if let Some(s) = Self::storage().get_item(&key).unwrap() {
            let mut data = Cursor::new(s);
            let buf = base64::read::DecoderReader::new(&mut data, base64::STANDARD);
            let conf = bincode::deserialize_from(buf).unwrap();
            Some(conf)
        } else {
            None
        }
    }

    fn store_peer_configuration(session_key: u128, peers: &[NodeId]) {
        let key = Self::peer_configuration_key(session_key);
        let mut buf = EncoderStringWriter::new(base64::STANDARD);
        bincode::serialize_into(&mut buf, peers).unwrap();
        let data = buf.into_inner();

        Self::storage().set_item(&key, &data).unwrap();
    }

    fn peer_configuration_key(session_key: u128) -> String {
        format!("peer-configuration-{}", session_key)
    }

    fn storage() -> web_sys::Storage {
        let window = web_sys::window().expect("no global window");
        window.local_storage().expect("no local storage").unwrap()
    }
}

impl<Cmd> Stream for Raft<Cmd> {
    type Item = Cmd;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        self.state_machine_rx.poll_next_unpin(cx)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.state_machine_rx.size_hint()
    }
}

pub trait Command: Serialize + DeserializeOwned + Debug + Send + 'static {}
