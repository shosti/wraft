use std::collections::HashMap;
use web_sys::{RtcDataChannel, RtcPeerConnection};

#[derive(Debug)]
pub struct Peer {
    connection: RtcPeerConnection,
    data_channel: RtcDataChannel,
}

#[derive(Debug)]
pub struct Client {
    peers: HashMap<String, Peer>,
}

impl Peer {
    pub fn new(connection: RtcPeerConnection, data_channel: RtcDataChannel) -> Self {
        Self {
            connection,
            data_channel,
        }
    }
}

impl Client {
    pub fn new(peers: HashMap<String, Peer>) -> Self {
        Self { peers }
    }
}
