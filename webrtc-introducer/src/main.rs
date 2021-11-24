mod errors;
use std::env;
use errors::Error;
use futures::sink::SinkExt;
use futures::stream::StreamExt;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};
use tokio::select;
use tokio::sync::broadcast::{channel, Sender};
use tokio::sync::mpsc;
use tokio_tungstenite::accept_hdr_async;
use tokio_tungstenite::tungstenite;
use tungstenite::handshake::server;
use webrtc_introducer_types::{Command, Session};

#[derive(Clone, Default)]
struct Channels {
    messages: Arc<RwLock<HashMap<u128, Sender<Command>>>>,
    online: Arc<RwLock<HashMap<u128, HashSet<u64>>>>,
}

struct Headers {}

impl server::Callback for Headers {
    fn on_request(
        self,
        _request: &server::Request,
        mut response: server::Response,
    ) -> Result<server::Response, server::ErrorResponse> {
        response.headers_mut().insert(
            http::header::CONTENT_SECURITY_POLICY,
            "default-src *".parse().unwrap(),
        );
        Ok(response)
    }
}

impl Channels {
    pub fn get_messages(&self, session_id: u128) -> Option<Sender<Command>> {
        let m = self.messages.read().unwrap();
        m.get(&session_id).cloned()
    }

    pub fn joined(&self, node_id: u64, session_id: u128) -> Result<(), Error> {
        {
            let mut os = self.online.write().unwrap();
            let o = os
                .entry(session_id)
                .or_insert_with(HashSet::new);
            o.insert(node_id);
        }
        self.broadcast_session_info(session_id)
    }

    pub fn left(&self, node_id: u64, session_id: u128) -> Result<(), Error> {
        {
            let mut o = self.online.write().unwrap();
            let online = o.get_mut(&session_id).unwrap();
            online.remove(&node_id);
        }
        self.broadcast_session_info(session_id)
    }

    pub fn broadcast(self, session_id: u128, command: Command) -> Result<(), Error> {
        let m = self.messages.read().unwrap();
        let tx = m
            .get(&session_id)
            .ok_or_else(|| Error::SessionNotFound(format!("{:032x}", session_id)))?;
        tx.send(command)?;
        Ok(())
    }

    pub fn broadcast_session_info(&self, session_id: u128) -> Result<(), Error> {
        let onlines = self.online.read().unwrap();
        let online = onlines.get(&session_id).unwrap();
        let status = Session {
            session_id,
            online: online.clone(),
        };
        let cmd = Command::SessionStatus(status);
        self.clone().broadcast(session_id, cmd)
    }

    pub fn ensure_session(&self, session_id: u128) {
        let mut m = self.messages.write().unwrap();
        if m.contains_key(&session_id) {
            return;
        }
        let (tx, _rx) = channel(10);
        m.insert(session_id, tx);
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let port = env::var("PORT").unwrap_or_else(|_| "3000".to_string());
    let listen = env::var("LISTEN_ADDR").unwrap_or_else(|_| "0.0.0.0".to_string());
    let addr = format!("{}:{}", listen, port);
    println!("listening on {}", addr);
    let listener = TcpListener::bind(addr).await?;
    let channels = Channels::default();

    loop {
        let ch = channels.clone();
        let (socket, _) = listener.accept().await?;
        tokio::spawn(async move {
            match process_socket(socket, ch).await {
                Ok(_) => (),
                Err(err) => {
                    println!("ERROR: {:#?}", err);
                }
            }
        });
    }
}

async fn process_socket(socket: TcpStream, channels: Channels) -> Result<(), Error> {
    let cb = Headers {};
    let conn = accept_hdr_async(socket, cb).await?;
    let (mut send, mut recv) = conn.split();
    let (join_tx, mut join_rx) = mpsc::channel::<(u64, u128)>(1);
    let (left_tx, mut left_rx) = mpsc::channel::<()>(1);
    let ch = channels.clone();
    let recv_handle: tokio::task::JoinHandle<Result<(), Error>> = tokio::spawn(async move {
        if let Some((node_id, session_id)) = join_rx.recv().await {
            ch.ensure_session(session_id);
            let mut rx = ch.get_messages(session_id).unwrap().subscribe();
            ch.joined(node_id, session_id)?;
            let mut ping_interval = tokio::time::interval(Duration::from_secs(5));
            loop {
                select! {
                    Ok(msg) = rx.recv() => {
                        match msg {
                            Command::SessionStatus(_) => {
                                // Always send session status to everyone
                                let data = bincode::serialize(&msg)?;
                                send.send(tungstenite::Message::Binary(data)).await?;
                            }
                            Command::Offer(ref offer) => {
                                if offer.session_id == session_id && offer.target_id == node_id {
                                    let data = bincode::serialize(&msg)?;
                                    send.send(tungstenite::Message::Binary(data)).await?;
                                }
                            }
                            Command::Answer(ref answer) => {
                                if answer.session_id == session_id && answer.target_id == node_id {
                                    let data = bincode::serialize(&msg)?;
                                    send.send(tungstenite::Message::Binary(data)).await?;
                                }
                            }
                            Command::IceCandidate(ref answer) => {
                                if answer.session_id == session_id && answer.target_id == node_id {
                                    let data = bincode::serialize(&msg)?;
                                    send.send(tungstenite::Message::Binary(data)).await?;
                                }
                            }
                            _ => unreachable!(),
                        }
                    }
                    _ = ping_interval.tick() => {
                        send.send(tungstenite::Message::Ping("hello".into())).await?;
                    }
                    _ = left_rx.recv() => return Ok(()),
                }
            }
        }
        Ok(())
    });

    let mut ids = None;
    while let Some(msg) = recv.next().await {
        let ch = channels.clone();
        match msg {
            Ok(tungstenite::Message::Binary(data)) => {
                match process_message(&data, ch, join_tx.clone()).await {
                    Ok(info) => ids = info,
                    Err(err) => println!("Error handling message: {:?}", err),
                }
            }
            Ok(tungstenite::Message::Pong(_)) => (),
            Ok(tungstenite::Message::Close(_)) => (),
            Ok(msg) => println!("Unexpected message: {}", msg),
            Err(err) => println!("Error handling receiving message: {:?}", err),
        }
    }

    left_tx.send(()).await?;
    recv_handle.await.unwrap()?;
    if let Some((node_id, session_id)) = ids {
        channels.left(node_id, session_id)?;
    }
    Ok(())
}

async fn process_message(
    data: &[u8],
    channels: Channels,
    join_tx: mpsc::Sender<(u64, u128)>,
) -> Result<Option<(u64, u128)>, Error> {
    let cmd: Command = bincode::deserialize(data)?;
    let mut ids = None;
    match cmd {
        Command::Join(join) => {
            let info = (join.node_id, join.session_id);
            ids = Some(info);
            join_tx.send(info).await?;
        }
        Command::Offer(ref offer) => {
            channels.broadcast(offer.session_id, cmd.clone())?;
        }
        Command::Answer(ref answer) => {
            channels.broadcast(answer.session_id, cmd.clone())?;
        }
        Command::IceCandidate(ref candidate) => {
            channels.broadcast(candidate.session_id, cmd.clone())?;
        }
        _ => unreachable!(),
    }

    Ok(ids)
}
