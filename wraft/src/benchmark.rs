use crate::console_log;
use crate::raft::{self, client::Client, Raft, RaftStateDump};
use crate::raft_init::{self, RaftProps};
use futures::channel::mpsc::{unbounded, UnboundedReceiver, UnboundedSender};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use wasm_bindgen_futures::spawn_local;
use web_sys::window;
use yew::prelude::*;

pub type Benchmark = raft_init::Model<Model, State>;

pub struct Model {
    state: BenchState,
    raft_client: Client<State>,
    link: ComponentLink<Self>,
    result: Option<BenchResult>,
    state_dump: Option<Box<RaftStateDump>>,
    bench_toggle: UnboundedSender<()>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum BenchMsg {
    Start,
    Iter,
}

impl raft::Command for BenchMsg {}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct State {}

pub struct BenchResult {
    start: f64,
    end: f64,
    iters: usize,
}

impl raft::State for State {
    type Command = BenchMsg;
    type Item = BenchResult;
    type Key = ();
    type Notification = BenchMsg;

    fn apply(&mut self, cmd: Self::Command) -> BenchMsg {
        cmd
    }

    fn get(&self, _: ()) -> Option<Self::Item> {
        None
    }
}

pub enum Msg {
    ToggleBenchmark,
    BenchResult(BenchResult),
    DumpState,
    StateDumped(Box<RaftStateDump>),
}

pub enum BenchState {
    Stopped,
    Running,
}

impl Component for Model {
    type Message = Msg;
    type Properties = RaftProps<State>;

    fn create(props: Self::Properties, link: ComponentLink<Self>) -> Self {
        let raft = props.raft.take().unwrap();
        let raft_client = raft.client();
        let (bench_toggle, bench_toggle_rx) = unbounded();

        Self::run_benchmarker(raft_client.clone(), bench_toggle_rx);
        Self::run_update_notifier(raft, link.clone());

        Self {
            link,
            raft_client,
            bench_toggle,
            state: BenchState::Stopped,
            result: None,
            state_dump: None,
        }
    }

    fn update(&mut self, msg: Self::Message) -> ShouldRender {
        match msg {
            Msg::ToggleBenchmark => {
                self.state = match self.state {
                    BenchState::Running => BenchState::Stopped,
                    BenchState::Stopped => BenchState::Running,
                };
                self.bench_toggle.unbounded_send(()).unwrap();
                true
            }
            Msg::BenchResult(result) => {
                self.result = Some(result);
                true
            }
            Msg::DumpState => {
                let client = self.raft_client.clone();
                let link = self.link.clone();
                spawn_local(async move {
                    if let Ok(dump) = client.debug().await {
                        link.send_message(Msg::StateDumped(dump));
                    }
                });
                false
            }
            Msg::StateDumped(dump) => {
                self.state_dump = Some(dump);
                true
            }
        }
    }

    fn change(&mut self, _props: Self::Properties) -> ShouldRender {
        false
    }

    fn view(&self) -> Html {
        match &self.state {
            BenchState::Running { .. } => html! {
                <>
                    <h1>{ "Benchmark running..." }</h1>
                { self.render_bench_result() }
                <button type="button" onclick=self.link.callback(|_| Msg::ToggleBenchmark)>{ "Stop" }</button>
                    <button type="button" onclick=self.link.callback(|_| Msg::DumpState)>{ "Show Raft State" }</button>
                { self.render_state_dump() }
                </>
            },
            BenchState::Stopped => html! {
                <>
                    <h1>{ "Benchmark WRaft" }</h1>
                { self.render_bench_result() }
                <button type="button" onclick=self.link.callback(|_| Msg::ToggleBenchmark)>{ "Start" }</button>
                    <button type="button" onclick=self.link.callback(|_| Msg::DumpState)>{ "Show Raft State" }</button>
                { self.render_state_dump() }
                </>
            },
        }
    }
}

impl Model {
    fn run_benchmarker(mut raft_client: Client<State>, mut bench_toggle: UnboundedReceiver<()>) {
        spawn_local(async move {
            loop {
                if let Some(()) = bench_toggle.next().await {
                    Self::run_benchmark(&mut raft_client, &mut bench_toggle).await;
                }
            }
        });
    }

    fn run_update_notifier(mut raft: Raft<State>, link: ComponentLink<Self>) {
        let performance = window().expect("no global window").performance().unwrap();
        spawn_local(async move {
            let mut start = performance.now();
            let mut iters = 0;
            while let Some(msg) = raft.next().await {
                match msg {
                    BenchMsg::Start => {
                        start = performance.now();
                        iters = 0;
                    }
                    BenchMsg::Iter => {
                        iters += 1;
                        if iters % 50 == 0 {
                            let result = BenchResult {
                                iters,
                                start,
                                end: performance.now(),
                            };
                            link.send_message(Msg::BenchResult(result));
                        }
                    }
                }
            }
        });
    }

    async fn run_benchmark(
        raft_client: &mut Client<State>,
        bench_toggle: &mut UnboundedReceiver<()>,
    ) {
        if let Err(err) = raft_client.send(BenchMsg::Start).await {
            console_log!("error: {:?}", err);
            return;
        }
        loop {
            if bench_toggle.try_next().is_ok() {
                return;
            }
            if let Err(err) = raft_client.send(BenchMsg::Iter).await {
                console_log!("error: {:?}", err);
            };
        }
    }

    fn render_bench_result(&self) -> Html {
        if let Some(res) = &self.result {
            let elapsed_secs = (res.end - res.start) / 1000.0;
            let iterations_per_sec = (res.iters as f64) / elapsed_secs;

            html! {
                <div>
                    <strong>{ "Results:" }</strong>
                {
                    format!(
                        "{} iterations in {:.2} seconds ({:.2} iterations per second)",
                        res.iters,
                        elapsed_secs,
                        iterations_per_sec
                    )
                }
                </div>
            }
        } else {
            html! {}
        }
    }

    fn render_state_dump(&self) -> Html {
        if let Some(dump) = &self.state_dump {
            html! {
                <pre>{ format!("{:#?}", dump) }</pre>
            }
        } else {
            html! {}
        }
    }
}
