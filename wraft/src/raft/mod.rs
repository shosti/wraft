pub mod errors;
mod rpc_server;
mod storage;
mod worker;

use self::worker::RaftDebugState;
use crate::console_log;
use crate::util::sleep;
use crate::webrtc_rpc::introduce;
use crate::webrtc_rpc::transport;
use errors::{ClientError, Error};
use futures::channel::mpsc::{channel, Sender};
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
use std::time::Duration;
use wasm_bindgen_futures::spawn_local;

pub type LogIndex = u64;
pub type TermIndex = u64;
pub type NodeId = u64;
pub type RpcMessage<T> = (
    RpcRequest<T>,
    oneshot::Sender<Result<RpcResponse<T>, transport::Error>>,
);
pub type ClientMessage<T> = (
    ClientRequest<T>,
    oneshot::Sender<Result<ClientResponse<T>, ClientError>>,
);

#[derive(Debug, Clone)]
pub struct Raft<T> {
    client_tx: Sender<ClientMessage<T>>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum RpcRequest<T> {
    AppendEntries(AppendEntriesRequest<T>),
    RequestVote(RequestVoteRequest),
    ForwardClientRequest(ClientRequest<T>),
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum RpcResponse<T> {
    AppendEntries(AppendEntriesResponse),
    RequestVote(RequestVoteResponse),
    ForwardClientRequest(Result<ClientResponse<T>, ClientError>),
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum ClientRequest<T> {
    Get(String),
    Set(String, T),
    Delete(String),
    Debug,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum ClientResponse<T> {
    Ack,
    Get(Option<T>),
    Debug(Box<RaftDebugState<T>>),
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct AppendEntriesRequest<T> {
    term: TermIndex,
    leader_id: NodeId,
    prev_log_index: LogIndex,
    prev_log_term: TermIndex,
    entries: Vec<LogEntry<T>>,
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
pub struct LogEntry<T> {
    pub cmd: LogCmd<T>,
    pub term: TermIndex,
    pub idx: LogIndex,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum LogCmd<T> {
    Set { key: String, data: T },
    Delete { key: String },
}

const CLIENT_REQUEST_TIMEOUT_MILLIS: u64 = 2000;

impl<T> Raft<T>
where
    T: Serialize + DeserializeOwned + Clone + Debug + Send + 'static,
{
    pub async fn initiate(
        hostname: &str,
        session_key: u128,
        cluster_size: usize,
    ) -> Result<Self, Error> {
        let (peers_tx, mut peers_rx) = channel(10);
        let mut peers = Vec::new();
        let mut peer_clients = HashMap::new();
        let node_id = Self::generate_node_id(hostname, session_key);

        spawn_local(introduce(node_id, session_key, peers_tx));

        let target_size = cluster_size - 1;
        while peers.len() < target_size {
            let peer = peers_rx.next().await.ok_or(Error::NotEnoughPeers)?;
            console_log!("Got client: {:016x}", peer.node_id());

            let peer_id = peer.node_id();
            let (client, server) = peer.start();
            peer_clients.insert(peer_id, client);
            peers.push(server);
        }

        let (rpc_tx, rpc_rx) = channel(100);
        let rpc_server = RpcServer::new(rpc_tx);

        while let Some(mut peer) = peers.pop() {
            let s = rpc_server.clone();
            spawn_local(async move {
                peer.serve(s).await;
            });
        }
        let client_tx = worker::run(
            node_id,
            session_key,
            rpc_rx,
            peers_rx,
            peer_clients,
            rpc_server,
        );

        Ok(Self { client_tx })
    }

    pub async fn get(&self, key: String) -> Result<Option<T>, ClientError> {
        let req = ClientRequest::Get(key);
        match self.do_client_request(req).await {
            Ok(ClientResponse::Get(val)) => Ok(val),
            Err(err) => Err(err),
            _ => unreachable!(),
        }
    }

    pub async fn set(&self, key: String, val: T) -> Result<(), ClientError> {
        let req = ClientRequest::Set(key, val);
        self.do_client_request(req).await.map(|_| ())
    }

    pub async fn delete(&self, key: String) -> Result<(), ClientError> {
        let req = ClientRequest::Delete(key);
        self.do_client_request(req).await.map(|_| ())
    }

    pub async fn debug(&self) -> Result<Box<RaftDebugState<T>>, ClientError> {
        let req = ClientRequest::Debug;
        match self.do_client_request(req).await {
            Ok(ClientResponse::Debug(debug)) => Ok(debug),
            Err(err) => Err(err),
            _ => unreachable!(),
        }
    }

    async fn do_client_request(
        &self,
        req: ClientRequest<T>,
    ) -> Result<ClientResponse<T>, ClientError> {
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
}
