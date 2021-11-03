pub mod client;
pub mod errors;
mod persistence;
mod rpc_server;
mod worker;

use crate::console_log;
use crate::webrtc_rpc::introduce;
use crate::webrtc_rpc::transport;
use errors::Error;
use futures::channel::mpsc::{channel, Sender};
use futures::channel::oneshot;
use futures::stream::StreamExt;
use rpc_server::RpcServer;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
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
    oneshot::Sender<Result<ClientResponse, client::Error>>,
);

#[derive(Debug)]
pub struct Raft {
    client_tx: Sender<ClientMessage>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum RpcRequest {
    AppendEntries(AppendEntriesRequest),
    RequestVote(RequestVoteRequest),
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum RpcResponse {
    AppendEntries(AppendEntriesResponse),
    RequestVote(RequestVoteResponse),
}

#[derive(Debug)]
pub enum ClientRequest {
    TODO,
}

#[derive(Debug)]
pub enum ClientResponse {
    Ack,
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
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum LogCmd {
    Set { key: String, data: Vec<u8> },
    Delete { key: String },
}

impl Raft {
    pub async fn initiate(
        node_id: NodeId,
        session_key: String,
        cluster_size: usize,
    ) -> Result<Self, Error> {
        let (peers_tx, mut peers_rx) = channel(10);
        let mut peers = Vec::new();
        let mut peer_clients = HashMap::new();

        spawn_local(introduce::<RpcRequest, RpcResponse>(
            node_id.clone(),
            session_key.clone(),
            peers_tx,
        ));

        let target_size = cluster_size - 1;
        while peers.len() < target_size {
            let peer = peers_rx.next().await.ok_or(Error::NotEnoughPeers)?;
            console_log!("Got client: {}", peer.node_id());

            let peer_id = peer.node_id().clone();
            peer_clients.insert(peer_id, peer.client());
            peers.push(peer);
        }

        let (rpc_tx, rpc_rx) = channel(100);
        let (client_tx, client_rx) = channel(100);
        let rpc_server = RpcServer::new(rpc_tx);

        while let Some(mut peer) = peers.pop() {
            let s = rpc_server.clone();
            spawn_local(async move {
                peer.serve(s).await;
            });
        }
        spawn_local(worker::run(
            node_id.clone(),
            session_key.clone(),
            rpc_rx,
            client_rx,
            peer_clients,
        ));

        Ok(Self { client_tx })
    }

    // pub async fn set(&self, key: String, data: Vec<u8>) -> Result<(), Error> {
    //     let (resp_tx, mut resp_rx) = oneshot::channel();
    //     let cmd = LogCmd::Set { key, data };
    //     self.state
    //         .cmds_tx
    //         .clone()
    //         .send((cmd, resp_tx))
    //         .await
    //         .unwrap();

    //     select! {
    //         res = resp_rx => res.expect("command channel closed"),
    //         _ = sleep(Duration::from_millis(COMMAND_TIMEOUT_MILLIS)) => Err(Error::CommandTimeout),
    //     }
    // }

    // fn votes_required(&self) -> usize {
    //     (self.state.cluster_size / 2) + 1
    // }

    // fn update_term(&self, term: TermIndex) {
    //     let current_term = self.state.persistent.current_term();
    //     if current_term < term {
    //         self.state.persistent.set_current_term(term);
    //         match self.get_status() {
    //             RaftStatus::Follower { .. } => (),
    //             _ => {
    //                 self.set_status(RaftStatus::Follower { leader_id: None });
    //             }
    //         }
    //     }
    // }

    // async fn handle_request_vote(&self, req: RequestVoteRequest) -> RequestVoteResponse {
    //     self.update_term(req.term);

    //     let persistent = &self.state.persistent;
    //     let voted_for = persistent.voted_for();
    //     let current_term = persistent.current_term();
    //     let commit_index = self.state.commit_index.load(Ordering::SeqCst);
    //     if req.term >= current_term
    //         && (voted_for == None || *voted_for.unwrap() == req.candidate)
    //         && req.last_log_index >= commit_index
    //     {
    //         persistent.set_voted_for(Some(&req.candidate));

    //         return RequestVoteResponse {
    //             term: req.last_log_term,
    //             vote_granted: true,
    //         };
    //     }

    //     RequestVoteResponse {
    //         term: 0,
    //         vote_granted: false,
    //     }
    // }

    // async fn send_heartbeat(&self, leader_id: &str) {
    //     let hb_tx = self.state.heartbeat_tx.lock().unwrap().clone();
    //     match hb_tx {
    //         None => (),
    //         Some(mut tx) => {
    //             // If the heartbeat fails it's probably because we changed state
    //             // or something, so ignore errors.
    //             let _ = tx.send(leader_id.to_string()).await;
    //         }
    //     }
    // }

    // async fn handle_append_entries(&self, req: AppendEntriesRequest) -> AppendEntriesResponse {
    //     self.update_term(req.term);
    //     self.send_heartbeat(&req.leader_id).await;

    //     let term = self.state.persistent.current_term();

    //     AppendEntriesResponse {
    //         term,
    //         success: true,
    //     }
    // }
}
