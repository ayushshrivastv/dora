use dora_core::{config::NodeId, daemon_messages, topics::DORA_DAEMON_PORT_DEFAULT};
use eyre::{eyre, Context};
use futures_concurrency::stream::Merge;
use shared_memory::ShmemConf;
use std::{collections::HashMap, io::ErrorKind, net::Ipv4Addr};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::mpsc,
};
use tokio_stream::{
    wrappers::{ReceiverStream, TcpListenerStream},
    StreamExt,
};

#[tokio::main]
async fn main() -> eyre::Result<()> {
    set_up_tracing().wrap_err("failed to set up tracing subscriber")?;

    let localhost = Ipv4Addr::new(127, 0, 0, 1);
    let socket = match TcpListener::bind((localhost, DORA_DAEMON_PORT_DEFAULT)).await {
        Ok(socket) => socket,
        Err(err) if err.kind() == ErrorKind::AddrInUse => {
            eyre::bail!(
                "port {DORA_DAEMON_PORT_DEFAULT} is already in use. \
                Is `dora-daemon` already running?"
            );
        }
        Err(err) => {
            return Err(eyre::Report::new(err).wrap_err(format!(
                "failed to listen on port {DORA_DAEMON_PORT_DEFAULT}"
            )))
        }
    };

    // TODO: set up connection to coordinator

    let new_connections = TcpListenerStream::new(socket).map(|c| {
        c.map(Event::NewConnection)
            .wrap_err("failed to open connection")
            .unwrap_or_else(Event::ConnectError)
    });
    let (node_events_tx, node_events_rx) = mpsc::channel(10);
    let node_events = ReceiverStream::new(node_events_rx);

    let mut events = (new_connections, node_events).merge();

    let mut uninit_shared_memory = HashMap::new();
    let mut sent_out_shared_memory = HashMap::new();

    while let Some(event) = events.next().await {
        match event {
            Event::NewConnection(mut connection) => {
                let events_tx = node_events_tx.clone();
                let mut id = None;
                tokio::spawn(async move {
                    loop {
                        let raw = match tcp_receive(&mut connection).await {
                            Ok(data) => data,
                            Err(err) if err.kind() == ErrorKind::UnexpectedEof => {
                                break;
                            }
                            Err(err) => {
                                tracing::error!("{err:?}");
                                continue;
                            }
                        };
                        let message: daemon_messages::Request = match serde_json::from_slice(&raw)
                            .wrap_err("failed to deserialize node message")
                        {
                            Ok(e) => e,
                            Err(err) => {
                                tracing::warn!("{err:?}");
                                continue;
                            }
                        };

                        let node_event = match message {
                            daemon_messages::Request::Register { node_id } => {
                                id = Some(node_id);

                                let reply = daemon_messages::Reply::RegisterResult(Ok(()));
                                let serialized = serde_json::to_vec(&reply)
                                    .wrap_err("failed to serialize register result");

                                let send_result = match serialized {
                                    Err(err) => {
                                        tracing::warn!("{err:?}");
                                        continue;
                                    }
                                    Ok(m) => tcp_send(&mut connection, &m).await,
                                };

                                match send_result {
                                    Ok(()) => continue,
                                    Err(err) => {
                                        tracing::warn!("{err:?}");
                                        break; // close connection
                                    }
                                }
                            }
                            daemon_messages::Request::PrepareOutputMessage { len } => {
                                NodeEvent::PrepareOutputMessage { len }
                            }
                            daemon_messages::Request::SendOutMessage { id } => {
                                NodeEvent::SendOutMessage { id }
                            }
                        };
                        let event = Event::Node {
                            id: match &id {
                                Some(id) => id.clone(),
                                None => {
                                    tracing::warn!(
                                        "Ignoring node event because no register \
                                        message was sent yet: {node_event:?}"
                                    );
                                    continue;
                                }
                            },
                            event: node_event,
                        };
                        let Ok(()) = events_tx.send(event).await else {
                            break;
                        };
                    }
                });
            }
            Event::ConnectError(err) => {
                tracing::warn!("{:?}", err.wrap_err("failed to connect"));
            }
            Event::Node { id, event } => match event {
                NodeEvent::PrepareOutputMessage { len } => {
                    let memory = ShmemConf::new()
                        .size(len)
                        .create()
                        .wrap_err("failed to allocate shared memory")?;
                    let id = memory.get_os_id().to_owned();
                    uninit_shared_memory.insert(id, memory);

                    // TODO send reply with id
                }
                NodeEvent::SendOutMessage { id } => {
                    let memory = uninit_shared_memory
                        .remove(&id)
                        .ok_or_else(|| eyre!("invalid shared memory id"))?;

                    // TODO send shared memory ID to all local receivers

                    let data = std::ptr::slice_from_raw_parts(memory.as_ptr(), memory.len());
                    // TODO send `data` via network to all remove receivers

                    sent_out_shared_memory.insert(id, memory);
                }
            },
        }
    }

    Ok(())
}

enum Event {
    NewConnection(TcpStream),
    ConnectError(eyre::Report),
    Node { id: NodeId, event: NodeEvent },
}

#[derive(Debug)]
pub enum NodeEvent {
    PrepareOutputMessage { len: usize },
    SendOutMessage { id: MessageId },
}

type MessageId = String;

async fn tcp_send(connection: &mut TcpStream, request: &[u8]) -> std::io::Result<()> {
    let len_raw = (request.len() as u64).to_le_bytes();
    connection.write_all(&len_raw).await?;
    connection.write_all(request).await?;
    Ok(())
}

async fn tcp_receive(connection: &mut TcpStream) -> std::io::Result<Vec<u8>> {
    let reply_len = {
        let mut raw = [0; 8];
        connection.read_exact(&mut raw).await?;
        u64::from_le_bytes(raw) as usize
    };
    let mut reply = vec![0; reply_len];
    connection.read_exact(&mut reply).await?;
    Ok(reply)
}

fn set_up_tracing() -> eyre::Result<()> {
    use tracing_subscriber::prelude::__tracing_subscriber_SubscriberExt;

    let stdout_log = tracing_subscriber::fmt::layer().pretty();
    let subscriber = tracing_subscriber::Registry::default().with(stdout_log);
    tracing::subscriber::set_global_default(subscriber)
        .context("failed to set tracing global subscriber")
}
