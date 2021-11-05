use crate::console_log;
use crate::raft::persistence::PersistentState;
use crate::raft::rpc_server::RpcServer;
use crate::raft::{
    AppendEntriesRequest, AppendEntriesResponse, ClientError, ClientMessage, ClientRequest,
    ClientResponse, LogCmd, LogIndex, NodeId, RequestVoteRequest, RequestVoteResponse, RpcMessage,
    RpcRequest, RpcResponse, TermIndex,
};
use crate::util::{interval, sleep, Sleep};
use crate::webrtc_rpc::transport::{self, Client, PeerTransport};
use futures::channel::mpsc::{channel, Receiver, Sender};
use futures::channel::oneshot;
use futures::select;
use futures::sink::SinkExt;
use futures::stream::StreamExt;
use rand::{thread_rng, Rng};
use std::cmp::{max, min};
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
struct InFlightAppend {
    votes: usize,
    resp_tx: oneshot::Sender<ClientResult>,
}

#[derive(Debug)]
struct Follower {}
#[derive(Debug)]
struct Candidate {}
#[derive(Debug)]
struct Leader {
    next_indices: HashMap<NodeId, LogIndex>,
    match_indices: HashMap<NodeId, LogIndex>,
    in_flight: HashMap<LogIndex, oneshot::Sender<ClientResult>>,
    responses_tx: Sender<(NodeId, TermIndex, RpcResult)>,
    responses_rx: Option<Receiver<(NodeId, TermIndex, RpcResult)>>,
}

type RpcClient = Client<RpcRequest, RpcResponse>;
type RpcResult = Result<RpcResponse, transport::Error>;
type ClientResult = Result<ClientResponse, ClientError>;

enum StateChange {
    Continue,
    BecomeFollower,
}

struct RaftState {
    leader_id: Option<NodeId>,
    node_id: NodeId,
    session_key: String,
    cluster_size: usize,
    client_rx: Receiver<ClientMessage>,
    rpc_rx: Receiver<RpcMessage>,
    rpc_server: RpcServer,
    peers_rx: Receiver<PeerTransport>,
    debug_tx: Sender<RaftDebugState>,
    current_state: HashMap<String, String>,

    // Peers is constant throughout the runtime (regardless of whether they're
    // online or not)
    peers: Vec<NodeId>,
    // Peer clients could connect/disconnect
    peer_clients: HashMap<NodeId, RpcClient>,

    // Volatile state
    commit_index: TermIndex,
    last_applied: TermIndex,

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
    online_peers: Vec<NodeId>,
    voted_for: Option<NodeId>,
    current_term: TermIndex,

