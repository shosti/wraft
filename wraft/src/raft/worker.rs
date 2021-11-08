use crate::console_log;
use crate::raft::rpc_server::RpcServer;
use crate::raft::storage::Storage;
use crate::raft::{
    AppendEntriesRequest, AppendEntriesResponse, ClientError, ClientMessage, ClientRequest,
    ClientResponse, LogCmd, LogEntry, LogIndex, NodeId, RequestVoteRequest, RequestVoteResponse,
    RpcMessage, RpcRequest, RpcResponse, TermIndex,
};
use crate::util::{sleep, Sleep};
use crate::webrtc_rpc::transport::{self, Client, PeerTransport};
use futures::channel::mpsc::{channel, Receiver, Sender};
use futures::channel::oneshot;
use futures::select;
use futures::sink::SinkExt;
use futures::stream::StreamExt;
use rand::{thread_rng, Rng};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::cmp::max;
use std::collections::HashMap;
use std::fmt::Debug;
use std::time::Duration;
use wasm_bindgen_futures::spawn_local;

const HEARBEAT_INTERVAL_MILLIS: u64 = 100;

enum RaftWorkerState<T> {
    Follower(RaftWorker<Follower, T>),
    Candidate(RaftWorker<Candidate, T>),
    Leader(RaftWorker<Leader<T>, T>),
}

struct RaftWorker<S, T> {
    inner: RaftWorkerInner<T>,
    state: S,
}

#[derive(Debug)]
struct Follower {}
#[derive(Debug)]
struct Candidate {}
#[derive(Debug)]
struct Leader<T> {
    next_indices: HashMap<NodeId, LogIndex>,
    match_indices: HashMap<NodeId, LogIndex>,
    in_flight: HashMap<LogIndex, oneshot::Sender<ClientResult<T>>>,
    responses_tx: Sender<(NodeId, LogIndex, RpcResult<T>)>,
    responses_rx: Option<Receiver<(NodeId, LogIndex, RpcResult<T>)>>,
}

type RpcClient<T> = Client<RpcRequest<T>, RpcResponse<T>>;
type RpcResult<T> = Result<RpcResponse<T>, transport::Error>;
type ClientResult<T> = Result<ClientResponse<T>, ClientError>;

enum StateChange {
    Continue,
    BecomeFollower,
}

struct RaftWorkerInner<T> {
    leader_id: Option<NodeId>,
    node_id: NodeId,
    session_key: u128,
    cluster_size: usize,
    client_rx: Receiver<ClientMessage<T>>,
    rpc_rx: Receiver<RpcMessage<T>>,
    rpc_server: RpcServer<T>,
    peers_rx: Receiver<PeerTransport>,
    current_state: HashMap<String, T>,

    // Peers is constant throughout the runtime (regardless of whether they're
    // online or not)
    peers: Vec<NodeId>,
    // Peer clients could connect/disconnect
    peer_clients: HashMap<NodeId, RpcClient<T>>,

    // Volatile state
    commit_index: LogIndex,
    last_applied: LogIndex,

