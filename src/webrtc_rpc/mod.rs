pub mod client;
mod introduction;
use client::Client;
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::fmt::Debug;

static CLUSTER_SIZE: usize = 3;

pub async fn introduce<Req, Resp>(id: &str, session_id: &str) -> Result<Client<Req, Resp>, error::Error>
where
    Req: DeserializeOwned + Debug + 'static,
    Resp: Serialize + Debug + 'static,
{
    let client = introduction::initiate::<Req, Resp>(id, session_id, CLUSTER_SIZE).await?;
    Ok(client)
}

pub mod error {
    use wasm_bindgen::JsValue;

    #[derive(Debug)]
    pub enum Error {
        Js(JsValue),
        Rust(Box<dyn std::error::Error>),
        String(String),
    }

    impl From<JsValue> for Error {
        fn from(js_val: JsValue) -> Self {
            Error::Js(js_val)
        }
    }

    impl From<Box<bincode::ErrorKind>> for Error {
        fn from(err: Box<bincode::ErrorKind>) -> Self {
            Error::Rust(err)
        }
    }

    impl From<futures::channel::mpsc::SendError> for Error {
        fn from(err: futures::channel::mpsc::SendError) -> Self {
            Error::Rust(Box::new(err))
        }
    }
}