    commit_index: LogIndex,
    last_applied: LogIndex,
    current_state: HashMap<String, String>,
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
    let peers = peer_clients.keys().cloned().collect();
    let state = RaftState {
        leader_id: None,
        persistent,
        node_id,
        session_key,
        cluster_size,
        peer_clients,
        peers,
        rpc_rx,
        client_rx,
        peers_rx,
        rpc_server,
        debug_tx,
        current_state: HashMap::new(),
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

    fn quorum(&self) -> usize {
        (self.state.cluster_size / 2) + 1
    }

    fn handle_request_vote(&mut self, req: RequestVoteRequest) -> RequestVoteResponse {
        let voted_for = self.state.persistent.voted_for();
        let current_term = self.state.persistent.current_term();
        if req.term >= current_term
            && (voted_for.is_none() || *voted_for.as_ref().unwrap() == req.candidate)
            && req.last_log_index >= self.state.commit_index
        {
            self.state.persistent.set_voted_for(Some(&req.candidate));

            RequestVoteResponse {
                term: req.last_log_term,
                vote_granted: true,
            }
        } else {
            RequestVoteResponse {
                term: 0,
                vote_granted: false,
            }
        }
    }

    fn handle_append_entries(&mut self, req: AppendEntriesRequest) -> AppendEntriesResponse {
        let current_term = self.state.persistent.current_term();
        if req.term < current_term {
            return AppendEntriesResponse {
                term: current_term,
                success: false,
            };
        }
        let entry = self.state.persistent.get_log(req.prev_log_index);
        if req.prev_log_index != 0 && (entry.is_none() || entry.unwrap().term != req.prev_log_term)
        {
            return AppendEntriesResponse {
                term: current_term,
                success: false,
            };
        }
        for e in &req.entries {
            if let Some(existing) = self.state.persistent.get_log(e.idx) {
                if existing.term != e.term {
                    self.state.persistent.truncate_from(e.idx);
                }
            }
        }
        for e in req.entries {
            let new = self.state.persistent.append_log(e.cmd);
            assert_eq!(new.idx, e.idx);
        }

        if req.leader_commit > self.state.commit_index {
            self.state.commit_index = req.leader_commit
        }

        AppendEntriesResponse {
            term: current_term,
            success: true,
        }
    }

    fn apply_log(&mut self) {
        for entry in self
            .state
            .persistent
            .sublog((self.state.last_applied + 1)..=self.state.commit_index)
        {
            match entry.cmd {
                LogCmd::Set { key, data } => {
                    self.state.current_state.insert(key, data);
                }
                LogCmd::Delete { ref key } => {
                    self.state.current_state.remove(key);
                }
            }
            self.state.last_applied = entry.idx;
        }
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
            self.apply_log();
            select! {
                res = self.state.rpc_rx.next() => {
                    let (req, resp_tx) = res.expect("RPC channel closed");
                    self.handle_rpc(req, resp_tx, &mut timeout);
                }
                res = self.state.client_rx.next() => {
                    let (req, resp_tx) = res.expect("Client channel closed");
                    self.forward_client_request(req, resp_tx);
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
        resp_tx: oneshot::Sender<RpcResult>,
        timeout: &mut Sleep,
    ) {
        match req {
            RpcRequest::RequestVote(req) => {
                self.state.persistent.update_term(req.term);
                let resp = self.handle_request_vote(req);
                resp_tx
                    .send(Ok(RpcResponse::RequestVote(resp)))
                    .expect("RPC response channel closed");
            }
            RpcRequest::AppendEntries(req) => {
                self.state.persistent.update_term(req.term);
                self.state.leader_id = Some(req.leader_id.clone());
                let resp = self.handle_append_entries(req);
                resp_tx
                    .send(Ok(RpcResponse::AppendEntries(resp)))
                    .expect("RPC response channel closed");

                // Got heartbeat, reset timeout
                *timeout = self.election_timeout();
            }
            RpcRequest::ForwardClientRequest(_) => {
                console_log!("got forwarded request while follower");
                let resp = Err(ClientError::Unavailable);
                resp_tx
                    .send(Ok(RpcResponse::ForwardClientRequest(resp)))
                    .expect("RPC response channel closed");
            }
        }
    }

    fn forward_client_request(&self, req: ClientRequest, resp_tx: oneshot::Sender<ClientResult>) {
        match self.state.leader_id {
            Some(ref leader_id) => {
                if let Some(mut client) = self.state.peer_clients.get(leader_id).cloned() {
                    spawn_local(async move {
                        match client.call(RpcRequest::ForwardClientRequest(req)).await {
                            Ok(RpcResponse::ForwardClientRequest(resp)) => {
                                let _ = resp_tx.send(resp);
                            }
                            Err(err) => {
                                console_log!("error forwarding request: {:?}", err);
                                let _ = resp_tx.send(Err(ClientError::Unavailable));
                            }
                            _ => unreachable!(),
                        }
                    });
                } else {
                    let _ = resp_tx.send(Err(ClientError::Unavailable));
                }
            }
            None => {
                let _ = resp_tx.send(Err(ClientError::Unavailable));
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
            self.apply_log();
            select! {
                res = votes_rx.next() => {
                    if res.is_some() {
                        votes += 1;
                        if votes >= self.quorum() {
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

    fn handle_rpc(&mut self, req: RpcRequest, resp_tx: oneshot::Sender<RpcResult>) -> StateChange {
        match req {
            RpcRequest::RequestVote(req) => {
                let new_term = self.state.persistent.update_term(req.term);
                let resp = self.handle_request_vote(req);
                resp_tx
                    .send(Ok(RpcResponse::RequestVote(resp)))
                    .expect("RPC response channel closed");
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
                let resp = self.handle_append_entries(req);
                resp_tx
                    .send(Ok(RpcResponse::AppendEntries(resp)))
                    .expect("RPC response channel closed");
                // Got a heartbeat, so we lose the election
                console_log!("I LOSE!");
                StateChange::BecomeFollower
            }
            RpcRequest::ForwardClientRequest(_) => {
                console_log!("got forwarded request while candidate");
                let resp = Err(ClientError::Unavailable);
                resp_tx
                    .send(Ok(RpcResponse::ForwardClientRequest(resp)))
                    .expect("RPC response channel closed");
                StateChange::Continue
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

        let mut resps_rx = self.status.responses_rx.take().unwrap();
        let mut debug_interval = interval(Duration::from_secs(1));

        // Initial heartbeat happens immediately
        let mut heartbeat = sleep(Duration::from_millis(0));
        loop {
            self.advance_commit_index();
            self.apply_log();
            select! {
                res = resps_rx.next() => {
                    let (peer_id, idx, resp) = res.expect("response channel closed");
                    if let StateChange::BecomeFollower = self.handle_append_entries_response(peer_id, idx, resp) {
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
                    self.handle_client_request(req, resp_tx);
                }
                _ = heartbeat => {
                    for peer in &self.state.peers {
                        self.append_entries(peer.to_string());
                    }
                    heartbeat = self.heartbeat_timeout();
                }
                _ = debug_interval.next() => {
                    self.send_debug();
                }
            }
        }
    }

    fn advance_commit_index(&mut self) {
        let next_commit_index = self.state.commit_index + 1;
        for n in next_commit_index..=self.state.persistent.last_log_index() {
            let agree = self
                .status
                .match_indices
                .iter()
                .filter(|(_, &idx)| idx >= n)
                .count();
            if agree < self.quorum() {
                break;
            }
            if self.state.persistent.get_log(n).unwrap().term
                != self.state.persistent.current_term()
            {
                break;
            }
            self.state.commit_index = n;
        }

        for idx in next_commit_index..=self.state.commit_index {
            if let Some(tx) = self.status.in_flight.remove(&idx) {
                let _ = tx.send(Ok(ClientResponse::Ack));
            }
        }
    }

    fn append_entries(&self, peer_id: NodeId) {
        let mut client = self.state.peer_clients.get(&peer_id).unwrap().clone();
        if !client.is_connected() {
            return;
        }

        let next_index = *self.status.next_indices.get(&peer_id).unwrap();
        let prev_log_index = next_index - 1;
        let prev_log_term = if prev_log_index > 0 {
            self.state.persistent.get_log(prev_log_index).unwrap().term
        } else {
            0
        };
        let last_entry = min(self.state.persistent.last_log_index(), next_index);
        let entries = self.state.persistent.sublog(next_index..=last_entry);
        let req = RpcRequest::AppendEntries(AppendEntriesRequest {
            leader_id: self.state.node_id.to_string(),
            term: self.state.persistent.current_term(),
            prev_log_index,
            prev_log_term,
            entries,
            leader_commit: self.state.commit_index,
        });

        let mut tx = self.status.responses_tx.clone();
        spawn_local(async move {
            let resp = client.call(req).await;
            let _ = tx.send((peer_id, last_entry, resp)).await;
        });
    }

    fn handle_append_entries_response(
        &mut self,
        peer_id: NodeId,
        idx: TermIndex,
        res: RpcResult,
    ) -> StateChange {
        match res {
            Err(transport::Error::Disconnected) => {
                console_log!("peer {} is disconnected", &peer_id);
                // No point retrying with a disconnected client (we'll try again
                // in the next heartbeat)
                StateChange::Continue
            }
            Err(err) => {
                console_log!("error in append entries response for {}: {}", &peer_id, err);
                console_log!("retrying...");
                self.append_entries(peer_id);
                StateChange::Continue
            }
            Ok(RpcResponse::AppendEntries(resp)) => {
                if self.state.persistent.update_term(resp.term) {
                    // Relinquish leadership since there's a higher term out
                    // there
                    return StateChange::BecomeFollower;
                }
                if resp.term < self.state.persistent.current_term() {
                    console_log!("received response from old term: {:?}", resp);
                    return StateChange::Continue;
                }

                let next = self.status.next_indices.get_mut(&peer_id).unwrap();
                if resp.success {
                    *next = max(*next, idx + 1);
                    let m = self.status.match_indices.get_mut(&peer_id).unwrap();
                    *m = max(*m, idx);
                } else {
                    // Try again with an earlier log
                    *next -= 1;
                    self.append_entries(peer_id);
                }
                StateChange::Continue
            }
            _ => unreachable!(),
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
                    let resp = self.handle_append_entries(req);
                    resp_tx
                        .send(Ok(RpcResponse::AppendEntries(resp)))
                        .expect("response channel closed");
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
                    let resp = self.handle_request_vote(req);
                    resp_tx
                        .send(Ok(RpcResponse::RequestVote(resp)))
                        .expect("response channel closed");
                    return StateChange::BecomeFollower;
                } else {
                    let resp = RpcResponse::RequestVote(RequestVoteResponse {
                        term,
                        vote_granted: false,
                    });
                    resp_tx.send(Ok(resp)).expect("response channel closed");
                }
            }
            RpcRequest::ForwardClientRequest(req) => {
                let (tx, rx) = oneshot::channel();
                spawn_local(async move {
                    if let Ok(resp) = rx.await {
                        let _ = resp_tx.send(Ok(RpcResponse::ForwardClientRequest(resp)));
                    }
                });
                self.handle_client_request(req, tx);
            }
        }

        StateChange::Continue
    }

    fn handle_client_request(
        &mut self,
        req: ClientRequest,
        resp_tx: oneshot::Sender<Result<ClientResponse, ClientError>>,
    ) {
        match req {
            ClientRequest::Get(ref key) => self.handle_get_request(key, resp_tx),
            ClientRequest::Set(ref key, ref val) => self.handle_set_request(key, val, resp_tx),
        }
    }

    fn handle_get_request(
        &self,
        key: &str,
        client_resp_tx: oneshot::Sender<Result<ClientResponse, ClientError>>,
    ) {
        let val = self.state.current_state.get(key).cloned();
        let resp = ClientResponse::Get(val);
        let _ = client_resp_tx.send(Ok(resp));
    }

    fn handle_set_request(&mut self, key: &str, val: &str, resp_tx: oneshot::Sender<ClientResult>) {
        let cmd = LogCmd::Set {
            key: key.to_string(),
            data: val.to_string(),
        };
        let entry = self.state.persistent.append_log(cmd);

        self.status.in_flight.insert(entry.idx, resp_tx);
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
        let next_indices = from
            .state
            .peers
            .iter()
            .map(|p| (p.to_string(), from.state.persistent.last_log_index() + 1))
            .collect();
        let match_indices = from
            .state
            .peers
            .iter()
            .map(|p| (p.to_string(), 0))
            .collect();
        let (responses_tx, responses_rx) = channel(100);
        let mut next = Self {
            state: from.state,
            status: Leader {
                next_indices,
                match_indices,
                responses_tx,
                responses_rx: Some(responses_rx),
                in_flight: HashMap::new(),
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
            peers: from.state.peers.clone(),
            online_peers: from
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
            // TODO: This is way too expensive
            current_state: from.state.current_state.clone(),
        }
    }
}
