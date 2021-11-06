pub mod errors;
mod storage;
mod rpc_server;
mod worker;

use crate::console_log;
use crate::util::sleep;
use crate::webrtc_rpc::introduce;
use crate::webrtc_rpc::transport;
use errors::{ClientError, Error};
use futures::channel::mpsc::{channel, Receiver, Sender};
use futures::channel::oneshot;
use futures::select;
use futures::sink::SinkExt;
use futures::stream::StreamExt;
use rpc_server::RpcServer;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use wasm_bindgen_futures::spawn_local;

pub type LogIndex = u64;
pub type TermIndex = u64;
pub type NodeId = String;
pub type RpcMessage = (
    RpcRequest,
    oneshot::Sender<Result<RpcResponse, transport::Error>>,
);
pub type ClientMessage = (
    ClientRequest,
    oneshot::Sender<Result<ClientResponse, ClientError>>,
);

#[derive(Debug, Clone)]
pub struct Raft {
    client_tx: Sender<ClientMessage>,
    debug_rx: Arc<Mutex<Option<Receiver<worker::RaftDebugState>>>>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum RpcRequest {
    AppendEntries(AppendEntriesRequest),
    RequestVote(RequestVoteRequest),
    ForwardClientRequest(ClientRequest),
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum RpcResponse {
    AppendEntries(AppendEntriesResponse),
    RequestVote(RequestVoteResponse),
    ForwardClientRequest(Result<ClientResponse, ClientError>),
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum ClientRequest {
    Get(String),
    Set(String, String),
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum ClientResponse {
    Ack,
    Get(Option<String>),
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct AppendEntriesRequest {
    term: TermIndex,
    leader_id: NodeId,
    prev_log_index: LogIndex,
    prev_log_term: TermIndex,
    entries: Vec<LogEntry>,
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
pub struct LogEntry {
    pub cmd: LogCmd,
    pub term: TermIndex,
    pub idx: LogIndex,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum LogCmd {
    Set { key: String, data: String },
    Delete { key: String },
}

const CLIENT_REQUEST_TIMEOUT_MILLIS: u64 = 2000;

impl Raft {
    pub async fn initiate(
        node_id: NodeId,
        session_key: String,
        cluster_size: usize,
    ) -> Result<Self, Error> {
        let (peers_tx, mut peers_rx) = channel(10);
        let mut peers = Vec::new();
        let mut peer_clients = HashMap::new();

        spawn_local(introduce(node_id.clone(), session_key.clone(), peers_tx));

        let target_size = cluster_size - 1;
        while peers.len() < target_size {
            let peer = peers_rx.next().await.ok_or(Error::NotEnoughPeers)?;
            console_log!("Got client: {}", peer.node_id());

            let peer_id = peer.node_id().clone();
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
        let (client_tx, debug_rx) = worker::run(
            node_id.clone(),
            session_key.clone(),
            rpc_rx,
            peers_rx,
            peer_clients,
            rpc_server,
        );

        Ok(Self {
            client_tx,
            debug_rx: Arc::new(Mutex::new(Some(debug_rx))),
        })
    }

    pub fn get_debug_channel(&self) -> Option<Receiver<worker::RaftDebugState>> {
        let mut rx = self.debug_rx.lock().unwrap();
        rx.take()
    }

    pub async fn get(&self, key: String) -> Result<Option<String>, ClientError> {
        let (resp_tx, mut resp_rx) = oneshot::channel();
        let msg = ClientRequest::Get(key);
        let mut tx = self.client_tx.clone();
        tx.send((msg, resp_tx))
            .await
            .map_err(|_| ClientError::Unavailable)?;
        select! {
            res = resp_rx => {
                match res {
                    Ok(Ok(ClientResponse::Get(val))) => Ok(val),
                    Ok(Err(err)) => Err(err),
                    Err(_) => Err(ClientError::Unavailable),
                    _ => unreachable!(),
                }
            }
            _ = sleep(Duration::from_millis(CLIENT_REQUEST_TIMEOUT_MILLIS)) => {
                Err(ClientError::Timeout)
            }
        }
    }

    pub async fn set(&self, key: String, val: String) -> Result<(), ClientError> {
        let (resp_tx, mut resp_rx) = oneshot::channel();
        let msg = ClientRequest::Set(key, val);
        let mut tx = self.client_tx.clone();
        tx.send((msg, resp_tx))
            .await
            .map_err(|_| ClientError::Unavailable)?;
        select! {
            res = resp_rx => {
                match res {
                    Ok(Ok(ClientResponse::Ack)) => Ok(()),
                    Ok(Err(err)) => Err(err),
                    Err(_) => Err(ClientError::Unavailable),
                    _ => unreachable!(),
                }
            }
            _ = sleep(Duration::from_millis(CLIENT_REQUEST_TIMEOUT_MILLIS)) => {
                Err(ClientError::Timeout)
            }
        }
    }
}
