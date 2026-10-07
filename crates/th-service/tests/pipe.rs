//! The pipe server end to end: a real named pipe, a client connecting as the current user.
#![cfg(windows)]

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use th_core::ipc::{
    self, ClientMessage, Event, PROTOCOL_VERSION, Request, Response, ServerMessage,
};
use th_service::pipe::{self, Handler, Listener};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, ReadHalf, WriteHalf};
use tokio::net::windows::named_pipe::NamedPipeClient;
use tokio::sync::{broadcast, oneshot};
use tokio::task::JoinHandle;
use tokio::time::{Duration, timeout};

/// Answers `GetStatus` with `Ok` and refuses the rest, naming what it received.
struct Recorder;

impl Handler for Recorder {
    async fn handle(&self, request: Request) -> Response {
        match request {
            Request::GetStatus => Response::Ok,
            other => Response::Error {
                message: format!("handler saw {other:?}"),
            },
        }
    }
}

struct Server {
    name: String,
    events: broadcast::Sender<Event>,
    stop: oneshot::Sender<()>,
    task: JoinHandle<std::io::Result<()>>,
}

fn unique_name() -> String {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    format!(
        r"\\.\pipe\tunnelhop-test-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

fn start() -> Server {
    let name = unique_name();
    let listener = Listener::bind(&name).unwrap();
    let (events, _) = broadcast::channel(16);
    let (stop, stopped) = oneshot::channel::<()>();
    let task = tokio::spawn(listener.serve(Arc::new(Recorder), events.clone(), async {
        let _ = stopped.await;
    }));
    Server {
        name,
        events,
        stop,
        task,
    }
}

struct Client {
    reader: BufReader<ReadHalf<NamedPipeClient>>,
    writer: WriteHalf<NamedPipeClient>,
}

impl Client {
    async fn connect(name: &str) -> Client {
        let pipe = pipe::connect(name).await.unwrap();
        let (reader, writer) = tokio::io::split(pipe);
        Client {
            reader: BufReader::new(reader),
            writer,
        }
    }

    /// Connected and past `Hello`.
    async fn greeted(name: &str) -> Client {
        let mut client = Client::connect(name).await;
        client
            .send(
                1,
                Request::Hello {
                    version: PROTOCOL_VERSION,
                },
            )
            .await;
        assert_eq!(
            client.response(1).await,
            ipc::check_version(PROTOCOL_VERSION).unwrap()
        );
        client
    }

    async fn send(&mut self, id: u64, request: Request) {
        self.send_raw(&ipc::encode(&ClientMessage { id, request }))
            .await;
    }

    async fn send_raw(&mut self, bytes: &[u8]) {
        self.writer.write_all(bytes).await.unwrap();
    }

    /// The next message, or `None` once the server has closed the connection.
    async fn next(&mut self) -> Option<ServerMessage> {
        let mut line = String::new();
        let read = timeout(Duration::from_secs(5), self.reader.read_line(&mut line))
            .await
            .expect("the server answered in time");
        match read {
            Ok(0) | Err(_) => None,
            Ok(_) => Some(ipc::decode(line.trim_end().as_bytes()).unwrap()),
        }
    }

    async fn response(&mut self, expected_id: u64) -> Response {
        match self.next().await {
            Some(ServerMessage::Response { id, response }) => {
                assert_eq!(id, expected_id);
                response
            }
            other => panic!("expected response {expected_id}, got {other:?}"),
        }
    }
}

fn is_error(response: &Response, containing: &str) -> bool {
    matches!(response, Response::Error { message } if message.contains(containing))
}

#[tokio::test]
async fn ping_round_trips_after_hello() {
    let server = start();
    let mut client = Client::greeted(&server.name).await;
    client.send(2, Request::Ping).await;
    assert_eq!(client.response(2).await, Response::Pong);
}

#[tokio::test]
async fn valid_requests_reach_the_handler() {
    let server = start();
    let mut client = Client::greeted(&server.name).await;
    client.send(7, Request::GetStatus).await;
    assert_eq!(client.response(7).await, Response::Ok);
}

#[tokio::test]
async fn an_invalid_request_is_refused_before_the_handler_and_the_connection_survives() {
    let server = start();
    let mut client = Client::greeted(&server.name).await;
    client.send(2, Request::SetInterval { minutes: 0 }).await;
    let response = client.response(2).await;
    assert!(is_error(&response, "between 1 and 1440"), "{response:?}");
    assert!(!is_error(&response, "handler saw"), "{response:?}");

    client.send(3, Request::Ping).await;
    assert_eq!(client.response(3).await, Response::Pong);
}

#[tokio::test]
async fn a_wrong_protocol_version_is_refused_and_closed() {
    let server = start();
    let mut client = Client::connect(&server.name).await;
    client.send(1, Request::Hello { version: 99 }).await;
    let response = client.response(1).await;
    assert!(is_error(&response, "not supported"), "{response:?}");
    assert_eq!(client.next().await, None);
}

#[tokio::test]
async fn a_request_before_hello_is_refused_and_closed() {
    let server = start();
    let mut client = Client::connect(&server.name).await;
    client.send(1, Request::Ping).await;
    let response = client.response(1).await;
    assert!(is_error(&response, "hello"), "{response:?}");
    assert_eq!(client.next().await, None);
}

#[tokio::test]
async fn a_second_hello_is_refused() {
    let server = start();
    let mut client = Client::greeted(&server.name).await;
    client
        .send(
            2,
            Request::Hello {
                version: PROTOCOL_VERSION,
            },
        )
        .await;
    assert!(is_error(&client.response(2).await, "already"));
}

#[tokio::test]
async fn a_malformed_line_closes_the_connection() {
    let server = start();
    let mut client = Client::greeted(&server.name).await;
    client
        .send_raw(b"{\"id\":2,\"request\":{\"type\":\"nope\"}}\n")
        .await;
    assert_eq!(client.next().await, None);
}

#[tokio::test]
async fn events_reach_every_greeted_client() {
    let server = start();
    let mut first = Client::greeted(&server.name).await;
    let mut second = Client::greeted(&server.name).await;
    let alert = Event::Alert {
        message: "blocked".into(),
    };
    // Each session subscribes when it is accepted; give the second a moment to get there.
    while server.events.receiver_count() < 2 {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    server.events.send(alert.clone()).unwrap();
    for client in [&mut first, &mut second] {
        assert_eq!(
            client.next().await,
            Some(ServerMessage::Event {
                event: alert.clone()
            })
        );
    }
}

#[tokio::test]
async fn a_second_server_cannot_take_the_name() {
    let server = start();
    let squatter = Listener::bind(&server.name);
    assert!(squatter.is_err(), "a second first instance was allowed");
}

#[tokio::test]
async fn shutdown_stops_the_server() {
    let server = start();
    let _client = Client::greeted(&server.name).await;
    server.stop.send(()).unwrap();
    let result = timeout(Duration::from_secs(5), server.task).await;
    assert!(matches!(result, Ok(Ok(Ok(())))), "{result:?}");
}
