use crate::webrtc_rpc::transport;
use crate::raft::persistence::DBError;

#[derive(Debug)]
pub enum Error {
    TransportError(transport::Error),
    NotEnoughPeers(),
    DatabaseError(DBError),
}

impl From<DBError> for Error {
    fn from(err: DBError) -> Self {
        Error::DatabaseError(err)
    }
}
