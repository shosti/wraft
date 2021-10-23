use js_sys::{Function, Promise};
use std::convert::TryInto;
use std::time::Duration;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
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

pub async fn sleep(d: Duration) -> Result<JsValue, JsValue> {
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
    wasm_bindgen_futures::JsFuture::from(promise).await
}
