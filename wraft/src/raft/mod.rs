pub mod errors;
mod persistence;

use crate::console_log;
use crate::util::sleep;
use crate::webrtc_rpc::introduce;
use crate::webrtc_rpc::transport::{self, Client, RequestHandler};
use async_trait::async_trait;
use errors::Error;
use futures::channel::mpsc::{channel, Receiver, Sender};
use futures::channel::oneshot;
use futures::select;
use futures::sink::SinkExt;
use futures::stream::{FuturesUnordered, StreamExt};
use persistence::PersistentState;
use rand::{thread_rng, Rng};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt::{Debug, Formatter};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;
use wasm_bindgen_futures::spawn_local;

pub type LogIndex = u64;
pub type TermIndex = u64;
pub type NodeId = String;
type CmdReceiver = Receiver<(LogCmd, oneshot::Sender<Result<(), Error>>)>;
type CmdSender = Sender<(LogCmd, oneshot::Sender<Result<(), Error>>)>;

const HEARBEAT_INTERVAL_MILLIS: u64 = 50;
const COMMAND_TIMEOUT_MILLIS: u64 = 500;

#[derive(Clone)]
pub struct Raft {
    state: Arc<RaftState>,
}

#[derive(Clone, Debug)]
enum RaftStatus {
    Follower { leader_id: Option<NodeId> },
    Candidate,
    Leader,
}

#[derive(Debug)]
struct RaftState {
    status: RwLock<RaftStatus>,
    node_id: NodeId,
    session_key: String,
    cluster_size: usize,
    peer_clients: HashMap<NodeId, Client<RpcRequest, RpcResponse>>,
    heartbeat_tx: Mutex<Option<Sender<NodeId>>>,
    cmds_tx: CmdSender,

    // Volatile state
    commit_index: AtomicU64,
    last_applied: AtomicU64,