    // Persistent state
    storage: Storage<T>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct RaftDebugState<T> {
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
    current_state: HashMap<String, T>,
}

impl<T> RaftWorkerState<T>
where
    T: Serialize + DeserializeOwned + Clone + Debug + Send + 'static,
{
    async fn next(self) -> Self {
        match self {
            RaftWorkerState::Follower(worker) => worker.next().await,
            RaftWorkerState::Candidate(worker) => worker.next().await,
            RaftWorkerState::Leader(worker) => worker.next().await,
        }
    }
}

pub fn run<T>(
    node_id: NodeId,
    session_key: u128,
    rpc_rx: Receiver<RpcMessage<T>>,
    peers_rx: Receiver<PeerTransport>,
    peer_clients: HashMap<NodeId, RpcClient<T>>,
    rpc_server: RpcServer<T>,
) -> Sender<ClientMessage<T>>
where
    T: Serialize + DeserializeOwned + Clone + Debug + Send + 'static,
{
    let (client_tx, client_rx) = channel(100);

    let cluster_size = peer_clients.len() + 1;
    let storage = Storage::new(session_key);
    let peers = peer_clients.keys().cloned().collect();
    let inner = RaftWorkerInner {
        leader_id: None,
        storage,
        node_id,
        session_key,
        cluster_size,
        peer_clients,
        peers,
        rpc_rx,
        client_rx,
        peers_rx,
        rpc_server,
        current_state: HashMap::new(),
        commit_index: 0,
        last_applied: 0,
    };

    spawn_local(async move {
        let mut worker = RaftWorkerState::Follower(RaftWorker::new(inner));
        loop {
            worker = worker.next().await;
        }
    });

    client_tx
}

impl<S, T> RaftWorker<S, T>
where
    S: std::fmt::Debug,
    T: Serialize + DeserializeOwned + Clone + Debug + Send + 'static,
{
    fn election_timeout(&self) -> Sleep {
        let delay = thread_rng().gen_range(500..1000);
        sleep(Duration::from_millis(delay))
    }

    fn quorum(&self) -> usize {
        (self.inner.cluster_size / 2) + 1
    }

    fn handle_request_vote(&mut self, req: RequestVoteRequest) -> RequestVoteResponse {
        let voted_for = self.inner.storage.voted_for();
        let current_term = self.inner.storage.current_term();
        if req.term >= current_term
            && (voted_for.is_none() || *voted_for.as_ref().unwrap() == req.candidate)
            && req.last_log_index >= self.inner.commit_index
        {
            self.inner.storage.set_voted_for(Some(req.candidate));

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

    fn handle_append_entries(&mut self, req: AppendEntriesRequest<T>) -> AppendEntriesResponse {
        let current_term = self.inner.storage.current_term();
        if req.term < current_term {
            return AppendEntriesResponse {
                term: current_term,
                success: false,
            };
        }
        let entry = self.inner.storage.get_log(req.prev_log_index);
        if req.prev_log_index != 0 && (entry.is_none() || entry.unwrap().term != req.prev_log_term)
        {
            return AppendEntriesResponse {
                term: current_term,
                success: false,
            };
        }
        for e in &req.entries {
            if let Some(existing) = self.inner.storage.get_log(e.idx) {
                if existing.term != e.term {
                    self.inner.storage.overwrite_log(e);
                }
            } else {
                self.inner.storage.append_log(e);
            }
        }

        if req.leader_commit > self.inner.commit_index {
            self.inner.commit_index = req.leader_commit
        }

        AppendEntriesResponse {
            term: current_term,
            success: true,
        }
    }

    fn handle_debug(&self, tx: oneshot::Sender<ClientResult<T>>) {
        let _ = tx.send(Ok(ClientResponse::Debug(Box::new(self.into()))));
    }

    fn apply_log(&mut self) {
        for entry in self
            .inner
            .storage
            .sublog((self.inner.last_applied + 1)..=self.inner.commit_index)
        {
            match entry.cmd {
                LogCmd::Set { key, data } => {
                    self.inner.current_state.insert(key, data);
                }
                LogCmd::Delete { ref key } => {
                    self.inner.current_state.remove(key);
                }
            }
            self.inner.last_applied = entry.idx;
        }
    }

    fn handle_new_peer(&mut self, peer: PeerTransport) {
        let (client, mut server) = peer.start();
        let peer_id = client.node_id();
        console_log!("Got new peer: {}", peer_id);

        let s = self.inner.rpc_server.clone();
        spawn_local(async move {
            server.serve(s).await;
        });
        self.inner.peer_clients.insert(client.node_id(), client);
    }
}

impl<T> RaftWorker<Follower, T>
where
    T: Serialize + DeserializeOwned + Clone + Debug + Send + 'static,
{
    pub fn new(inner: RaftWorkerInner<T>) -> Self {
        Self {
            inner,
            state: Follower {},
        }
    }

    async fn next(mut self) -> RaftWorkerState<T> {
        console_log!("BEING A FOLLOWER");
        let mut timeout = self.election_timeout();

        loop {
            self.apply_log();
            select! {
                res = self.inner.rpc_rx.next() => {
                    let (req, resp_tx) = res.expect("RPC channel closed");
                    self.handle_rpc(req, resp_tx, &mut timeout);
                }
                res = self.inner.client_rx.next() => {
                    let (req, resp_tx) = res.expect("Client channel closed");
                    self.forward_client_request(req, resp_tx);
                }
                res = self.inner.peers_rx.next() => {
                    self.handle_new_peer(res.expect("peer channel closed"));
                }
                _ = timeout => {
                    console_log!("Calling election!");
                    return RaftWorkerState::Candidate(self.into());
                }
            }
        }
    }

    fn handle_rpc(
        &mut self,
        req: RpcRequest<T>,
        resp_tx: oneshot::Sender<RpcResult<T>>,
        timeout: &mut Sleep,
    ) {
        match req {
            RpcRequest::RequestVote(req) => {
                self.inner.storage.update_term(req.term);
                let resp = self.handle_request_vote(req);
                resp_tx
                    .send(Ok(RpcResponse::RequestVote(resp)))
                    .expect("RPC response channel closed");
            }
            RpcRequest::AppendEntries(req) => {
                self.inner.storage.update_term(req.term);
                self.inner.leader_id = Some(req.leader_id);
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

    fn forward_client_request(
        &self,
        req: ClientRequest<T>,
        resp_tx: oneshot::Sender<ClientResult<T>>,
    ) {
        match self.inner.leader_id {
            Some(ref leader_id) => {
                if let Some(mut client) = self.inner.peer_clients.get(leader_id).cloned() {
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

impl<T> RaftWorker<Candidate, T>
where
    T: Serialize + DeserializeOwned + Clone + Debug + Send + 'static,
{
    async fn next(mut self) -> RaftWorkerState<T> {
        console_log!("BEING A CANDIDATE");
        self.inner.storage.increment_term();
        self.vote_for_self();
        let mut votes = 1; // Voted for self

        let mut votes_rx = self.request_votes();
        let mut timeout = self.election_timeout();
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
                res = self.inner.rpc_rx.next() => {
                    let (req, resp_tx) = res.expect("RPC channel closed");
                    if let StateChange::BecomeFollower = self.handle_rpc(req, resp_tx) {
                        return RaftWorkerState::Follower(self.into());
                    }
                }
                _ = timeout => {
                    console_log!("ELECTION TIMED OUT");
                    return RaftWorkerState::Candidate(self);
                }
            }
        }
    }

    fn vote_for_self(&mut self) {
        self.inner.storage.set_voted_for(Some(self.inner.node_id));
    }

    fn handle_rpc(
        &mut self,
        req: RpcRequest<T>,
        resp_tx: oneshot::Sender<RpcResult<T>>,
    ) -> StateChange {
        match req {
            RpcRequest::RequestVote(req) => {
                let new_term = self.inner.storage.update_term(req.term);
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
                self.inner.storage.update_term(req.term);
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
        let (votes_tx, votes_rx) = channel(self.inner.cluster_size);
        let req = RpcRequest::RequestVote(RequestVoteRequest {
            term: self.inner.storage.current_term(),
            candidate: self.inner.node_id,
            last_log_index: self.inner.storage.last_log_index(),
            last_log_term: self.inner.storage.last_log_term(),
        });
        for client in self
            .inner
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

impl<T> RaftWorker<Leader<T>, T>
where
    T: Serialize + DeserializeOwned + Clone + Debug + Send + 'static,
{
    async fn next(mut self) -> RaftWorkerState<T> {
        console_log!("BEING A LEADER");

        let mut resps_rx = self.state.responses_rx.take().unwrap();

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
                res = self.inner.rpc_rx.next() => {
                    let (req, resp_tx) = res.expect("RPC channel closed");
                    if let StateChange::BecomeFollower = self.handle_rpc(req, resp_tx, &mut heartbeat) {
                        return RaftWorkerState::Follower(self.into());
                    }
                }
                res = self.inner.client_rx.next() => {
                    let (req, resp_tx) = res.expect("client channel closed");
                    self.handle_client_request(req, resp_tx, &mut heartbeat);
                }
                _ = heartbeat => {
                    for &peer in &self.inner.peers {
                        self.append_entries(peer);
                    }
                    heartbeat = self.heartbeat_timeout();
                }
            }
        }
    }

    fn advance_commit_index(&mut self) {
        let next_commit_index = self.inner.commit_index + 1;
        for n in next_commit_index..=self.inner.storage.last_log_index() {
            let agree = self
                .state
                .match_indices
                .iter()
                .filter(|(_, &idx)| idx >= n)
                .count();
            // We implicitly agree, so we need (quorum - 1) peers to
            // agree in order to commit
            if agree < self.quorum() - 1 {
                break;
            }
            if self.inner.storage.get_log(n).unwrap().term != self.inner.storage.current_term() {
                break;
            }
            self.inner.commit_index = n;
        }

        for idx in next_commit_index..=self.inner.commit_index {
            if let Some(tx) = self.state.in_flight.remove(&idx) {
                let _ = tx.send(Ok(ClientResponse::Ack));
            }
        }
    }

    fn append_entries(&self, peer_id: NodeId) {
        const MAX_BATCH_SIZE: u64 = 10;

        let mut client = self.inner.peer_clients.get(&peer_id).unwrap().clone();
        if !client.is_connected() {
            return;
        }

        let next_index = *self.state.next_indices.get(&peer_id).unwrap();
        let prev_log_index = next_index - 1;
        let prev_log_term = if prev_log_index > 0 {
            self.inner.storage.get_log(prev_log_index).unwrap().term
        } else {
            0
        };
        let entries = self
            .inner
            .storage
            .sublog(next_index..(next_index + MAX_BATCH_SIZE));
        let last_entry = entries
            .last()
            .map_or(self.inner.storage.last_log_index(), |e| e.idx);
        let req = RpcRequest::AppendEntries(AppendEntriesRequest {
            leader_id: self.inner.node_id,
            term: self.inner.storage.current_term(),
            prev_log_index,
            prev_log_term,
            entries,
            leader_commit: self.inner.commit_index,
        });

        let mut tx = self.state.responses_tx.clone();
        spawn_local(async move {
            let resp = client.call(req).await;
            let _ = tx.send((peer_id, last_entry, resp)).await;
        });
    }

    fn handle_append_entries_response(
        &mut self,
        peer_id: NodeId,
        idx: LogIndex,
        res: RpcResult<T>,
    ) -> StateChange {
        match res {
            Err(err) => {
                console_log!("error in append entries response for {}: {}", &peer_id, err);
                console_log!("retrying...");
                StateChange::Continue
            }
            Ok(RpcResponse::AppendEntries(resp)) => {
                if self.inner.storage.update_term(resp.term) {
                    // Relinquish leadership since there's a higher term out
                    // there
                    return StateChange::BecomeFollower;
                }
                if resp.term < self.inner.storage.current_term() {
                    console_log!("received response from old term: {:?}", resp);
                    return StateChange::Continue;
                }

                let next = self.state.next_indices.get_mut(&peer_id).unwrap();
                if resp.success {
                    *next = max(*next, idx + 1);
                    let m = self.state.match_indices.get_mut(&peer_id).unwrap();
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
        req: RpcRequest<T>,
        resp_tx: oneshot::Sender<Result<RpcResponse<T>, transport::Error>>,
        heartbeat: &mut Sleep,
    ) -> StateChange {
        let term = self.inner.storage.current_term();
        match req {
            RpcRequest::AppendEntries(req) => {
                if self.inner.storage.update_term(req.term) {
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
                if self.inner.storage.update_term(req.term) {
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
                self.handle_client_request(req, tx, heartbeat);
            }
        }

        StateChange::Continue
    }

    fn handle_client_request(
        &mut self,
        req: ClientRequest<T>,
        resp_tx: oneshot::Sender<Result<ClientResponse<T>, ClientError>>,
        heartbeat: &mut Sleep,
    ) {
        match req {
            ClientRequest::Get(ref key) => self.handle_get_request(key, resp_tx),
            ClientRequest::Set(ref key, val) => {
                self.handle_set_request(key, val, resp_tx);
                // Reset the heartbeat since we presumably contacted all peers
                *heartbeat = self.heartbeat_timeout();
            }
            ClientRequest::Delete(ref key) => self.handle_delete_request(key, resp_tx),
            ClientRequest::Debug => self.handle_debug(resp_tx),
        }
    }

    fn handle_get_request(
        &self,
        key: &str,
        client_resp_tx: oneshot::Sender<Result<ClientResponse<T>, ClientError>>,
    ) {
        let val = self.inner.current_state.get(key).cloned();
        let resp = ClientResponse::Get(val);
        let _ = client_resp_tx.send(Ok(resp));
    }

    fn handle_set_request(&mut self, key: &str, val: T, resp_tx: oneshot::Sender<ClientResult<T>>) {
        let cmd = LogCmd::Set {
            key: key.to_string(),
            data: val,
        };
        self.handle_log_update(cmd, resp_tx)
    }

    fn handle_delete_request(&mut self, key: &str, resp_tx: oneshot::Sender<ClientResult<T>>) {
        let cmd = LogCmd::Delete { key: key.to_string() };
        self.handle_log_update(cmd, resp_tx)
    }

    fn handle_log_update(&mut self, cmd: LogCmd<T>, resp_tx: oneshot::Sender<ClientResult<T>>) {
        let idx = self.inner.storage.last_log_index() + 1;
        let entry = LogEntry {
            cmd,
            idx,
            term: self.inner.storage.current_term(),
        };
        self.inner.storage.append_log(&entry);

        self.state.in_flight.insert(idx, resp_tx);

        for &p in &self.inner.peers {
            self.append_entries(p)
        }
    }
}

// State machine transitions
impl<T> From<RaftWorker<Candidate, T>> for RaftWorker<Follower, T> {
    fn from(from: RaftWorker<Candidate, T>) -> Self {
        Self {
            inner: from.inner,
            state: Follower {},
        }
    }
}

impl<T> From<RaftWorker<Leader<T>, T>> for RaftWorker<Follower, T> {
    fn from(from: RaftWorker<Leader<T>, T>) -> Self {
        Self {
            inner: from.inner,
            state: Follower {},
        }
    }
}

impl<T> From<RaftWorker<Follower, T>> for RaftWorker<Candidate, T> {
    fn from(from: RaftWorker<Follower, T>) -> Self {
        let mut next = Self {
            inner: from.inner,
            state: Candidate {},
        };
        next.inner.leader_id = None;
        next
    }
}

impl<T> From<RaftWorker<Candidate, T>> for RaftWorker<Leader<T>, T>
where
    T: Serialize + DeserializeOwned + Clone + Debug + Send + 'static,
{
    fn from(from: RaftWorker<Candidate, T>) -> Self {
        let next_indices = from
            .inner
            .peers
            .iter()
            .map(|p| (*p, from.inner.storage.last_log_index() + 1))
            .collect();
        let match_indices = from.inner.peers.iter().map(|p| (*p, 0)).collect();
        let (responses_tx, responses_rx) = channel(100);
        let mut next = Self {
            inner: from.inner,
            state: Leader {
                next_indices,
                match_indices,
                responses_tx,
                responses_rx: Some(responses_rx),
                in_flight: HashMap::new(),
            },
        };
        next.inner.leader_id = Some(next.inner.node_id);
        next
    }
}

impl<S, T> From<&RaftWorker<S, T>> for RaftDebugState<T>
where
    S: std::fmt::Debug,
    T: Serialize + DeserializeOwned + Clone + Debug + Send + 'static,
{
    fn from(from: &RaftWorker<S, T>) -> Self {
        Self {
            state: format!("{:?}", from.state),
            leader_id: from.inner.leader_id,
            node_id: from.inner.node_id,
            session_key: from.inner.session_key,
            cluster_size: from.inner.cluster_size,
            peers: from.inner.peers.clone(),
            online_peers: from
                .inner
                .peer_clients
                .iter()
                .filter(|(_, c)| c.is_connected())
                .map(|(k, _)| *k)
                .collect(),
            voted_for: *from.inner.storage.voted_for(),
            current_term: from.inner.storage.current_term(),
            last_log_index: from.inner.storage.last_log_index(),

            commit_index: from.inner.commit_index,
            last_applied: from.inner.last_applied,
            // TODO: This is way too expensive
            current_state: from.inner.current_state.clone(),
        }
    }
}
