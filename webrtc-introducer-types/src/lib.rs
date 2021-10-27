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
    pub node_id: String,
    pub session_id: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Session {
    pub session_id: String,
    pub online: HashSet<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Offer {
    pub session_id: String,
    pub node_id: String,
    pub target_id: String,
    pub sdp_data: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct IceCandidate {
    pub session_id: String,
    pub node_id: String,
    pub target_id: String,
    pub candidate: String,
    pub sdp_mid: Option<String>,
}
