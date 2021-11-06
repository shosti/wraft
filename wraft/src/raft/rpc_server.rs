use crate::raft::{RpcMessage, RpcRequest, RpcResponse};
use crate::webrtc_rpc::transport::{self, RequestHandler};
use async_trait::async_trait;
use futures::channel::mpsc::Sender;
use futures::channel::oneshot;
use futures::SinkExt;
use std::marker::Send;

#[derive(Clone, Debug)]
pub struct RpcServer<T> {
    tx: Sender<RpcMessage<T>>,
}

impl<T> RpcServer<T>
where
    T: Send,
{
    pub fn new(tx: Sender<RpcMessage<T>>) -> Self {
        Self { tx }
    }
}

#[async_trait]
impl<T> RequestHandler<RpcRequest<T>, RpcResponse<T>> for RpcServer<T>
where
    T: Send,
{
    async fn handle(&self, req: RpcRequest<T>) -> Result<RpcResponse<T>, transport::Error> {
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
