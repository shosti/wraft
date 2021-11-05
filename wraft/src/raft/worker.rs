use crate::console_log;
use crate::raft::persistence::PersistentState;
use crate::raft::rpc_server::RpcServer;
use crate::raft::{
    AppendEntriesRequest, AppendEntriesResponse, ClientError, ClientMessage, ClientRequest,
    ClientResponse, LogCmd, LogEntry, LogIndex, NodeId, RequestVoteRequest, RequestVoteResponse,
    RpcMessage, RpcRequest, RpcResponse,
};
use crate::util::{interval, sleep, Sleep};
use crate::webrtc_rpc::transport::{self, Client, PeerTransport};
use futures::channel::mpsc::{channel, Receiver, Sender};
use futures::channel::oneshot;
use futures::select;
use futures::sink::SinkExt;
use futures::stream::StreamExt;
use rand::{thread_rng, Rng};
use std::collections::HashMap;
use std::time::Duration;
use wasm_bindgen_futures::spawn_local;

const HEARBEAT_INTERVAL_MILLIS: u64 = 50;

enum RaftWorkerState {
    Follower(RaftWorker<Follower>),
    Candidate(RaftWorker<Candidate>),
    Leader(RaftWorker<Leader>),
}

struct RaftWorker<S> {
    state: RaftState,
    status: S,
}

#[derive(Debug)]
struct Follower {}
#[derive(Debug)]
struct Candidate {}
#[derive(Debug)]
struct Leader {
    _next_indices: HashMap<NodeId, LogIndex>,
    _match_indices: HashMap<NodeId, LogIndex>,
}

type RpcClient = Client<RpcRequest, RpcResponse>;

enum StateChange {
    Continue,
    BecomeFollower,
}

struct RaftState {
    leader_id: Option<NodeId>,
    node_id: NodeId,
    session_key: String,
    cluster_size: usize,
    peer_clients: HashMap<NodeId, RpcClient>,
    client_rx: Receiver<ClientMessage>,
    rpc_rx: Receiver<RpcMessage>,
    rpc_server: RpcServer,
    peers_rx: Receiver<PeerTransport>,
    debug_tx: Sender<RaftDebugState>,

    // Volatile state
    commit_index: u64,
    last_applied: u64,

    // Persistent state
    persistent: PersistentState,
}

#[derive(Debug)]
pub struct RaftDebugState {
    status: String,
    leader_id: Option<NodeId>,
    node_id: NodeId,
    session_key: String,
    cluster_size: usize,
    peers: Vec<NodeId>,
    voted_for: Option<NodeId>,
    current_term: u64,

    commit_index: u64,
    last_applied: u64,
    snapshot: HashMap<String, String>,
}

pub fn run(
    node_id: NodeId,
    session_key: String,
    rpc_rx: Receiver<RpcMessage>,
    peers_rx: Receiver<PeerTransport>,
    peer_clients: HashMap<NodeId, RpcClient>,
    rpc_server: RpcServer,
) -> (Sender<ClientMessage>, Receiver<RaftDebugState>) {
    let (client_tx, client_rx) = channel(100);
    let (debug_tx, debug_rx) = channel(100);

    let cluster_size = peer_clients.len();
    let persistent = PersistentState::new(&session_key);
    let state = RaftState {
        leader_id: None,
        persistent,
        node_id,
        session_key,
        cluster_size,
        peer_clients,
        rpc_rx,
        client_rx,
        peers_rx,
        rpc_server,
        debug_tx,
        commit_index: 0,
        last_applied: 0,
    };

    spawn_local(async move {
        let mut worker = RaftWorkerState::Follower(RaftWorker::new(state));
        loop {
            worker = worker.next().await;
        }
    });

    (client_tx, debug_rx)
}

impl RaftWorkerState {
    async fn next(self) -> Self {
        match self {
            RaftWorkerState::Follower(worker) => worker.next().await,
            RaftWorkerState::Candidate(worker) => worker.next().await,
            RaftWorkerState::Leader(worker) => worker.next().await,
        }
    }
}

