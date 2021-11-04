use crate::console_log;
use crate::raft::persistence::PersistentState;
use crate::raft::{
    AppendEntriesRequest, AppendEntriesResponse, ClientMessage, ClientResponse, LogEntry, LogIndex,
    NodeId, RequestVoteRequest, RequestVoteResponse, RpcMessage, RpcRequest, RpcResponse,
};
use crate::util::{sleep, Sleep};
use crate::webrtc_rpc::transport;
use crate::webrtc_rpc::transport::Client;
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

enum RaftWorkerWrapper {
    Follower(RaftWorker<Follower>),
    Candidate(RaftWorker<Candidate>),
    Leader(RaftWorker<Leader>),
}

struct RaftWorker<S> {
    state: RaftState,
    _status: S,
}

struct Follower {}
struct Candidate {}
struct Leader {}

type RpcClient = Client<RpcRequest, RpcResponse>;

enum StateChange {
    Continue,
    BecomeFollower,
}

#[derive(Debug)]
struct RaftState {
    leader_id: Option<NodeId>,
    node_id: NodeId,
    session_key: String,
    cluster_size: usize,
    peer_clients: HashMap<NodeId, RpcClient>,
    client_rx: Receiver<ClientMessage>,
    rpc_rx: Receiver<RpcMessage>,

    // Volatile state
    commit_index: u64,
    last_applied: u64,

    // Persistent state
    persistent: PersistentState,
}

impl RaftState {}

pub async fn run(
    node_id: NodeId,
    session_key: String,
    rpc_rx: Receiver<RpcMessage>,
    client_rx: Receiver<ClientMessage>,
    peer_clients: HashMap<NodeId, RpcClient>,
) {
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
        commit_index: 0,
        last_applied: 0,
    };

    let mut worker = RaftWorkerWrapper::Follower(RaftWorker::new(state));
    loop {
        worker = worker.next().await;
    }
}

impl RaftWorkerWrapper {
    async fn next(self) -> Self {
        match self {
            RaftWorkerWrapper::Follower(worker) => worker.next().await,
            RaftWorkerWrapper::Candidate(worker) => worker.next().await,
            RaftWorkerWrapper::Leader(worker) => worker.next().await,
        }
    }
}

impl<S> RaftWorker<S> {
    fn election_timeout(&self) -> Sleep {
        let delay = thread_rng().gen_range(150..300);
        sleep(Duration::from_millis(delay))
    }

    fn votes_required(&self) -> usize {
        (self.state.cluster_size / 2) + 1
    }

    fn handle_request_vote(&self, req: &RequestVoteRequest) -> RpcResponse {
        let voted_for = self.state.persistent.voted_for();
        let current_term = self.state.persistent.current_term();
        if req.term >= current_term
            && (voted_for == None || *voted_for.unwrap() == req.candidate)
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
}

impl RaftWorker<Follower> {
    pub fn new(state: RaftState) -> Self {
        Self {
            state,
            _status: Follower {},
        }
    }

    async fn next(mut self) -> RaftWorkerWrapper {
        console_log!("BEING A FOLLOWER");
        let mut timeout = self.election_timeout();

        loop {
            select! {
                res = self.state.rpc_rx.next() => {
                    let (req, resp_tx) = res.expect("RPC channel closed");
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
                            timeout = self.election_timeout();
                        }
                    }
                }
                res = self.state.client_rx.next() => {
                    let (req, _resp_tx) = res.expect("Client channel closed");
                    console_log!("WOULD FORWARD REQUEST {:?} TO {:?}", req, self.state.leader_id);
                }
                _ = timeout => {
                    console_log!("Calling election!");
                    return RaftWorkerWrapper::Candidate(self.into());
                }
            }
        }
    }
}

impl RaftWorker<Candidate> {
    async fn next(mut self) -> RaftWorkerWrapper {
        console_log!("BEING A CANDIDATE");
        self.state.persistent.increment_term();
        self.vote_for_self();
        let mut votes = 1; // Voted for self

        let mut votes_rx = self.request_votes();
        let mut timeout = self.election_timeout();
        loop {
            select! {
                res = votes_rx.next() => {
                    res.expect("votes channel closed");
                    votes += 1;
                    if votes >= self.votes_required() {
                        console_log!("I WIN!!!");
                        return RaftWorkerWrapper::Leader(self.into());
                    }
                }
                res = self.state.rpc_rx.next() => {
                    let (req, resp_tx) = res.expect("RPC channel closed");
                    if let StateChange::BecomeFollower = self.handle_rpc(&req, resp_tx) {
                        return RaftWorkerWrapper::Follower(self.into());
                    }
                }
                _ = timeout => {
                    console_log!("ELECTION TIMED OUT");
                    return RaftWorkerWrapper::Candidate(self);
                }
            }
        }
    }

    fn vote_for_self(&self) {
        self.state
            .persistent
            .set_voted_for(Some(&self.state.node_id));
    }

