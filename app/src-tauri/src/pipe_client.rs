//! The app's connection to th-service: one session over the named pipe, opened with `Hello`,
//! carrying numbered requests and the service's events, and opened again whenever it drops.
//!
//! The UI runs unelevated and the service may stop or restart under it, so losing the pipe is a
//! normal state rather than an error: it is reported as [`Update::Disconnected`], requests made
//! meanwhile are answered at once with "not running", and the client keeps trying. Kept apart
//! from Tauri so it can be tested against a real pipe without a window.

use std::collections::HashMap;
use std::future::Future;
use std::time::Duration;

use th_core::ipc::{
    self, ClientMessage, Event, PROTOCOL_VERSION, Request, Response, ServerMessage,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot};

use crate::lang::text;

/// What the app hears from the connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Update {
    /// A session is open: the service greeted us.
    Connected,
    /// The session ended or could not be opened; the client retries.
    Disconnected(String),
    Event(Event),
}

pub struct Options {
    pub pipe: String,
    /// Refuse a pipe that SYSTEM or Administrators do not own. Only tests, whose server is the
    /// test process itself, turn this off.
    pub verify_owner: bool,
    /// How long to wait before trying to connect again.
    pub retry: Duration,
}

/// Why the app is not connected, for the log.
pub const NOT_RUNNING: &str = "the TunnelHop service is not running";

/// The same, for the user: the answer to a request made while the service is away.
pub fn not_running() -> String {
    text("client.not_running", &[])
}

type Reply = oneshot::Sender<Result<Response, String>>;

/// Sends requests to the service. Cheap to clone; every clone talks through the same session.
#[derive(Clone)]
pub struct ServiceClient {
    requests: mpsc::Sender<(Request, Reply)>,
}

impl ServiceClient {
    /// The client, and the task that runs the connection: spawn it on the app's runtime. The
    /// task ends once every clone of the client is gone.
    pub fn new(
        options: Options,
        on_update: impl Fn(Update) + Send + Sync + 'static,
    ) -> (ServiceClient, impl Future<Output = ()> + Send + 'static) {
        let (requests, incoming) = mpsc::channel(32);
        (
            ServiceClient { requests },
            run(options, incoming, on_update),
        )
    }

    /// One request and its response, or why there is none.
    pub async fn request(&self, request: Request) -> Result<Response, String> {
        let (reply, answer) = oneshot::channel();
        self.requests
            .send((request, reply))
            .await
            .map_err(|_| text("client.closing", &[]))?;
        answer.await.map_err(|_| text("client.lost", &[]))?
    }
}

async fn run(
    options: Options,
    mut requests: mpsc::Receiver<(Request, Reply)>,
    on_update: impl Fn(Update),
) {
    // A failure is reported when it changes, not on every retry: while the service is down the
    // app would otherwise log and redraw every couple of seconds.
    let last_failure = std::sync::Mutex::new(None::<String>);
    let report = |update: Update| {
        let mut last = last_failure.lock().unwrap_or_else(|e| e.into_inner());
        match &update {
            Update::Connected => *last = None,
            Update::Disconnected(reason) if last.as_deref() == Some(reason) => return,
            Update::Disconnected(reason) => *last = Some(reason.clone()),
            Update::Event(_) => {}
        }
        drop(last);
        on_update(update);
    };
    loop {
        match session(&options, &mut requests, &report).await {
            Ok(()) => return,
            Err(reason) => report(Update::Disconnected(reason)),
        }
        // Until the next attempt, nothing can be delivered: say so rather than queue requests
        // the user would see happen much later.
        let retry = tokio::time::sleep(options.retry);
        tokio::pin!(retry);
        loop {
            tokio::select! {
                _ = &mut retry => break,
                request = requests.recv() => match request {
                    Some((_, reply)) => {
                        let _ = reply.send(Err(not_running()));
                    }
                    None => return,
                },
            }
        }
    }
}

#[cfg(windows)]
async fn connect(
    options: &Options,
) -> std::io::Result<impl tokio::io::AsyncRead + tokio::io::AsyncWrite> {
    if options.verify_owner {
        th_service::pipe::connect(&options.pipe).await
    } else {
        th_service::pipe::connect_unverified(&options.pipe).await
    }
}

#[cfg(not(windows))]
async fn connect(
    _: &Options,
) -> std::io::Result<impl tokio::io::AsyncRead + tokio::io::AsyncWrite> {
    Err::<tokio::io::DuplexStream, _>(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "the TunnelHop service runs on Windows only",
    ))
}

/// One session: `Ok` when the app dropped every client, `Err` with the reason when the pipe
/// could not be opened or went away.
async fn session(
    options: &Options,
    requests: &mut mpsc::Receiver<(Request, Reply)>,
    on_update: &impl Fn(Update),
) -> Result<(), String> {
    let pipe = connect(options).await.map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => NOT_RUNNING.to_string(),
        _ => format!("cannot reach the TunnelHop service: {e}"),
    })?;
    let (reader, mut writer) = tokio::io::split(pipe);
    let mut lines = BufReader::new(reader).lines();
    let lost = |e: std::io::Error| format!("the connection to the service failed: {e}");

    let hello = ClientMessage {
        id: 0,
        request: Request::Hello {
            version: PROTOCOL_VERSION,
        },
    };
    writer.write_all(&ipc::encode(&hello)).await.map_err(lost)?;
    loop {
        let line = lines
            .next_line()
            .await
            .map_err(lost)?
            .ok_or("the service closed the connection")?;
        match ipc::decode::<ServerMessage>(line.as_bytes()) {
            Ok(ServerMessage::Response {
                id: 0,
                response: Response::Hello { .. },
            }) => break,
            Ok(ServerMessage::Response {
                id: 0,
                response: Response::Error { message },
            }) => return Err(format!("the service refused this app: {message}")),
            other => log::warn!("unexpected before the greeting: {other:?}"),
        }
    }
    on_update(Update::Connected);

    // Dropped with the session, which answers every pending request with "lost".
    let mut pending: HashMap<u64, Reply> = HashMap::new();
    let mut next_id = 1u64;
    loop {
        tokio::select! {
            request = requests.recv() => {
                let Some((request, reply)) = request else {
                    return Ok(());
                };
                let id = next_id;
                next_id += 1;
                writer
                    .write_all(&ipc::encode(&ClientMessage { id, request }))
                    .await
                    .map_err(lost)?;
                pending.insert(id, reply);
            }
            line = lines.next_line() => {
                let line = line.map_err(lost)?.ok_or("the service closed the connection")?;
                match ipc::decode::<ServerMessage>(line.as_bytes()) {
                    Ok(ServerMessage::Response { id, response }) => {
                        if let Some(reply) = pending.remove(&id) {
                            let _ = reply.send(Ok(response));
                        }
                    }
                    Ok(ServerMessage::Event { event }) => on_update(Update::Event(event)),
                    Err(e) => log::warn!("a message from the service did not parse: {e}"),
                }
            }
        }
    }
}