impl<S> RaftWorker<S>
where
    S: std::fmt::Debug,
{
    fn election_timeout(&self) -> Sleep {
        let delay = thread_rng().gen_range(150..300);
        sleep(Duration::from_millis(delay))
    }

    fn votes_required(&self) -> usize {
        (self.state.cluster_size / 2) + 1
    }

    fn handle_request_vote(&mut self, req: &RequestVoteRequest) -> RpcResponse {
        let voted_for = self.state.persistent.voted_for();
        let current_term = self.state.persistent.current_term();
        if req.term >= current_term
            && (voted_for.is_none() || *voted_for.as_ref().unwrap() == req.candidate)
            && req.last_log_index >= self.state.commit_index
        {
            self.state.persistent.set_voted_for(Some(&req.candidate));

            RpcResponse::RequestVote(RequestVoteResponse {
                term: req.last_log_term,
                vote_granted: true,
            })
        } else {
            RpcResponse::RequestVote(RequestVoteResponse {
                term: 0,
                vote_granted: false,
            })
        }
    }

    fn handle_append_entries(&self, _req: &AppendEntriesRequest) -> RpcResponse {
        let term = self.state.persistent.current_term();

        // TODO: append the actual entries

        RpcResponse::AppendEntries(AppendEntriesResponse {
            term,
            success: true,
        })
    }

    fn handle_new_peer(&mut self, peer: PeerTransport) {
        let (client, mut server) = peer.start();
        let peer_id = client.node_id();
        console_log!("Got new peer: {}", peer_id);

        let s = self.state.rpc_server.clone();
        spawn_local(async move {
            server.serve(s).await;
        });
        self.state.peer_clients.insert(client.node_id(), client);
    }

    fn send_debug(&self) {
        let debug: RaftDebugState = self.into();
        let mut tx = self.state.debug_tx.clone();
        spawn_local(async move {
            let _ = tx.send(debug).await;
        });
    }
}

impl RaftWorker<Follower> {
    pub fn new(state: RaftState) -> Self {
        Self {
            state,
            status: Follower {},
        }
    }

    async fn next(mut self) -> RaftWorkerState {
        console_log!("BEING A FOLLOWER");
        let mut timeout = self.election_timeout();
        let mut debug_interval = interval(Duration::from_secs(1));

        loop {
            select! {
                res = self.state.rpc_rx.next() => {
                    let (req, resp_tx) = res.expect("RPC channel closed");
                    self.handle_rpc(req, resp_tx, &mut timeout);
                }
                res = self.state.client_rx.next() => {
                    let (req, _resp_tx) = res.expect("Client channel closed");
                    console_log!("WOULD FORWARD REQUEST {:?} TO {:?}", req, self.state.leader_id);
                }
                res = self.state.peers_rx.next() => {
                    self.handle_new_peer(res.expect("peer channel closed"));
                }
                _ = timeout => {
                    console_log!("Calling election!");
                    return RaftWorkerState::Candidate(self.into());
                }
                _ = debug_interval.next() => {
                    self.send_debug();
                }
            }
        }
    }

    fn handle_rpc(
        &mut self,
        req: RpcRequest,
        resp_tx: oneshot::Sender<Result<RpcResponse, transport::Error>>,
        timeout: &mut Sleep,
    ) {
        match req {
            RpcRequest::RequestVote(req) => {
                self.state.persistent.update_term(req.term);
                let resp = self.handle_request_vote(&req);
                resp_tx.send(Ok(resp)).expect("RPC response channel closed");
            }
            RpcRequest::AppendEntries(req) => {
                self.state.persistent.update_term(req.term);
                self.state.leader_id = Some(req.leader_id.clone());
                let resp = self.handle_append_entries(&req);
                resp_tx.send(Ok(resp)).expect("RPC response channel closed");

                // Got heartbeat, reset timeout
                *timeout = self.election_timeout();
            }
        }
    }
}

impl RaftWorker<Candidate> {
    async fn next(mut self) -> RaftWorkerState {
        console_log!("BEING A CANDIDATE");
        self.state.persistent.increment_term();
        self.vote_for_self();
        let mut votes = 1; // Voted for self

        let mut votes_rx = self.request_votes();
        let mut timeout = self.election_timeout();
        let mut debug_interval = interval(Duration::from_secs(1));
        loop {
            select! {
                res = votes_rx.next() => {
                    if res.is_some() {
                        votes += 1;
                        if votes >= self.votes_required() {
                            console_log!("I WIN!!!");
                            return RaftWorkerState::Leader(self.into());
                        }
                    }
                }
                res = self.state.rpc_rx.next() => {
                    let (req, resp_tx) = res.expect("RPC channel closed");
                    if let StateChange::BecomeFollower = self.handle_rpc(req, resp_tx) {
                        return RaftWorkerState::Follower(self.into());
                    }
                }
                _ = debug_interval.next() => {
                    self.send_debug();
                }
                _ = timeout => {
                    console_log!("ELECTION TIMED OUT");
                    return RaftWorkerState::Candidate(self);
                }
            }
        }
    }

    fn vote_for_self(&mut self) {
        self.state
            .persistent
            .set_voted_for(Some(&self.state.node_id));
    }

