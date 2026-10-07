//! The app's service client against a real named pipe served by th-service's own listener.
#![cfg(windows)]

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use th_app_lib::pipe_client::{NOT_RUNNING, Options, ServiceClient, Update};
use th_core::ipc::{Event, Request, Response};
use th_service::pipe::{Handler, Listener};
use tokio::sync::{broadcast, mpsc};
use tokio::time::timeout;

/// Answers `GetStatus` with `Ok`, and names anything else in an error.
struct Service;

impl Handler for Service {
    async fn handle(&self, request: Request) -> Response {
        match request {
            Request::GetStatus => Response::Ok,
            other => Response::Error {
                message: format!("saw {other:?}"),
            },
        }
    }
}

/// The service, on a runtime of its own: stopping it shuts that runtime down, which closes every
/// open session as the service process exiting would. Stopping only the listener would not:
/// sessions already accepted run on as their own tasks.
struct Server {
    events: broadcast::Sender<Event>,
    runtime: tokio::runtime::Runtime,
}

impl Server {
    fn stop(self) {
        self.runtime.shutdown_background();
    }
}

fn unique_name() -> String {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    format!(
        r"\\.\pipe\tunnelhop-app-test-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

fn serve(name: &str) -> Server {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap();
    let listener = {
        let _inside = runtime.enter();
        Listener::bind(name).unwrap()
    };
    let (events, _) = broadcast::channel(16);
    let serving = listener.serve(Arc::new(Service), events.clone(), std::future::pending());
    runtime.spawn(serving);
    Server { events, runtime }
}

fn client(name: &str) -> (ServiceClient, mpsc::UnboundedReceiver<Update>) {
    let (updates, heard) = mpsc::unbounded_channel();
    let options = Options {
        pipe: name.to_string(),
        verify_owner: false,
        retry: Duration::from_millis(100),
    };
    let (client, connection) = ServiceClient::new(options, move |update| {
        let _ = updates.send(update);
    });
    tokio::spawn(connection);
    (client, heard)
}

async fn next(heard: &mut mpsc::UnboundedReceiver<Update>) -> Update {
    timeout(Duration::from_secs(5), heard.recv())
        .await
        .expect("an update within 5 s")
        .expect("the connection task is running")
}

#[tokio::test]
async fn requests_are_answered_and_events_forwarded() {
    let name = unique_name();
    let server = serve(&name);
    let (client, mut heard) = client(&name);
    assert_eq!(next(&mut heard).await, Update::Connected);

    assert_eq!(client.request(Request::GetStatus).await, Ok(Response::Ok));
    // Several in flight at once still each get their own answer.
    let (a, b) = tokio::join!(
        client.request(Request::RotateNow),
        client.request(Request::GetStatus)
    );
    assert!(matches!(a, Ok(Response::Error { message }) if message.contains("RotateNow")));
    assert_eq!(b, Ok(Response::Ok));

    let alert = Event::Alert {
        message: "every attempt failed".into(),
    };
    server.events.send(alert.clone()).unwrap();
    assert_eq!(next(&mut heard).await, Update::Event(alert));
    server.stop();
}

#[tokio::test]
async fn a_missing_service_is_reported_and_the_client_reconnects_when_it_returns() {
    let name = unique_name();
    let (client, mut heard) = client(&name);
    assert_eq!(
        next(&mut heard).await,
        Update::Disconnected(NOT_RUNNING.into())
    );
    // Answered at once rather than queued until some later connection.
    let answer = timeout(Duration::from_secs(1), client.request(Request::GetStatus))
        .await
        .expect("answered without waiting for the service");
    assert_eq!(answer, Err(NOT_RUNNING.into()));

    let server = serve(&name);
    loop {
        if next(&mut heard).await == Update::Connected {
            break;
        }
    }
    assert_eq!(client.request(Request::GetStatus).await, Ok(Response::Ok));

    // The service stops: the session ends, and is reported.
    server.stop();
    loop {
        if let Update::Disconnected(_) = next(&mut heard).await {
            break;
        }
    }
}
