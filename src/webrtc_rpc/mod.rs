mod introduction;

pub async fn introduce(
    id: String,
    session_id: String,
) -> Result<(), error::Error> {
    introduction::initiate(&id, &session_id).await
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
