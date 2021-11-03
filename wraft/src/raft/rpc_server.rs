use crate::raft::{RpcMessage, RpcRequest, RpcResponse};
use crate::webrtc_rpc::transport::{self, RequestHandler};
use async_trait::async_trait;
use futures::channel::mpsc::Sender;
use futures::channel::oneshot;
use futures::SinkExt;

#[derive(Clone, Debug)]
pub struct RpcServer {
    tx: Sender<RpcMessage>,
}

impl RpcServer {
    pub fn new(tx: Sender<RpcMessage>) -> Self {
        Self { tx }
    }
}

#[async_trait]
impl RequestHandler<RpcRequest, RpcResponse> for RpcServer {
    async fn handle(&self, req: RpcRequest) -> Result<RpcResponse, transport::Error> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .clone()
            .send((req, resp_tx))
            .await
            .expect("request channel closed");

        match resp_rx.await {
            Ok(resp) => resp,
            // If the response channel is closed, we probably changed state
            // mid-request so there's no reasonable response
            Err(_) => Err(transport::Error::Unavailable),
        }
    }
}
