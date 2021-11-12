use crate::raft::{RpcMessage, RpcRequest, RpcResponse};
use crate::webrtc_rpc::transport::{self, RequestHandler};
use async_trait::async_trait;
use futures::channel::mpsc::Sender;
use futures::channel::oneshot;
use futures::SinkExt;
use std::marker::Send;

#[derive(Debug)]
pub struct RpcServer<Cmd> {
    tx: Sender<RpcMessage<Cmd>>,
}

impl<Cmd> RpcServer<Cmd>
where
    Cmd: Send,
{
    pub fn new(tx: Sender<RpcMessage<Cmd>>) -> Self {
        Self { tx }
    }
}

#[async_trait]
impl<Cmd> RequestHandler<RpcRequest<Cmd>, RpcResponse<Cmd>> for RpcServer<Cmd>
where
    Cmd: Send,
{
    async fn handle(&self, req: RpcRequest<Cmd>) -> Result<RpcResponse<Cmd>, transport::Error> {
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
