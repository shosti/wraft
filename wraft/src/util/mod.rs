use futures::channel::mpsc::{channel, Receiver};
use futures::stream::FusedStream;
use futures::task::{Context, Poll};
use futures::Stream;
use futures::StreamExt;
use js_sys::{Function, Promise};
use std::convert::TryInto;
use std::pin::Pin;
use std::time::Duration;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;
use web_sys::window;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = console)]
    pub fn log(s: &str);
}

#[macro_export]
macro_rules! console_log {
    ($($t:tt)*) => (crate::util::log(&format_args!($($t)*).to_string()))
}

pub fn set_panic_hook() {
    // When the `console_error_panic_hook` feature is enabled, we can call the
    // `set_panic_hook` function at least once during initialization, and then
    // we will get better error messages if our code ever panics.
    //
    // For more details see
    // https://github.com/rustwasm/console_error_panic_hook#readme
    #[cfg(feature = "console_error_panic_hook")]
    console_error_panic_hook::set_once();
}

pub async fn sleep(d: Duration) {
    // Keep reference to callback closure to prevent it from getting prematurely
    // dropped.
    let mut _closure: Option<Closure<dyn Fn()>> = None;
    let mut cb = |resolve: Function, _reject: Function| {
        let c = Closure::wrap(Box::new(move || {
            resolve.call0(&JsValue::UNDEFINED).unwrap();
        }) as Box<dyn Fn()>);

        window()
            .expect("no global window")
            .set_timeout_with_callback_and_timeout_and_arguments_0(
                c.as_ref().unchecked_ref(),
                d.as_millis().try_into().unwrap(),
            )
            .unwrap();
        _closure = Some(c);
    };
    let promise = Promise::new(&mut cb);
    JsFuture::from(promise).await.unwrap();
}

pub struct Interval {
    rx: Receiver<()>,
    interval_id: i32,
    _cb: Closure<dyn FnMut()>,
}

impl Interval {
    pub fn new(d: Duration) -> Self {
        let (mut tx, rx) = channel(5);

        let cb = Closure::wrap(Box::new(move || {
            tx.try_send(()).unwrap();
        }) as Box<dyn FnMut()>);
        let interval_id = window()
            .expect("no global window")
            .set_interval_with_callback_and_timeout_and_arguments_0(
                cb.as_ref().unchecked_ref(),
                d.as_millis().try_into().unwrap(),
            )
            .unwrap();

        Self {
            interval_id,
            rx,
            _cb: cb,
        }
    }
}

impl Drop for Interval {
    fn drop(&mut self) {
        window()
            .expect("no global window")
            .clear_interval_with_handle(self.interval_id);
    }
}

impl Stream for Interval {
    type Item = ();

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.rx.poll_next_unpin(cx)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.rx.size_hint()
    }
}

impl FusedStream for Interval {
    fn is_terminated(&self) -> bool {
        self.rx.is_terminated()
    }
}
