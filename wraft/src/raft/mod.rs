pub mod errors;
mod rpc_server;
mod storage;
mod worker;

use self::worker::WorkerBuilder;
use crate::console_log;
use crate::util::sleep;
use crate::webrtc_rpc::introduce;
use crate::webrtc_rpc::transport;
use base64::write::EncoderStringWriter;
use errors::{ClientError, Error};
use futures::channel::mpsc::unbounded;
use futures::channel::mpsc::{channel, Receiver, Sender, UnboundedReceiver};
use futures::channel::oneshot;
use futures::select;
use futures::sink::SinkExt;
use futures::stream::StreamExt;
use rpc_server::RpcServer;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::fmt::Debug;
use std::hash::{Hash, Hasher};
use std::io::Cursor;
use std::sync::{Arc, Mutex};
use std::time::Duration;
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

#[derive(Debug, Clone)]
pub struct Raft<Cmd> {
    client_tx: Sender<ClientMessage<Cmd>>,
    subscribers: Arc<Mutex<HashMap<u64, Sender<Cmd>>>>,
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

const CLIENT_REQUEST_TIMEOUT_MILLIS: u64 = 2000;

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
    Cmd: Serialize + DeserializeOwned + Clone + Debug + Send + 'static,
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
        let subscribers = Arc::new(Mutex::new(HashMap::new()));

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

        spawn_local(Self::handle_subscriptions(
            state_machine_rx,
            subscribers.clone(),
        ));

        Ok(Self {
            client_tx,
            subscribers,
        })
    }

    pub fn subscribe(&self) -> Receiver<Cmd> {
        let (tx, rx) = channel(100);
        let mut s = self.subscribers.lock().unwrap();
        let i = s.keys().max().map_or(0, |i| *i) + 1;
        s.insert(i, tx);

        rx
    }

    pub async fn send(&self, val: Cmd) -> Result<(), ClientError> {
        let req = ClientRequest::Apply(val);
        self.do_client_request(req).await.map(|_| ())
    }

    pub async fn debug(&self) -> Result<Box<RaftStateDump>, ClientError> {
        let req = ClientRequest::Debug;
        match self.do_client_request(req).await {
            Ok(ClientResponse::Debug(debug)) => Ok(debug),
            Err(err) => Err(err),
            _ => unreachable!(),
        }
    }

    async fn do_client_request(
        &self,
        req: ClientRequest<Cmd>,
    ) -> Result<ClientResponse<Cmd>, ClientError> {
        let (resp_tx, mut resp_rx) = oneshot::channel();
        let mut tx = self.client_tx.clone();
        tx.send((req, resp_tx))
            .await
            .map_err(|_| ClientError::Unavailable)?;
        select! {
            res = resp_rx => {
                match res {
                    Ok(Ok(resp)) => Ok(resp),
                    Ok(Err(err)) => Err(err),
                    Err(_) => Err(ClientError::Unavailable),
                }
            }
            _ = sleep(Duration::from_millis(CLIENT_REQUEST_TIMEOUT_MILLIS)) => {
                Err(ClientError::Timeout)
            }
        }
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

    async fn handle_subscriptions(
        mut state_machine_rx: UnboundedReceiver<Cmd>,
        subscribers: Arc<Mutex<HashMap<u64, Sender<Cmd>>>>,
    ) {
        while let Some(ref cmd) = state_machine_rx.next().await {
            let mut closed = Vec::new();
            {
                let mut s = subscribers.lock().unwrap();
                for (i, tx) in s.iter_mut() {
                    if tx.send(cmd.clone()).await.is_err() {
                        closed.push(*i);
                    };
                }
            }
            {
                let mut s = subscribers.lock().unwrap();
                for i in closed {
                    s.remove(&i);
                }
            }
        }
    }
}
