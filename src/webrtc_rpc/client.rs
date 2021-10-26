use std::collections::HashMap;
use std::marker::PhantomData;
use web_sys::{RtcDataChannel, RtcPeerConnection};

#[derive(Debug)]
pub struct Peer<T> {
    pub node_id: String,
    connection: RtcPeerConnection,
    data_channel: RtcDataChannel,
    _message: PhantomData<T>,
}

#[derive(Debug)]
pub struct Client<T> {
    peers: HashMap<String, Peer<T>>,
}

impl<T> Peer<T> {
    pub fn new(
        node_id: String,
        connection: RtcPeerConnection,
        data_channel: RtcDataChannel,
    ) -> Self {
        Self {
            node_id,
            connection,
            data_channel,
            _message: PhantomData,
        }
    }
}

impl<T> Client<T> {
    pub fn new(peers: HashMap<String, Peer<T>>) -> Self {
        Self {
            peers,
        }
    }
}
