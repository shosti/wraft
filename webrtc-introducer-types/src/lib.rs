use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum Command {
    Join(Join),
    SessionStatus(Session),
    Offer(Offer),
    Answer(Offer),
    IceCandidate(IceCandidate),
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Join {
    pub node_id: u64,
    pub session_id: u128,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Session {
    pub session_id: u128,
    pub online: HashSet<u64>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Offer {
    pub session_id: u128,
    pub node_id: u64,
    pub target_id: u64,
    pub sdp_data: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct IceCandidate {
    pub session_id: u128,
    pub node_id: u64,
    pub target_id: u64,
    pub candidate: String,
    pub sdp_mid: Option<String>,
}
