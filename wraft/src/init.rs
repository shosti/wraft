use rand::{thread_rng, Rng};
use yew::prelude::*;
use yew::Properties;

pub struct ClusterInit {
    link: ComponentLink<Self>,
    session_key: String,
    props: InitProps,
}

#[derive(Properties, Clone)]
pub struct InitProps {
    pub onstart: yew::Callback<u128>,
}

pub enum Msg {
    UpdateSessionKey(String),
    StartCluster,
    JoinCluster,
    ClearStorage,
}

impl Component for ClusterInit {
    type Message = Msg;
    type Properties = InitProps;

    fn create(props: Self::Properties, link: ComponentLink<Self>) -> Self {
        Self {
            link,
            props,
            session_key: "".into(),
        }
    }

    fn update(&mut self, msg: Self::Message) -> ShouldRender {
        match msg {
            Msg::UpdateSessionKey(key) => {
                self.session_key = key;
                true
            }
            Msg::StartCluster => {
                let session_key = self.generate_session_key();
                self.props.onstart.emit(session_key);
                true
            }
            Msg::JoinCluster => match u128::from_str_radix(&self.session_key, 16) {
                Ok(session_key) => {
                    self.props.onstart.emit(session_key);
                    true
                }
                Err(_) => {
                    web_sys::window()
                        .unwrap()
                        .alert_with_message("Invalid session key!")
                        .unwrap();
                    false
                }
            },
            Msg::ClearStorage => {
                web_sys::window()
                    .unwrap()
                    .local_storage()
                    .unwrap()
                    .unwrap()
                    .clear()
                    .unwrap();
                false
            },
        }
    }

    fn change(&mut self, _props: Self::Properties) -> ShouldRender {
        false
    }

    fn view(&self) -> Html {
        let start = self.link.callback(|_| Msg::StartCluster);
        let session_key = self.session_key.clone();
        html! {
            <>
                <h1>{ "Startup" }</h1>
                <p>
                <button type="button" onclick=start>{ "Start new cluster" }</button>
                </p>
                <p>
                <div>
                <label for="join-session-key">{ "Join existing cluster" }</label>
                </div>
                <input
                type="text"
                value=session_key
                name="join-session-key"
                oninput=self.link.callback(|e: InputData| Msg::UpdateSessionKey(e.value))
                onkeypress=self.link.batch_callback(move |e: KeyboardEvent| {
                    if e.key() == "Enter" { Some(Msg::JoinCluster) } else { None }
                })
                />
                <div>
                <button type="button" onclick=self.link.callback(|_| Msg::ClearStorage)>{ "Clear local storage" }</button>
                </div>
                </p>
                </>
        }
    }
}

impl ClusterInit {
    fn generate_session_key(&self) -> u128 {
        thread_rng().gen()
    }
}

#[derive(Properties, Clone)]
pub struct WaitingProps {
    pub session_key: u128,
}

pub struct ClusterWaiting {
    session_key: u128,
}

impl Component for ClusterWaiting {
    type Message = ();
    type Properties = WaitingProps;

    fn create(props: Self::Properties, _link: ComponentLink<Self>) -> Self {
        Self {
            session_key: props.session_key,
        }
    }

    fn update(&mut self, _msg: Self::Message) -> ShouldRender {
        false
    }

    fn change(&mut self, props: Self::Properties) -> ShouldRender {
        props.session_key != self.session_key
    }

    fn view(&self) -> Html {
        html! {
            <>
            <h1>{ "Waiting for cluster to start..." }</h1>
                <h3>{ format!("Session key is {:032x}", self.session_key) }</h3>
                </>
        }
    }
}
