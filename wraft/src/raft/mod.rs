pub mod errors;
mod persistence;

use crate::console_log;
use crate::util::sleep;
use crate::webrtc_rpc::introduce;
use crate::webrtc_rpc::transport::{Client, RequestContext, RequestHandler};
use async_trait::async_trait;
use errors::Error;
use futures::channel::mpsc::{channel, Receiver, Sender};
use futures::channel::oneshot;
use futures::select;
use futures::sink::SinkExt;
use futures::stream::{FuturesUnordered, StreamExt};
use persistence::{LogEntry, PersistentState};
use rand::{thread_rng, Rng};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt::{Debug, Formatter};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;
use wasm_bindgen_futures::spawn_local;

pub type LogPosition = u64;
pub type TermIndex = u64;
pub type NodeId = String;

#[derive(Clone)]
pub struct Raft {
    state: Arc<RaftState>,
}

#[derive(Clone, PartialEq, Debug)]
enum RaftStatus {
    Follower { leader: Option<NodeId> },
    Candidate,
    Leader,
}

#[derive(Debug)]
struct RaftState {
    status: RwLock<RaftStatus>,
    node_id: NodeId,
    session_key: String,
    cluster_size: usize,
    peer_clients: HashMap<NodeId, Client<RPCRequest, RPCResponse>>,
    heartbeat_tx: Mutex<Option<Sender<NodeId>>>,

    // Volatile state
    commit_index: AtomicU64,
    last_applied: AtomicU64,

    // Persistent state
    persistent: PersistentState,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
enum RPCRequest {
    AppendEntries(AppendEntriesRequest),
    RequestVote(RequestVoteRequest),
}

#[derive(Serialize, Deserialize, Debug, Clone)]
enum RPCResponse {
    AppendEntries(AppendEntriesResponse),
    RequestVote(RequestVoteResponse),
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct AppendEntriesRequest {
    term: TermIndex,
    leader: NodeId,
    prev_log_index: LogPosition,
    prev_log_term: TermIndex,
    entries: Vec<LogEntry>,
    leader_commit: LogPosition,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct RequestVoteRequest {
    term: TermIndex,
    candidate: NodeId,
    last_log_index: LogPosition,
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

impl Raft {
    pub async fn initiate(
        node_id: NodeId,
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
            let peer = peers_rx.next().await.ok_or(Error::NotEnoughPeers())?;
            console_log!("Got client: {}", peer.node_id());

            let peer_id = peer.node_id().clone();
            peer_clients.insert(peer_id, peer.client());
            peers.push(peer);
        }

        let persistent = PersistentState::new(&session_key);
        console_log!("FIRING UP!!!");
        let raft = Self {
            state: Arc::new(RaftState {
                status: RwLock::new(RaftStatus::Follower { leader: None }),
                persistent,
                node_id,
                session_key,
                cluster_size,
                peer_clients,
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
            r.run().await;
        });

        Ok(raft)
    }

    // fn status_is(&self, status: RaftStatus) -> bool {
    //     let s = self.state.status.read().unwrap();
    //     *s == status
    // }

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

    async fn run(&self) {
        loop {
            match self.get_status() {
                RaftStatus::Follower { .. } => self.be_follower().await,
                RaftStatus::Candidate => self.be_candidate().await,
                RaftStatus::Leader => self.be_leader().await,
            }
        }
    }

    async fn be_follower(&self) {
        console_log!("BEING A FOLLOWER");
        let mut hb_rx = self.get_heartbeat();
        let (timeout_tx, mut timeout) = oneshot::channel::<()>();
        spawn_local(async move {
            sleep(election_timeout()).await;
            let _ = timeout_tx.send(());
        });

        loop {
            select! {
                _ = timeout => {
                    console_log!("Calling election!");
                    self.set_status(RaftStatus::Candidate);
                    return;
                }
                res = hb_rx.next() => {
                    let leader = Some(res.unwrap());
                    self.set_status(RaftStatus::Follower { leader });
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

        let req = RPCRequest::RequestVote(RequestVoteRequest {
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

        let (timeout_tx, mut timeout) = oneshot::channel::<()>();
        spawn_local(async move {
            sleep(election_timeout()).await;
            let _ = timeout_tx.send(());
        });
        loop {
            select! {
                res = vote_calls.next() =>  {
                    if let Some(Ok(RPCResponse::RequestVote(resp))) = res {
                        if resp.vote_granted {
                            votes += 1;
                        }
                    }
                    console_log!("VOTES: {:#?}", votes);
                    if votes >= self.votes_required() {
                        console_log!("I WIN!!!");
                        self.set_status(RaftStatus::Leader);
                        break;
                    }
                }
                res = hb_rx.next() => {
                    let leader = res.unwrap();
                    console_log!("LEADER: {:#?}", leader);
                    console_log!("I LOSE!");
                    // We got a heartbeat, so we're a follower now
                    self.set_status(RaftStatus::Follower { leader: Some(leader) });
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

    async fn be_leader(&self) {
        console_log!("BEING A LEADER");
        let term = self.state.persistent.current_term();
        while let RaftStatus::Leader = self.get_status() {
            let req = RPCRequest::AppendEntries(AppendEntriesRequest {
                term,
                leader_commit: 0, // TODO: fix
                prev_log_index: 0,
                prev_log_term: 0,
                leader: self.state.node_id.to_string(),
                entries: Vec::new(),
            });
            let mut heartbeat_calls = self
                .state
                .peer_clients
                .iter()
                .map(|(_a, client)| client.call(req.clone()))
                .collect::<FuturesUnordered<_>>();
            while let Some(_res) = heartbeat_calls.next().await {}
            sleep(Duration::from_millis(50)).await;
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
                    self.set_status(RaftStatus::Follower { leader: None });
                }
            }
        }
    }

    async fn handle_request_vote(&self, req: RequestVoteRequest) -> RequestVoteResponse {
        console_log!("Responding to vote request for {}", req.candidate);
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

    async fn send_heartbeat(&self, leader: &str) {
        let hb_tx = self.state.heartbeat_tx.lock().unwrap().clone();
        match hb_tx {
            None => (),
            Some(mut tx) => match tx.send(leader.to_string()).await {
                Ok(()) => (),
                Err(err) => console_log!("heartbeat channel error: {}", err),
            },
        }
    }

    async fn handle_append_entries(&self, req: AppendEntriesRequest) -> AppendEntriesResponse {
        self.update_term(req.term);
        self.send_heartbeat(&req.leader).await;

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
impl RequestHandler<RPCRequest, RPCResponse> for Raft {
    async fn handle(&self, req: RPCRequest, cx: RequestContext) -> RPCResponse {
        console_log!("Got {:?} from {}", req, cx.source_node_id);
        match req {
            RPCRequest::AppendEntries(req) => {
                RPCResponse::AppendEntries(self.handle_append_entries(req).await)
            }
            RPCRequest::RequestVote(req) => {
                RPCResponse::RequestVote(self.handle_request_vote(req).await)
            }
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
