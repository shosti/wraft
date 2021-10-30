use futures::channel::mpsc::{channel, Receiver};
use futures::channel::oneshot;
use futures::future::FusedFuture;
use futures::stream::FusedStream;
use futures::task::{Context, Poll};
use futures::StreamExt;
use futures::{Future, FutureExt, Stream};
use std::convert::TryInto;
use std::pin::Pin;
use std::time::Duration;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{window, Window};

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

pub struct Sleep {
    rx: oneshot::Receiver<()>,
    timeout_handle: i32,
    _cb: Closure<dyn FnMut()>,
}

impl Sleep {
    pub fn new(d: Duration) -> Self {
        let (tx, rx) = oneshot::channel::<()>();
        let _cb = Closure::once(move || {
            tx.send(()).unwrap();
        });

        let timeout_handle = get_window()
            .set_timeout_with_callback_and_timeout_and_arguments_0(
                _cb.as_ref().unchecked_ref(),
                d.as_millis().try_into().unwrap(),
            )
            .unwrap();

        Self {
            rx,
            timeout_handle,
            _cb,
        }
    }
}

impl Drop for Sleep {
    fn drop(&mut self) {
        get_window().clear_timeout_with_handle(self.timeout_handle);
    }
}

impl Future for Sleep {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match self.rx.poll_unpin(cx) {
            Poll::Ready(Ok(())) => Poll::Ready(()),
            Poll::Ready(Err(err)) => panic!("sleep receiver poll failed: {}", err),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl FusedFuture for Sleep {
    fn is_terminated(&self) -> bool {
        self.rx.is_terminated()
    }
}

pub fn get_window() -> Window {
    window().expect("no global window")
}

pub async fn sleep(d: Duration) {
    Sleep::new(d).await
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
