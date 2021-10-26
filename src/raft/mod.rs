use crate::console_log;
use crate::webrtc_rpc::client::Client;
use crate::webrtc_rpc::introduce;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use tarpc::{Request, Response};

pub struct Raft {
    node_id: String,
    session_key: String,
    client: Option<Client<Request<RpcMessage>, Response<RpcMessage>>>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
enum RpcMessage {}

impl Raft {
    pub fn new(node_id: String, session_key: String) -> Self {
        Raft {
            node_id,
            session_key,
            client: None,
        }
    }

    pub async fn run(&mut self) -> Result<()> {
        console_log!("RUNNING!");
        let rtc_client =
            introduce::<Request<RpcMessage>, Response<RpcMessage>>(&self.node_id, &self.session_key)
                .await
                .unwrap();
        console_log!("WE'RE ALL INTRODUCED!");

        self.client = Some(rtc_client);
        Ok(())
    }
}
