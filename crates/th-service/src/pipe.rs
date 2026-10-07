//! The named pipe the UI talks to, `\\.\pipe\tunnelhop` — SPEC §2.2 and §6.
//!
//! Everything arriving here is untrusted. A connection must open with `Hello` within
//! [`HELLO_TIMEOUT`]; after that every request is validated before the [`Handler`] sees it.
//! A line that is too large or does not parse closes the connection, since there is no request
//! id to answer it by.

use std::future::Future;
use std::io;
use std::os::windows::io::IntoRawHandle;
use std::sync::Arc;
use std::time::Duration;

use th_core::ipc::{self, ClientMessage, Event, FrameBuffer, Request, Response, ServerMessage};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::windows::named_pipe::{NamedPipeClient, NamedPipeServer, ServerOptions};
use tokio::sync::{Semaphore, broadcast};
use windows_sys::Win32::Foundation::ERROR_PIPE_BUSY;

use crate::win::SecurityDescriptor;

pub const PIPE_NAME: &str = r"\\.\pipe\tunnelhop";
/// SYSTEM, Administrators and the pipe's owner (the process that created it, SYSTEM in the
/// service): full control, which includes creating further instances. Interactive users (the
/// logged-in user running the UI): `0x12019b`, which is read and write *without*
/// `FILE_CREATE_PIPE_INSTANCE`. Plain `GW` would include it (it is `FILE_APPEND_DATA` on a
/// file), letting any user add an instance of this pipe and impersonate the service to the UI.
/// Clients must therefore ask for exactly these rights rather than `GENERIC_WRITE`: see
/// [`connect`].
pub const PIPE_SDDL: &str = "D:(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;OW)(A;;0x12019b;;;IU)";
/// How long a new connection may take to say `Hello`.
pub const HELLO_TIMEOUT: Duration = Duration::from_secs(10);
/// Connections served at once; more are closed straight away.
pub const MAX_CLIENTS: usize = 16;

const READ_CHUNK: usize = 64 * 1024;

/// Open a client end of the service's pipe, refusing a server that SYSTEM or Administrators do
/// not own. While the service is stopped any user can create a pipe under its name; without this
/// check the UI would hand that impostor whatever it sends, credentials included. Must run
/// inside a Tokio runtime.
pub async fn connect(name: &str) -> io::Result<NamedPipeClient> {
    let client = connect_unverified(name).await?;
    if !crate::win::owned_by_system_or_admins(&client)? {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{name} is not served by the TunnelHop service; refusing to use it"),
        ));
    }
    Ok(client)
}

/// [`connect`] without checking who serves the pipe: only for tests, whose server is the test
/// process itself.
pub async fn connect_unverified(name: &str) -> io::Result<NamedPipeClient> {
    const ATTEMPTS: u32 = 20;
    for attempt in 1..=ATTEMPTS {
        match crate::win::open_pipe_client(name) {
            Ok(handle) => {
                // SAFETY: the handle was opened overlapped, as Tokio requires, and is owned here.
                return unsafe { NamedPipeClient::from_raw_handle(handle.into_raw_handle()) };
            }
            Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY as i32) && attempt < ATTEMPTS => {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(e) => return Err(e),
        }
    }
    unreachable!("the last attempt returns")
}

/// What the service does with a request that passed validation. `Hello` and `Ping` are
/// answered by the pipe itself and never reach it.
pub trait Handler: Send + Sync + 'static {
    fn handle(&self, request: Request) -> impl Future<Output = Response> + Send;
}

/// The pipe, bound and waiting for its first client.
pub struct Listener {
    name: String,
    descriptor: SecurityDescriptor,
    next: NamedPipeServer,
}

impl Listener {
    /// Create the first instance of the pipe. Fails if the name already exists: another
    /// process holding it could impersonate the service to the UI. Must run inside a Tokio
    /// runtime.
    pub fn bind(name: &str) -> io::Result<Listener> {
        let descriptor = SecurityDescriptor::from_sddl(PIPE_SDDL)?;
        let next = create(name, &descriptor, true)?;
        Ok(Listener {
            name: name.to_string(),
            descriptor,
            next,
        })
    }

    /// The instance waiting for the next client.
    pub fn pending(&self) -> &NamedPipeServer {
        &self.next
    }

    /// Accept clients until `shutdown` completes. Each client gets its own task and every
    /// event sent on `events`.
    pub async fn serve<H: Handler>(
        mut self,
        handler: Arc<H>,
        events: broadcast::Sender<Event>,
        shutdown: impl Future<Output = ()>,
    ) -> io::Result<()> {
        let slots = Arc::new(Semaphore::new(MAX_CLIENTS));
        tokio::pin!(shutdown);
        loop {
            tokio::select! {
                () = &mut shutdown => return Ok(()),
                connected = self.next.connect() => {
                    let fresh = create(&self.name, &self.descriptor, false).inspect_err(|e| {
                        log::error!("cannot create another instance of the pipe: {e}");
                    })?;
                    let pipe = std::mem::replace(&mut self.next, fresh);
                    if let Err(e) = connected {
                        log::warn!("pipe client failed to connect: {e}");
                        continue;
                    }
                    let Ok(slot) = slots.clone().try_acquire_owned() else {
                        log::warn!("more than {MAX_CLIENTS} pipe clients; closing the newest");
                        continue;
                    };
                    let (handler, events) = (handler.clone(), events.subscribe());
                    tokio::spawn(async move {
                        session(pipe, &*handler, events).await;
                        drop(slot);
                    });
                }
            }
        }
    }
}

