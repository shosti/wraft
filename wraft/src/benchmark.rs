// use crate::console_log;
// use crate::init::{ClusterInit, ClusterWaiting};
// use crate::raft::{Raft, RaftStateDump};
// use futures::channel::oneshot;
// use wasm_bindgen_futures::spawn_local;
// use web_sys::window;
// use yew::prelude::*;

// const CLUSTER_SIZE: usize = 3;

// pub struct Model {
//     state: State,
//     link: ComponentLink<Self>,
//     result: Option<BenchResult>,
//     state_dump: Option<Box<RaftStateDump<String>>>,
// }

// enum State {
//     Setup,
//     Waiting { session_key: u128 },
//     Running { bench: Benchmark },
// }

// pub enum Msg {
//     StartCluster(u128),
//     ClusterStarted(Benchmark),
//     StartBenchmark,
//     StopBenchmark,
//     BenchResult(BenchResult),
//     DumpState,
//     StateDumped(Box<RaftStateDump<String>>),
// }

// pub struct Benchmark {
//     raft: Raft<String>,
//     state: BenchState,
//     link: ComponentLink<Model>,
// }

// enum BenchState {
//     Stopped,
//     Running { stop: oneshot::Sender<()> },
// }

// impl Default for BenchState {
//     fn default() -> Self {
//         Self::Stopped
//     }
// }

// impl Component for Model {
//     type Message = Msg;
//     type Properties = ();

//     fn create(_props: Self::Properties, link: ComponentLink<Self>) -> Self {
//         Self {
//             state: State::Setup,
//             link,
//             result: None,
//             state_dump: None,
//         }
//     }

//     fn update(&mut self, msg: Self::Message) -> ShouldRender {
//         match msg {
//             Msg::StartCluster(session_key) => {
//                 self.start_raft(session_key);
//                 self.state = State::Waiting { session_key };
//                 true
//             }
//             Msg::ClusterStarted(bench) => {
//                 self.state = State::Running { bench };
//                 true
//             }
//             Msg::StartBenchmark => {
//                 if let State::Running { bench } = &mut self.state {
//                     bench.start();
//                     true
//                 } else {
//                     false
//                 }
//             }
//             Msg::StopBenchmark => {
//                 if let State::Running { bench } = &mut self.state {
//                     bench.stop();
//                     true
//                 } else {
//                     false
//                 }
//             }
//             Msg::BenchResult(result) => {
//                 self.result = Some(result);
//                 true
//             }
//             Msg::DumpState => {
//                 if let State::Running { bench } = &self.state {
//                     bench.dump_state(self.link.clone());
//                 }
//                 false
//             }
//             Msg::StateDumped(dump) => {
//                 self.state_dump = Some(dump);
//                 true
//             }
//         }
//     }

//     fn change(&mut self, _props: Self::Properties) -> ShouldRender {
//         false
//     }

//     fn view(&self) -> Html {
//         match &self.state {
//             State::Setup => {
//                 let onstart = self.link.callback(Msg::StartCluster);
//                 html! {
//                     <ClusterInit onstart=onstart />
//                 }
//             }
//             State::Waiting { session_key } => html! {
//                 <ClusterWaiting session_key=*session_key />
//             },
//             State::Running { bench } => match bench.state {
//                 BenchState::Running { .. } => html! {
//                     <>
//                         <h1>{ "Benchmark running..." }</h1>
//                     { self.render_bench_result() }
//                     <button type="button" onclick=self.link.callback(|_| Msg::StopBenchmark)>{ "Stop" }</button>
//                         <button type="button" onclick=self.link.callback(|_| Msg::DumpState)>{ "Show Raft State" }</button>
//                     { self.render_state_dump() }
//                         </>
//                 },
//                 BenchState::Stopped => html! {
//                     <>
//                         <h1>{ "Benchmark WRaft" }</h1>
//                     { self.render_bench_result() }
//                     <button type="button" onclick=self.link.callback(|_| Msg::StartBenchmark)>{ "Start" }</button>
//                         <button type="button" onclick=self.link.callback(|_| Msg::DumpState)>{ "Show Raft State" }</button>
//                     { self.render_state_dump() }
//                         </>
//                 },
//             },
//         }
//     }
// }

