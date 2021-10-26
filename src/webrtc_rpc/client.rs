use std::collections::HashMap;
use std::marker::PhantomData;
use web_sys::{RtcDataChannel, RtcPeerConnection};

#[derive(Debug)]
pub struct Peer<Req, Resp> {
    pub node_id: String,
    connection: RtcPeerConnection,
    data_channel: RtcDataChannel,
    _request: PhantomData<Req>,
    _response: PhantomData<Resp>,
}

#[derive(Debug)]
pub struct Client<Req, Resp> {
    peers: HashMap<String, Peer<Req, Resp>>,
}

impl<Req, Resp> Peer<Req, Resp> {
    pub fn new(
        node_id: String,
        connection: RtcPeerConnection,
        data_channel: RtcDataChannel,
    ) -> Self {
        Self {
            node_id,
            connection,
            data_channel,
            _request: PhantomData,
            _response: PhantomData,
        }
    }
}

impl<Req, Resp> Client<Req, Resp> {
    pub fn new(peers: HashMap<String, Peer<Req, Resp>>) -> Self {
        Self {
            peers,
        }
    }
}