    fn handle_rpc(
        &self,
        req: &RpcRequest,
        resp_tx: oneshot::Sender<Result<RpcResponse, transport::Error>>,
    ) -> StateChange {
        match req {
            RpcRequest::RequestVote(req) => {
                let new_term = self.state.persistent.update_term(req.term);
                let resp = self.handle_request_vote(req);
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
                let resp = self.handle_append_entries(req);
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
        for client in self.state.peer_clients.values() {
            let c = client.clone();
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
    async fn next(mut self) -> RaftWorkerWrapper {
        console_log!("BEING A LEADER");

        let term = self.state.persistent.current_term();
        let mut _next_indices: HashMap<NodeId, LogIndex> = HashMap::new();
        let mut _match_indices: HashMap<NodeId, LogIndex> = HashMap::new();
        let (resps_tx, mut resps_rx) = channel::<Result<RpcResponse, transport::Error>>(100);

        let heartbeat_interval = Duration::from_millis(HEARBEAT_INTERVAL_MILLIS);
        let mut heartbeat = sleep(heartbeat_interval);
        loop {
            select! {
                res = resps_rx.next() => {
                    match res.expect("response channel closed") {
                        Ok(RpcResponse::AppendEntries(resp)) => {
                            if self.state.persistent.update_term(resp.term) {
                                // A higher term is out there; switch to being a
                                // follower
                                return RaftWorkerWrapper::Follower(self.into());
                            }
                            if resp.success {
                                console_log!("SUCCESSFUL THING: {:?}", resp);
                            } else {
                                console_log!("UNSUCCESSFUL THING: {:?}", resp);
                            }
                        }
                        Err(err) => {
                            console_log!("ERROR!!! {:?}", err);
                        }
                        _ => unreachable!(),
                    }
                }
                res = self.state.rpc_rx.next() => {
                    let (req, resp_tx) = res.expect("RPC channel closed");
                    match req {
                        RpcRequest::AppendEntries(req) => {
                            if self.state.persistent.update_term(req.term) {
                                let resp = self.handle_append_entries(&req);
                                resp_tx.send(Ok(resp)).expect("response channel closed");
                                return RaftWorkerWrapper::Follower(self.into());
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
                                return RaftWorkerWrapper::Follower(self.into());
                            } else {
                                let resp = RpcResponse::RequestVote(RequestVoteResponse {
                                    term,
                                    vote_granted: false,
                                });
                                resp_tx.send(Ok(resp)).expect("response channel closed");
                            }
                        }
                    }
                }
                res = self.state.client_rx.next() => {
                    let (req, resp_tx) = res.expect("client channel closed");
                    console_log!("Doing something with: {:?}", req);
                    resp_tx.send(Ok(ClientResponse::Ack)).expect("client response closed");
                    // We sent something, so reset the heartbeat timeout
                    heartbeat = sleep(heartbeat_interval);
                }
                _ = heartbeat => {
                    let empty_entries = Vec::new();
                    for client in self.state.peer_clients.values() {
                        self.send_append_entries(
                            empty_entries.clone(),
                            client.clone(),
                            term,
                            self.state.node_id.clone(),
                            resps_tx.clone()
                        );
                    }
                    heartbeat = sleep(heartbeat_interval);
                }
            }
        }
    }

    fn send_append_entries(
        &self,
        entries: Vec<LogEntry>,
        client: RpcClient,
        term: u64,
        leader_id: NodeId,
        mut resps_tx: Sender<Result<RpcResponse, transport::Error>>,
    ) {
        let req = RpcRequest::AppendEntries(AppendEntriesRequest {
            term,
            leader_id,
            // TODO: fix all of these
            leader_commit: 0,
            prev_log_index: 0,
            prev_log_term: 0,
            entries,
        });
        spawn_local(async move {
            let resp = client.call(req).await;
            let _ = resps_tx.send(resp).await;
        });
    }
}

// State machine transitions
impl From<RaftWorker<Candidate>> for RaftWorker<Follower> {
    fn from(from: RaftWorker<Candidate>) -> RaftWorker<Follower> {
        RaftWorker {
            state: from.state,
            _status: Follower {},
        }
    }
}

impl From<RaftWorker<Leader>> for RaftWorker<Follower> {
    fn from(from: RaftWorker<Leader>) -> Self {
        Self {
            state: from.state,
            _status: Follower {},
        }
    }
}

impl From<RaftWorker<Follower>> for RaftWorker<Candidate> {
    fn from(from: RaftWorker<Follower>) -> Self {
        Self {
            state: from.state,
            _status: Candidate {},
        }
    }
}

impl From<RaftWorker<Candidate>> for RaftWorker<Leader> {
    fn from(from: RaftWorker<Candidate>) -> Self {
        Self {
            state: from.state,
            _status: Leader {},
        }
    }
}
