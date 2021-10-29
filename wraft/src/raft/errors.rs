use crate::webrtc_rpc::transport;

#[derive(Debug)]
pub enum Error {
    TransportError(transport::Error),
    NotEnoughPeers(),
    DatabaseError(String),
}
