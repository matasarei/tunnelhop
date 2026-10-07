//! The management client against a fake OpenVPN on localhost that replays a transcript: what
//! OpenVPN sends, and exactly what the client must answer.

use std::net::SocketAddr;

use th_core::ipc::Secret;
use th_service::drivers::ovpn_mgmt::{Management, ManagementError, Notification, State};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

enum Step {
    /// OpenVPN sends these bytes as they are (line breaks included where it sends them).
    Send(&'static str),
    /// The client must send exactly this line.
    Expect(&'static str),
    /// OpenVPN closes the connection.
    Close,
}

use Step::*;

/// A fake OpenVPN that plays `steps` to its first client. Await the handle to surface a wrong
/// line from the client as a test failure.
async fn openvpn(steps: Vec<Step>) -> (SocketAddr, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let (reader, mut writer) = socket.into_split();
        let mut lines = BufReader::new(reader).lines();
        for step in steps {
            match step {
                Send(bytes) => writer.write_all(bytes.as_bytes()).await.unwrap(),
                Expect(line) => {
                    let got = lines
                        .next_line()
                        .await
                        .unwrap()
                        .expect("the client hung up");
                    assert_eq!(got, line, "the client sent the wrong line");
                }
                Close => return,
            }
        }
        // Hold the connection until the client is done.
        let _ = lines.next_line().await;
    });
    (addr, task)
}

fn password() -> Secret {
    Secret::new("0123456789abcdef")
}

#[tokio::test]
async fn a_wrong_management_password_is_reported() {
    let (addr, server) = openvpn(vec![
        Send("ENTER PASSWORD:"),
        Expect("0123456789abcdef"),
        Send("ERROR: bad password\r\n"),
        Close,
    ])
    .await;
    let result = Management::connect(addr, &password()).await;
    assert!(
        matches!(result, Err(ManagementError::BadPassword)),
        "{:?}",
        result.err()
    );
    server.await.unwrap();
}

#[tokio::test]
async fn a_port_that_does_not_ask_for_the_password_is_not_ours() {
    let (addr, _server) = openvpn(vec![Send(
        ">INFO:OpenVPN Management Interface Version 5 -- type 'help' for more info\r\n",
    )])
    .await;
    let result = Management::connect(addr, &password()).await;
    assert!(
        matches!(result, Err(ManagementError::Unexpected(_))),
        "{:?}",
        result.err()
    );
}

#[tokio::test]
async fn a_connection_goes_from_hold_to_connected() {
    let (addr, server) = openvpn(vec![
        Send("ENTER PASSWORD:"),
        Expect("0123456789abcdef"),
        Send("SUCCESS: password is correct\r\n"),
        Send(">INFO:OpenVPN Management Interface Version 5 -- type 'help' for more info\r\n"),
        Send(">HOLD:Waiting for hold release:0\r\n"),
        Expect("state on"),
        Send("SUCCESS: real-time state notification set to ON\r\n"),
        Expect("hold release"),
        // A notification can arrive before the answer to a command.
        Send(">STATE:1700000000,WAIT,,,,,,\r\n"),
        Send("SUCCESS: hold release succeeded\r\n"),
        Send(">STATE:1700000001,AUTH,,,,,,\r\n"),
        Send(">STATE:1700000002,ASSIGN_IP,,10.77.3.2,,,,\r\n"),
        Send(">STATE:1700000003,CONNECTED,SUCCESS,10.77.3.2,192.168.77.1,1194,,\r\n"),
    ])
    .await;
    let mut management = Management::connect(addr, &password()).await.unwrap();
    management.start().await.unwrap();

    let mut seen = Vec::new();
    loop {
        match management.next_notification().await.unwrap() {
            Notification::State(change) if change.state == State::Connected => {
                assert_eq!(change.local_ip, Some("10.77.3.2".parse().unwrap()));
                break;
            }
            Notification::State(change) => seen.push(format!("{:?}", change.state)),
            Notification::Hold => seen.push("hold".into()),
            Notification::Other(_) => {}
            other => panic!("unexpected {other:?}"),
        }
    }
    assert_eq!(
        seen,
        [
            "hold",
            r#"Other("WAIT")"#,
            r#"Other("AUTH")"#,
            r#"Other("ASSIGN_IP")"#
        ],
        "every notification, in order, none lost to a command's answer"
    );
    drop(management);
    server.await.unwrap();
}

