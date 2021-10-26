pub mod client;
mod introduction;

static CLUSTER_SIZE: usize = 3;

pub async fn introduce(id: String, session_id: String) -> Result<(), error::Error> {
    let client = introduction::initiate(&id, &session_id, CLUSTER_SIZE).await?;
    println!("CLIENT: {:#?}", client);
    Ok(())
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

    impl From<futures::channel::oneshot::Canceled> for Error {
        fn from(err: futures::channel::oneshot::Canceled) -> Self {
            Error::Rust(Box::new(err))
        }
    }
}
