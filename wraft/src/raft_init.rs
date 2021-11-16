use crate::raft::{self, Raft};
use rand::{thread_rng, Rng};
use std::marker::PhantomData;
use std::sync::{Arc, Mutex};
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;
use yew::Properties;

const CLUSTER_SIZE: usize = 3;

pub struct RaftWrapper<S: raft::State>(Arc<Mutex<Option<Raft<S>>>>);

impl<S: raft::State> RaftWrapper<S> {
    pub fn new(raft: Raft<S>) -> Self {
        Self(Arc::new(Mutex::new(Some(raft))))
    }

    pub fn take(&self) -> Option<Raft<S>> {
        let mut r = self.0.lock().unwrap();
        r.take()
    }
}

impl<S: raft::State> Clone for RaftWrapper<S> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

#[derive(Properties, Clone)]
pub struct Props {
    pub session_key: Option<u128>,
}

#[derive(Properties)]
pub struct RaftProps<S: raft::State> {
    pub raft: RaftWrapper<S>,
}

impl<S: raft::State> Clone for RaftProps<S> {
    fn clone(&self) -> Self {
        Self {
            raft: self.raft.clone(),
        }
    }
}

pub enum Msg<S: raft::State> {
    UpdateSessionKey(String),
    StartCluster,
    JoinCluster,
    ClearStorage,
    ClusterStarted(RaftWrapper<S>),
}

enum State<S: raft::State> {
    Setup,
    Waiting(u128),
    Running(RaftWrapper<S>),
}

pub struct Model<C: Component<Properties = RaftProps<S>>, S: raft::State>
where
    S: raft::State + Clone,
{
    link: ComponentLink<Self>,
    session_key: String,
    state: State<S>,
    _component: PhantomData<C>,
    _message: PhantomData<S>,
}

impl<C: Component<Properties = RaftProps<S>>, S> Component for Model<C, S>
where
    S: raft::State + Clone,
{
    type Message = Msg<S>;
    type Properties = Props;

    fn create(props: Self::Properties, link: ComponentLink<Self>) -> Self {
        if let Some(session_key) = props.session_key {
            Self {
                link,
                state: State::Waiting(session_key),
                session_key: "".into(),
                _component: PhantomData,
                _message: PhantomData,
            }
        } else {
            Self {
                link,
                state: State::Setup,
                session_key: "".into(),
                _component: PhantomData,
                _message: PhantomData,
            }
        }
    }

    fn update(&mut self, msg: Self::Message) -> ShouldRender {
        match msg {
            Msg::UpdateSessionKey(key) => {
                self.session_key = key;
                true
            }
            Msg::StartCluster => {
                let session_key = generate_session_key();
                self.start_raft(session_key, self.link.clone());
                self.state = State::Waiting(session_key);
                true
            }
            Msg::JoinCluster => match u128::from_str_radix(&self.session_key, 16) {
                Ok(session_key) => {
                    self.start_raft(session_key, self.link.clone());
                    self.state = State::Waiting(session_key);
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
            }
            Msg::ClusterStarted(raft) => {
                self.state = State::Running(raft);
                true
            }
        }
    }

    fn change(&mut self, _props: Self::Properties) -> ShouldRender {
        false
    }

    fn view(&self) -> Html {
        match &self.state {
            State::Setup => self.render_setup(),
            State::Waiting(session_key) => self.render_waiting(*session_key),
            State::Running(raft) => self.render_running(raft.clone()),
        }
    }
}

impl<C: Component<Properties = RaftProps<S>>, S> Model<C, S>
where
    S: raft::State + Clone,
{
    fn render_setup(&self) -> Html {
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

    fn render_waiting(&self, session_key: u128) -> Html {
        html! {
            <>
            <h1>{ "Waiting for cluster to start..." }</h1>
                <h3>{ format!("Session key is {:032x}", session_key) }</h3>
                </>
        }
    }

    fn render_running(&self, raft: RaftWrapper<S>) -> Html {
        html! { <C raft=raft /> }
    }

    fn start_raft(&self, session_key: u128, link: ComponentLink<Self>) {
        spawn_local(async move {
            let hostname = hostname();
            let raft = Raft::start(&hostname, session_key, CLUSTER_SIZE)
                .await
                .unwrap();
            link.send_message(Msg::ClusterStarted(RaftWrapper::new(raft)));
        });
    }
}

fn generate_session_key() -> u128 {
    thread_rng().gen()
}

fn hostname() -> String {
    web_sys::window()
        .expect("no global window")
        .location()
        .hostname()
        .unwrap()
}