    // Persistent state
    persistent: PersistentState,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
enum RpcRequest {
    AppendEntries(AppendEntriesRequest),
    RequestVote(RequestVoteRequest),
}

#[derive(Serialize, Deserialize, Debug, Clone)]
enum RpcResponse {
    AppendEntries(AppendEntriesResponse),
    RequestVote(RequestVoteResponse),
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct AppendEntriesRequest {
    term: TermIndex,
    leader_id: NodeId,
    prev_log_index: LogIndex,
    prev_log_term: TermIndex,
    entries: Vec<LogEntry>,
    leader_commit: LogIndex,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct RequestVoteRequest {
    term: TermIndex,
    candidate: NodeId,
    last_log_index: LogIndex,
    last_log_term: TermIndex,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct RequestVoteResponse {
    term: TermIndex,
    vote_granted: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct AppendEntriesResponse {
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

        let (cmds_tx, cmds_rx) = channel(100);
        let persistent = PersistentState::new(&session_key);
        console_log!("FIRING UP!!!");
        let raft = Self {
            state: Arc::new(RaftState {
                status: RwLock::new(RaftStatus::Follower { leader_id: None }),
                persistent,
                node_id,
                session_key,
                cluster_size,
                peer_clients,
                cmds_tx,
                commit_index: 0.into(),
                last_applied: 0.into(),
                heartbeat_tx: Mutex::new(None),
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
            r.run(cmds_rx).await;
        });

        Ok(raft)
    }

    pub async fn set(&self, key: String, data: Vec<u8>) -> Result<(), Error> {
        let (resp_tx, mut resp_rx) = oneshot::channel();
        let cmd = LogCmd::Set { key, data };
        self.state
            .cmds_tx
            .clone()
            .send((cmd, resp_tx))
            .await
            .unwrap();

        select! {
            res = resp_rx => res.expect("command channel closed"),
            _ = sleep(Duration::from_millis(COMMAND_TIMEOUT_MILLIS)) => Err(Error::CommandTimeout),
        }
    }

    fn set_status(&self, status: RaftStatus) {
        let mut s = self.state.status.write().unwrap();
        *s = status;
    }

    fn get_status(&self) -> RaftStatus {
        let s = self.state.status.read().unwrap();
        s.clone()
    }

    fn votes_required(&self) -> usize {
        (self.state.cluster_size / 2) + 1
    }

    async fn run(&self, mut cmds_rx: CmdReceiver) {
        loop {
            match self.get_status() {
                RaftStatus::Follower { .. } => self.be_follower(&mut cmds_rx).await,
                RaftStatus::Candidate => self.be_candidate().await,
                RaftStatus::Leader { .. } => self.be_leader(&mut cmds_rx).await,
            }
        }
    }

    async fn be_follower(&self, cmds_rx: &mut CmdReceiver) {
        console_log!("BEING A FOLLOWER");
        let mut hb_rx = self.get_heartbeat();
        let (done_tx, mut done_rx) = oneshot::channel::<()>();

        let s = self.clone();
        spawn_local(async move {
            loop {
                select! {
                    _ = sleep(election_timeout()) => {
                        console_log!("Calling election!");
                        s.set_status(RaftStatus::Candidate);
                        done_tx.send(()).unwrap();
                        return;
                    }
                    res = hb_rx.next() => {
                        let leader_id = Some(res.unwrap());
                        s.set_status(RaftStatus::Follower { leader_id });
                    }
                }
            }
        });

        loop {
            select! {
                res = cmds_rx.next() => {
                    console_log!("WOULD FORWARD REQUEST: {:?}", res);
                }
                _ = done_rx => {
                    return;
                }
            }
        }
    }

    async fn be_candidate(&self) {
        console_log!("BEING A CANDIDATE!");
        let mut hb_rx = self.get_heartbeat();
        let persistent = &self.state.persistent;
        persistent.set_voted_for(Some(&self.state.node_id));
        persistent.set_current_term(persistent.current_term() + 1);

        let req = RpcRequest::RequestVote(RequestVoteRequest {
            term: persistent.current_term(),
            candidate: self.state.node_id.to_string(),
            last_log_index: persistent.last_log_index(),
            last_log_term: persistent.last_log_term(),
        });
        let mut votes = 1; // Voted for self
        let mut vote_calls = self
            .state
            .peer_clients
            .iter()
            .map(|(_k, client)| client.call(req.clone()))
            .collect::<FuturesUnordered<_>>();

        let mut timeout = sleep(election_timeout());
        loop {
            select! {
                res = vote_calls.next() =>  {
                    if let Some(Ok(RpcResponse::RequestVote(resp))) = res {
                        if resp.vote_granted {
                            votes += 1;
                        }
                    }
                    if votes >= self.votes_required() {
                        console_log!("I WIN!!!");
                        self.set_status(RaftStatus::Leader);
                        break;
                    }
                }
                res = hb_rx.next() => {
                    let leader_id = res.unwrap();
                    console_log!("LEADER: {:#?}", leader_id);
                    console_log!("I LOSE!");
                    // We got a heartbeat, so we're a follower now
                    self.set_status(RaftStatus::Follower { leader_id: Some(leader_id) });
                    break;
                }
                _ = timeout => {
                    console_log!("ELECTION TIMED OUT");
                    // Election timed out, try again
                    break;
                }
            }
        }
        persistent.set_voted_for(None);
    }

    async fn be_leader(&self, cmds_rx: &mut CmdReceiver) {
        console_log!("BEING A LEADER");
        let term = self.state.persistent.current_term();
        let mut _next_indices: HashMap<NodeId, LogIndex> = HashMap::new();
        let mut _match_indices: HashMap<NodeId, LogIndex> = HashMap::new();

        loop {
            select! {
                res = cmds_rx.next() => {
                    console_log!("Handling: {:?}", res);
                }
                _ = sleep(Duration::from_millis(HEARBEAT_INTERVAL_MILLIS)) => {
                    let req = RpcRequest::AppendEntries(AppendEntriesRequest {
                        term,
                        leader_commit: 0, // TODO: fix
                        prev_log_index: 0,
                        prev_log_term: 0,
                        leader_id: self.state.node_id.to_string(),
                        entries: Vec::new(),
                    });
                    let mut heartbeat_calls = self
                        .state
                        .peer_clients
                        .iter()
                        .map(|(_a, client)| client.call(req.clone()))
                        .collect::<FuturesUnordered<_>>();
                    while let Some(_res) = heartbeat_calls.next().await {}
                }
            }
        }
    }

    fn get_heartbeat(&self) -> Receiver<NodeId> {
        let (tx, rx) = channel(10);
        let mut hb_tx = self.state.heartbeat_tx.lock().unwrap();
        *hb_tx = Some(tx);

        rx
    }

    fn update_term(&self, term: TermIndex) {
        let current_term = self.state.persistent.current_term();
        if current_term < term {
            self.state.persistent.set_current_term(term);
            match self.get_status() {
                RaftStatus::Follower { .. } => (),
                _ => {
                    self.set_status(RaftStatus::Follower { leader_id: None });
                }
            }
        }
    }

    async fn handle_request_vote(&self, req: RequestVoteRequest) -> RequestVoteResponse {
        self.update_term(req.term);

        let persistent = &self.state.persistent;
        let voted_for = persistent.voted_for();
        let current_term = persistent.current_term();
        let commit_index = self.state.commit_index.load(Ordering::SeqCst);
        if req.term >= current_term
            && (voted_for == None || *voted_for.unwrap() == req.candidate)
            && req.last_log_index >= commit_index
        {
            persistent.set_voted_for(Some(&req.candidate));

            return RequestVoteResponse {
                term: req.last_log_term,
                vote_granted: true,
            };
        }

        RequestVoteResponse {
            term: 0,
            vote_granted: false,
        }
    }

    async fn send_heartbeat(&self, leader_id: &str) {
        let hb_tx = self.state.heartbeat_tx.lock().unwrap().clone();
        match hb_tx {
            None => (),
            Some(mut tx) => {
                // If the heartbeat fails it's probably because we changed state
                // or something, so ignore errors.
                let _ = tx.send(leader_id.to_string()).await;
            }
        }
    }

    async fn handle_append_entries(&self, req: AppendEntriesRequest) -> AppendEntriesResponse {
        self.update_term(req.term);
        self.send_heartbeat(&req.leader_id).await;

        let term = self.state.persistent.current_term();

        AppendEntriesResponse {
            term,
            success: true,
        }
    }
}

fn election_timeout() -> Duration {
    let delay = thread_rng().gen_range(150..300);
    Duration::from_millis(delay)
}

#[async_trait]
impl RequestHandler<RpcRequest, RpcResponse> for Raft {
    async fn handle(&self, req: RpcRequest) -> Result<RpcResponse, transport::Error> {
        match req {
            RpcRequest::AppendEntries(req) => Ok(RpcResponse::AppendEntries(
                self.handle_append_entries(req).await,
            )),
            RpcRequest::RequestVote(req) => Ok(RpcResponse::RequestVote(
                self.handle_request_vote(req).await,
            )),
        }
    }
}

impl Debug for Raft {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result<(), std::fmt::Error> {
        write!(
            f,
            "Raft (id: {}, state: {:?})",
            self.state.node_id,
            self.get_status()
        )?;
        Ok(())
    }
}
