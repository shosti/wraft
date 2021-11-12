use crate::{
    raft::{errors::ClientError, ClientMessage, ClientRequest, ClientResponse, RaftStateDump},
    util::sleep,
};
use futures::{
    channel::{mpsc::Sender, oneshot},
    select, SinkExt,
};
use std::time::Duration;

const CLIENT_REQUEST_TIMEOUT_MILLIS: u64 = 2000;

pub struct Client<Cmd> {
    client_tx: Sender<ClientMessage<Cmd>>,
}

impl<Cmd> Client<Cmd> {
    pub fn new(client_tx: Sender<ClientMessage<Cmd>>) -> Self {
        Self { client_tx }
    }

    pub async fn send(&self, val: Cmd) -> Result<(), ClientError> {
        let req = ClientRequest::Apply(val);
        self.do_client_request(req).await.map(|_| ())
    }

    pub async fn debug(&self) -> Result<Box<RaftStateDump>, ClientError> {
        let req = ClientRequest::Debug;
        match self.do_client_request(req).await {
            Ok(ClientResponse::Debug(debug)) => Ok(debug),
            Err(err) => Err(err),
            _ => unreachable!(),
        }
    }

    async fn do_client_request(
        &self,
        req: ClientRequest<Cmd>,
    ) -> Result<ClientResponse<Cmd>, ClientError> {
        let (resp_tx, mut resp_rx) = oneshot::channel();
        let mut tx = self.client_tx.clone();
        tx.send((req, resp_tx))
            .await
            .map_err(|_| ClientError::Unavailable)?;
        select! {
            res = resp_rx => {
                match res {
                    Ok(Ok(resp)) => Ok(resp),
                    Ok(Err(err)) => Err(err),
                    Err(_) => Err(ClientError::Unavailable),
                }
            }
            _ = sleep(Duration::from_millis(CLIENT_REQUEST_TIMEOUT_MILLIS)) => {
                Err(ClientError::Timeout)
            }
        }
    }
}

impl<Cmd> Clone for Client<Cmd> {
    fn clone(&self) -> Self {
        Self {
            client_tx: self.client_tx.clone(),
        }
    }
}