// impl Model {
//     fn start_raft(&self, session_key: u128) {
//         let link = self.link.clone();
//         spawn_local(async move {
//             let hostname = Self::hostname();
//             let raft = Raft::start(&hostname, session_key, CLUSTER_SIZE)
//                 .await
//                 .unwrap();
//             let bench = Benchmark::new(raft, link.clone());
//             link.send_message(Msg::ClusterStarted(bench));
//         });
//     }

//     fn hostname() -> String {
//         web_sys::window()
//             .expect("no global window")
//             .location()
//             .hostname()
//             .unwrap()
//     }

//     fn render_bench_result(&self) -> Html {
//         if let Some(res) = &self.result {
//             res.render()
//         } else {
//             html! {}
//         }
//     }

//     fn render_state_dump(&self) -> Html {
//         if let Some(dump) = &self.state_dump {
//             html! {
//                 <pre>{ format!("{:#?}", dump) }</pre>
//             }
//         } else {
//             html! {}
//         }
//     }
// }

// pub struct BenchResult {
//     iterations: usize,
//     t_start: f64,
//     t_end: f64,
// }

// impl Benchmark {
//     pub fn new(raft: Raft<String>, link: ComponentLink<Model>) -> Self {
//         Self {
//             raft,
//             link,
//             state: BenchState::default(),
//         }
//     }

//     pub fn start(&mut self) {
//         match self.state {
//             BenchState::Running { .. } => (),
//             BenchState::Stopped => {
//                 let (stop, stop_rx) = oneshot::channel();
//                 spawn_local(Self::run_benchmark(
//                     self.raft.clone(),
//                     self.link.clone(),
//                     stop_rx,
//                 ));
//                 self.state = BenchState::Running { stop }
//             }
//         }
//     }

//     pub fn stop(&mut self) {
//         let state = std::mem::take(&mut self.state);
//         match state {
//             BenchState::Running { stop } => {
//                 stop.send(()).unwrap();
//             }
//             BenchState::Stopped => (),
//         }
//     }

//     pub fn dump_state(&self, link: ComponentLink<Model>) {
//         let raft = self.raft.clone();
//         spawn_local(async move {
//             match raft.debug().await {
//                 Ok(dump) => {
//                     link.send_message(Msg::StateDumped(dump));
//                 }
//                 Err(err) => console_log!("Error dumping raft state: {:?}", err),
//             }
//         });
//     }

//     async fn run_benchmark(
//         raft: Raft<String>,
//         link: ComponentLink<Model>,
//         mut stop_rx: oneshot::Receiver<()>,
//     ) {
//         let key = "iter".to_string();
//         let performance = window()
//             .expect("no global window")
//             .performance()
//             .expect("performance not available");
//         let t_start = performance.now();
//         let mut i = 0;
//         loop {
//             if let Ok(Some(())) = stop_rx.try_recv() {
//                 break;
//             }
//             raft.set(key.clone(), i.to_string()).await.unwrap();
//             i += 1;
//             if i % 50 == 0 {
//                 link.send_message(Msg::BenchResult(BenchResult {
//                     iterations: i,
//                     t_start,
//                     t_end: performance.now(),
//                 }));
//             }
//         }
//         link.send_message(Msg::BenchResult(BenchResult {
//             iterations: i,
//             t_start,
//             t_end: performance.now(),
//         }));
//     }
// }

// impl BenchResult {
//     pub fn render(&self) -> Html {
//         let elapsed_secs = (self.t_end - self.t_start) / 1000.0;
//         let iterations_per_sec = (self.iterations as f64) / elapsed_secs;

//         html! {
//             <div>
//                 <strong>{ "Results:" }</strong>
//             {
//                 format!(
//                     "{} iterations in {} seconds ({} iterations per second)",
//                     self.iterations,
//                     elapsed_secs,
//                     iterations_per_sec
//                 )
//             }
//             </div>
//         }
//     }
// }