    fn handle_rpc(
        &mut self,
        req: RpcRequest,
        resp_tx: oneshot::Sender<Result<RpcResponse, transport::Error>>,
    ) -> StateChange {
        match req {
            RpcRequest::RequestVote(req) => {
                let new_term = self.state.persistent.update_term(req.term);
                let resp = self.handle_request_vote(&req);
                resp_tx.send(Ok(resp)).expect("RPC response channel closed");
                if new_term {
                    // We got a request from a higher term, switch
                    // back to being a follower
                    StateChange::BecomeFollower
                } else {
                    StateChange::Continue
                }
            }
            RpcRequest::AppendEntries(req) => {
                self.state.persistent.update_term(req.term);
                let resp = self.handle_append_entries(&req);
                resp_tx.send(Ok(resp)).expect("RPC response channel closed");
                // Got a heartbeat, so we lose the election
                console_log!("I LOSE!");
                StateChange::BecomeFollower
            }
        }
    }

    fn request_votes(&self) -> Receiver<()> {
        let (votes_tx, votes_rx) = channel(self.state.cluster_size);
        let req = RpcRequest::RequestVote(RequestVoteRequest {
            term: self.state.persistent.current_term(),
            candidate: self.state.node_id.to_string(),
            last_log_index: self.state.persistent.last_log_index(),
            last_log_term: self.state.persistent.last_log_term(),
        });
        for client in self
            .state
            .peer_clients
            .values()
            .filter(|c| c.is_connected())
        {
            let mut c = client.clone();
            let mut tx = votes_tx.clone();
            let r = req.clone();
            spawn_local(async move {
                match c.call(r).await {
                    Ok(RpcResponse::RequestVote(resp)) if resp.vote_granted => {
                        // If channel is closed, the election is probably over so ignore
                        // the error
                        let _ = tx.send(()).await;
                    }
                    Ok(RpcResponse::RequestVote(_)) => {
                        // We don't really care if we didn't get a vote
                    }
                    Err(err) => {
                        // We don't care too much about errors because we'll
                        // just get another election if this one times out
                        console_log!("Error getting vote: {:?}", err);
                    }
                    _ => unreachable!(),
                };
            });
        }

        votes_rx
    }
}

impl RaftWorker<Leader> {
    async fn next(mut self) -> RaftWorkerState {
        console_log!("BEING A LEADER");

        let (resps_tx, mut resps_rx) = channel(100);
        let mut debug_interval = interval(Duration::from_secs(1));
        let mut log_votes = HashMap::<LogIndex, usize>::new();
        let mut client_set_resps =
            HashMap::<LogIndex, oneshot::Sender<Result<ClientResponse, ClientError>>>::new();

        // Initial heartbeat happens immediately
        let mut heartbeat = sleep(Duration::from_millis(0));
        loop {
            select! {
                res = resps_rx.next() => {
                    let resp = res.expect("response channel closed");
                    if let StateChange::BecomeFollower = self.handle_append_entries_response(resp) {
                        return RaftWorkerState::Follower(self.into());
                    }
                }
                res = self.state.rpc_rx.next() => {
                    let (req, resp_tx) = res.expect("RPC channel closed");
                    if let StateChange::BecomeFollower = self.handle_rpc(req, resp_tx) {
                        return RaftWorkerState::Follower(self.into());
                    }
                }
                res = self.state.client_rx.next() => {
                    let (req, resp_tx) = res.expect("client channel closed");
                    match req {
                        ClientRequest::Get(ref key) => self.handle_get_request(key, resp_tx),
                        ClientRequest::Set(key, val) => {
                            let cmd = LogCmd::Set { key, data: val };
                            let idx = self.state.persistent.append_log(cmd);
                            client_set_resps.insert(idx, resp_tx);
                            log_votes.insert(idx, 0);
                        }
                    }
                }
                _ = heartbeat => {
                    let empty_entries = Vec::new();
                    self.send_append_entries(empty_entries, &resps_tx);
                    heartbeat = self.heartbeat_timeout();
                }
                _ = debug_interval.next() => {
                    self.send_debug();
                }
            }
        }
    }

    fn heartbeat_timeout(&self) -> Sleep {
        sleep(Duration::from_millis(HEARBEAT_INTERVAL_MILLIS))
    }