fn create(name: &str, descriptor: &SecurityDescriptor, first: bool) -> io::Result<NamedPipeServer> {
    let mut attributes = descriptor.attributes();
    let mut options = ServerOptions::new();
    options
        .first_pipe_instance(first)
        .reject_remote_clients(true);
    // SAFETY: `attributes` points at a descriptor that outlives the call, which copies it.
    unsafe { options.create_with_security_attributes_raw(name, (&raw mut attributes).cast()) }
}

/// Why a connection stopped reading.
enum End {
    Closed,
    Broken(String),
}

/// One client, from `Hello` to disconnect.
async fn session<S, H>(pipe: S, handler: &H, mut events: broadcast::Receiver<Event>)
where
    S: AsyncRead + AsyncWrite,
    H: Handler,
{
    let (mut reader, mut writer) = tokio::io::split(pipe);
    let mut frames = FrameBuffer::default();
    let mut chunk = vec![0u8; READ_CHUNK];

    let hello = tokio::time::timeout(
        HELLO_TIMEOUT,
        next_message(&mut reader, &mut frames, &mut chunk),
    )
    .await;
    let hello = match hello {
        Ok(Ok(message)) => message,
        Ok(Err(End::Broken(why))) => return log::warn!("pipe client dropped: {why}"),
        Ok(Err(End::Closed)) => return,
        Err(_) => return log::warn!("pipe client did not say hello in time"),
    };
    let (greeting, accepted) = match hello.request {
        Request::Hello { version } => match ipc::check_version(version) {
            Ok(response) => (response, true),
            Err(e) => (error(e), false),
        },
        _ => (error("the first message must be hello"), false),
    };
    if write(&mut writer, &response(hello.id, greeting))
        .await
        .is_err()
        || !accepted
    {
        return;
    }

    let mut events_open = true;
    loop {
        let outgoing = tokio::select! {
            message = next_message(&mut reader, &mut frames, &mut chunk) => match message {
                Ok(message) => response(message.id, respond(handler, message.request).await),
                Err(End::Closed) => return,
                Err(End::Broken(why)) => return log::warn!("pipe client dropped: {why}"),
            },
            event = events.recv(), if events_open => match event {
                Ok(event) => ServerMessage::Event { event },
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    log::warn!("a pipe client fell {skipped} events behind");
                    continue;
                }
                Err(broadcast::error::RecvError::Closed) => {
                    events_open = false;
                    continue;
                }
            },
        };
        if write(&mut writer, &outgoing).await.is_err() {
            return;
        }
    }
}

async fn respond<H: Handler>(handler: &H, request: Request) -> Response {
    match request {
        Request::Hello { .. } => error("hello was already exchanged"),
        Request::Ping => Response::Pong,
        request => match request.validate() {
            Ok(()) => handler.handle(request).await,
            Err(e) => error(e),
        },
    }
}

fn error(message: impl ToString) -> Response {
    Response::Error {
        message: message.to_string(),
    }
}

fn response(id: u64, response: Response) -> ServerMessage {
    ServerMessage::Response { id, response }
}

/// The next complete message. Cancel-safe: bytes already read stay in `frames`.
async fn next_message<R: AsyncRead + Unpin>(
    reader: &mut R,
    frames: &mut FrameBuffer,
    chunk: &mut [u8],
) -> Result<ClientMessage, End> {
    loop {
        match frames.next_frame() {
            Ok(Some(line)) => {
                return ipc::decode(&line).map_err(|e| End::Broken(e.to_string()));
            }
            Ok(None) => {}
            Err(e) => return Err(End::Broken(e.to_string())),
        }
        match reader.read(chunk).await {
            Ok(0) => return Err(End::Closed),
            Ok(n) => frames.push(&chunk[..n]),
            Err(e) => return Err(End::Broken(e.to_string())),
        }
    }
}

async fn write<W: AsyncWrite + Unpin>(writer: &mut W, message: &ServerMessage) -> io::Result<()> {
    writer.write_all(&ipc::encode(message)).await?;
    writer.flush().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_pipe_carries_exactly_its_sddl() {
        let name = format!(r"\\.\pipe\tunnelhop-dacl-{}", std::process::id());
        let listener = Listener::bind(&name).unwrap();
        let dacl = crate::win::handle_dacl_sddl(listener.pending()).unwrap();
        // Generic rights are stored mapped to the pipe's own (GA is FA). Interactive users must
        // not have FILE_CREATE_PIPE_INSTANCE (0x4): 0x12019b, not 0x12019f.
        assert_eq!(
            dacl,
            "D:(A;;FA;;;SY)(A;;FA;;;BA)(A;;FA;;;OW)(A;;0x12019b;;;IU)"
        );
    }
}