#[tokio::test]
async fn credentials_with_quotes_and_backslashes_stay_one_argument_each() {
    let (addr, server) = openvpn(vec![
        Send("ENTER PASSWORD:"),
        Expect("0123456789abcdef"),
        Send("SUCCESS: password is correct\r\n"),
        Send(">PASSWORD:Need 'Auth' username/password\r\n"),
        Expect(r#"username "Auth" "al\"ice""#),
        Send("SUCCESS: 'Auth' username entered, but not yet verified\r\n"),
        Expect(r#"password "Auth" "p\\a\"ss word\\""#),
        Send("SUCCESS: 'Auth' password entered, but not yet verified\r\n"),
        Send(">PASSWORD:Verification Failed: 'Auth'\r\n"),
    ])
    .await;
    let mut management = Management::connect(addr, &password()).await.unwrap();
    let Notification::NeedCredentials { realm } = management.next_notification().await.unwrap()
    else {
        panic!("expected a credential prompt");
    };
    management
        .send_credentials(&realm, r#"al"ice"#, &Secret::new(r#"p\a"ss word\"#))
        .await
        .unwrap();
    assert_eq!(
        management.next_notification().await.unwrap(),
        Notification::CredentialsRefused {
            realm: "Auth".into()
        }
    );
    drop(management);
    server.await.unwrap();
}

#[tokio::test]
async fn a_refused_command_names_only_its_first_word() {
    let (addr, server) = openvpn(vec![
        Send("ENTER PASSWORD:"),
        Expect("0123456789abcdef"),
        Send("SUCCESS: password is correct\r\n"),
        Expect(r#"password "Auth" "hunter2""#),
        Send("ERROR: password command failed\r\n"),
    ])
    .await;
    let mut management = Management::connect(addr, &password()).await.unwrap();
    let error = management
        .command(r#"password "Auth" "hunter2""#)
        .await
        .unwrap_err();
    let message = error.to_string();
    assert!(message.contains("\"password\""), "{message}");
    assert!(
        !message.contains("hunter2"),
        "a credential reached an error: {message}"
    );
    drop(management);
    server.await.unwrap();
}

#[tokio::test]
async fn sigterm_asks_openvpn_to_exit() {
    let (addr, server) = openvpn(vec![
        Send("ENTER PASSWORD:"),
        Expect("0123456789abcdef"),
        Send("SUCCESS: password is correct\r\n"),
        Expect("signal SIGTERM"),
        Send("SUCCESS: signal SIGTERM thrown\r\n"),
        Send(">STATE:1700000009,EXITING,SIGTERM,,,,,\r\n"),
    ])
    .await;
    let mut management = Management::connect(addr, &password()).await.unwrap();
    management.sigterm().await.unwrap();
    let Notification::State(change) = management.next_notification().await.unwrap() else {
        panic!("expected a state");
    };
    assert_eq!(change.state, State::Exiting);
    drop(management);
    server.await.unwrap();
}

#[tokio::test]
async fn a_server_side_close_is_reported_not_hung_on() {
    let (addr, server) = openvpn(vec![
        Send("ENTER PASSWORD:"),
        Expect("0123456789abcdef"),
        Send("SUCCESS: password is correct\r\n"),
        Send(">STATE:1700000000,CONNECTED,SUCCESS,10.77.3.2,,,,\r\n"),
        Close,
    ])
    .await;
    let mut management = Management::connect(addr, &password()).await.unwrap();
    assert!(matches!(
        management.next_notification().await.unwrap(),
        Notification::State(_)
    ));
    let next = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        management.next_notification(),
    )
    .await
    .expect("a closed connection is noticed, not waited on");
    assert!(matches!(next, Err(ManagementError::Closed)), "{next:?}");
    server.await.unwrap();
}

#[tokio::test]
async fn an_endless_line_is_refused() {
    let flood: &'static str = Box::leak("x".repeat(70 * 1024).into_boxed_str());
    let (addr, _server) = openvpn(vec![
        Send("ENTER PASSWORD:"),
        Expect("0123456789abcdef"),
        Send("SUCCESS: password is correct\r\n"),
        Send(flood),
    ])
    .await;
    let mut management = Management::connect(addr, &password()).await.unwrap();
    let next = management.next_notification().await;
    assert!(matches!(next, Err(ManagementError::TooLong)), "{next:?}");
}