    fn handle_rpc(
        &mut self,
        req: RpcRequest,
        resp_tx: oneshot::Sender<Result<RpcResponse, transport::Error>>,
    ) -> StateChange {
        let term = self.state.persistent.current_term();
        match req {
            RpcRequest::AppendEntries(req) => {
                if self.state.persistent.update_term(req.term) {
                    let resp = self.handle_append_entries(&req);
                    resp_tx.send(Ok(resp)).expect("response channel closed");
                    return StateChange::BecomeFollower;
                } else {
                    let resp = RpcResponse::AppendEntries(AppendEntriesResponse {
                        term,
                        success: false,
                    });
                    resp_tx.send(Ok(resp)).expect("response channel closed");
                }
            }
            RpcRequest::RequestVote(req) => {
                if self.state.persistent.update_term(req.term) {
                    let resp = self.handle_request_vote(&req);
                    resp_tx.send(Ok(resp)).expect("response channel closed");
                    return StateChange::BecomeFollower;
                } else {
                    let resp = RpcResponse::RequestVote(RequestVoteResponse {
                        term,
                        vote_granted: false,
                    });
                    resp_tx.send(Ok(resp)).expect("response channel closed");
                }
            }
        }

        StateChange::Continue
    }

    fn handle_append_entries_response(
        &mut self,
        resp: Result<RpcResponse, transport::Error>,
    ) -> StateChange {
        match resp {
            Ok(RpcResponse::AppendEntries(resp)) => {
                if self.state.persistent.update_term(resp.term) {
                    // A higher term is out there; switch to being a
                    // follower
                    return StateChange::BecomeFollower;
                }
                // if resp.success {
                //     console_log!("successful thing: {:?}", resp);
                // } else {
                //     console_log!("unsuccessful thing: {:?}", resp);
                // }
            }
            Err(err) => {
                console_log!("ERROR!!! {:?}", err);
            }
            _ => unreachable!(),
        }

        StateChange::Continue
    }

    fn send_append_entries(
        &self,
        entries: Vec<LogEntry>,
        resps_tx: &Sender<Result<RpcResponse, transport::Error>>,
    ) {
        let req = RpcRequest::AppendEntries(AppendEntriesRequest {
            term: self.state.persistent.current_term(),
            leader_id: self.state.node_id.clone(),
            // TODO: fix all of these
            leader_commit: 0,
            prev_log_index: 0,
            prev_log_term: 0,
            entries,
        });
        for mut client in self
            .state
            .peer_clients
            .values()
            .filter(|c| c.is_connected())
            .cloned()
        {
            let r = req.clone();
            let mut tx = resps_tx.clone();
            spawn_local(async move {
                let resp = client.call(r.clone()).await;
                // If the other side is closed, we've probably changed state and
                // it doesn't matter
                let _ = tx.send(resp).await;
            });
        }
    }

    fn handle_get_request(
        &self,
        key: &str,
        client_resp_tx: oneshot::Sender<Result<ClientResponse, ClientError>>,
    ) {
        let val = self.state.persistent.get(key);
        let resp = ClientResponse::Get(val);
        let _ = client_resp_tx.send(Ok(resp));
    }
}

// State machine transitions
impl From<RaftWorker<Candidate>> for RaftWorker<Follower> {
    fn from(from: RaftWorker<Candidate>) -> Self {
        Self {
            state: from.state,
            status: Follower {},
        }
    }
}

impl From<RaftWorker<Leader>> for RaftWorker<Follower> {
    fn from(from: RaftWorker<Leader>) -> Self {
        Self {
            state: from.state,
            status: Follower {},
        }
    }
}

impl From<RaftWorker<Follower>> for RaftWorker<Candidate> {
    fn from(from: RaftWorker<Follower>) -> Self {
        let mut next = Self {
            state: from.state,
            status: Candidate {},
        };
        next.state.leader_id = None;
        next
    }
}

impl From<RaftWorker<Candidate>> for RaftWorker<Leader> {
    fn from(from: RaftWorker<Candidate>) -> Self {
        let mut next = Self {
            state: from.state,
            status: Leader {
                _next_indices: HashMap::new(),
                _match_indices: HashMap::new(),
            },
        };
        next.state.leader_id = Some(next.state.node_id.clone());
        next
    }
}

impl<S> From<&RaftWorker<S>> for RaftDebugState
where
    S: std::fmt::Debug,
{
    fn from(from: &RaftWorker<S>) -> Self {
        Self {
            status: format!("{:?}", from.status),
            leader_id: from.state.leader_id.clone(),
            node_id: from.state.node_id.clone(),
            session_key: from.state.session_key.clone(),
            cluster_size: from.state.cluster_size,
            peers: from
                .state
                .peer_clients
                .iter()
                .filter(|(_, c)| c.is_connected())
                .map(|(k, _)| k.to_string())
                .collect(),
            voted_for: from.state.persistent.voted_for().clone(),
            current_term: from.state.persistent.current_term(),

            commit_index: from.state.commit_index,
            last_applied: from.state.last_applied,
            snapshot: from.state.persistent.snapshot(),
        }
    }
}
