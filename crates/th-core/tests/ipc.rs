use std::net::{IpAddr, Ipv4Addr};

use serde_json::json;
use th_core::bundle::MAX_ZIP_BYTES;
use th_core::ipc::{
    self, Availability, BundleInfo, ClientMessage, Event, FrameBuffer, IpcError, MAX_FRAME_BYTES,
    PROTOCOL_VERSION, ProfileInfo, Request, Response, RuntimeInfo, ServerMessage, StatusReport,
};
use th_core::manifest::Protocol;
use th_core::rotation::State;

fn report() -> StatusReport {
    StatusReport {
        state: State::Connected,
        profile_id: Some("ua-wg-01".into()),
        public_ip: Some(IpAddr::V4(Ipv4Addr::new(198, 51, 100, 50))),
        killswitch_engaged: true,
        bundle: Some(BundleInfo {
            bundle_id: "0123456789abcdef".into(),
            bundle_name: "Ukraine".into(),
            profiles: vec![ProfileInfo {
                id: "ua-wg-01".into(),
                name: "Kyiv #1".into(),
                protocol: Protocol::Wireguard,
                has_credentials: true,
            }],
        }),
        auto_rotate: true,
        interval_minutes: 15,
        runtimes: vec![
            RuntimeInfo {
                protocol: Protocol::Wireguard,
                availability: Availability::Available {
                    version: "0.5.3".into(),
                },
            },
            RuntimeInfo {
                protocol: Protocol::Openvpn,
                availability: Availability::TooOld {
                    found: "2.5.9".into(),
                    required: "2.6.0".into(),
                },
            },
        ],
    }
}

#[test]
fn every_message_round_trips() {
    let requests = vec![
        Request::Hello {
            version: PROTOCOL_VERSION,
        },
        Request::Ping,
        Request::GetStatus,
        Request::ImportBundle {
            zip_base64: ipc::encode_zip(b"PK\x03\x04"),
        },
        Request::SelectBundle {
            bundle_id: "0123456789abcdef".into(),
        },
        Request::Connect { profile_id: None },
        Request::Connect {
            profile_id: Some("ua-wg-01".into()),
        },
        Request::RotateNow,
        Request::SetAutoRotate { enabled: false },
        Request::SetInterval { minutes: 30 },
        Request::Disconnect,
        Request::Shutdown { disconnect: true },
    ];
    for (id, request) in requests.into_iter().enumerate() {
        let message = ClientMessage {
            id: id as u64,
            request,
        };
        let line = ipc::encode(&message);
        assert_eq!(line.last(), Some(&b'\n'));
        assert_eq!(
            line.iter().filter(|&&b| b == b'\n').count(),
            1,
            "one message, one line"
        );
        let back: ClientMessage = ipc::decode(&line[..line.len() - 1]).unwrap();
        assert_eq!(back, message);
    }

    let responses = vec![
        Response::Hello { version: 1 },
        Response::Pong,
        Response::Status { report: report() },
        Response::Imported {
            bundle: report().bundle.unwrap(),
            already_present: false,
            needs_credentials: vec!["ua-ovpn-01".into()],
        },
        Response::ImportFailed {
            errors: vec!["openvpn/a.ovpn:3: directive \"up\" is not allowed".into()],
        },
        Response::Ok,
        Response::Error {
            message: "nope".into(),
        },
    ];
    for response in responses {
        let message = ServerMessage::Response { id: 9, response };
        let back: ServerMessage = ipc::decode(ipc::encode(&message).trim_ascii_end()).unwrap();
        assert_eq!(back, message);
    }
    for event in [
        Event::Status { report: report() },
        Event::Alert {
            message: "blocked".into(),
        },
    ] {
        let message = ServerMessage::Event { event };
        let back: ServerMessage = ipc::decode(ipc::encode(&message).trim_ascii_end()).unwrap();
        assert_eq!(back, message);
    }
}

#[test]
fn wire_format_is_stable() {
    let line = ipc::encode(&ClientMessage {
        id: 3,
        request: Request::SetInterval { minutes: 5 },
    });
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&line).unwrap(),
        json!({ "id": 3, "request": { "type": "set_interval", "minutes": 5 } })
    );
}

#[test]
fn version_mismatch_is_refused() {
    assert_eq!(
        ipc::check_version(PROTOCOL_VERSION),
        Ok(Response::Hello {
            version: PROTOCOL_VERSION
        })
    );
    let error = ipc::check_version(2).unwrap_err();
    assert_eq!(error, IpcError::VersionMismatch { client: 2 });
    assert!(error.to_string().contains("service speaks 1"));
}

#[test]
fn malformed_and_unknown_messages_are_refused() {
    for line in [
        &b"not json"[..],
        br#"{"id":1,"request":{"type":"format_disk"}}"#,
        br#"{"id":1,"request":{"type":"set_interval","minutes":5,"extra":true}}"#,
        br#"{"id":1,"request":{"type":"set_interval","minutes":-5}}"#,
        br#"{"request":{"type":"ping"}}"#,
    ] {
        assert!(
            matches!(
                ipc::decode::<ClientMessage>(line),
                Err(IpcError::Malformed(_))
            ),
            "{}",
            String::from_utf8_lossy(line)
        );
    }
}

#[test]
fn requests_are_validated() {
    assert!(Request::SetInterval { minutes: 0 }.validate().is_err());
    assert!(Request::SetInterval { minutes: 1441 }.validate().is_err());
    assert!(Request::SetInterval { minutes: 60 }.validate().is_ok());
    assert!(
        Request::SelectBundle {
            bundle_id: "../../etc".into()
        }
        .validate()
        .is_err()
    );
    assert!(
        Request::SelectBundle {
            bundle_id: "0123456789abcdeZ".into()
        }
        .validate()
        .is_err()
    );
    assert!(
        Request::SelectBundle {
            bundle_id: "0123456789ABCDEF".into()
        }
        .validate()
        .is_err(),
        "bundle ids are lowercase"
    );
    assert!(
        Request::Connect {
            profile_id: Some(String::new())
        }
        .validate()
        .is_err()
    );
    assert!(
        Request::ImportBundle {
            zip_base64: "%%%".into()
        }
        .validate()
        .is_err()
    );
}

#[test]
fn import_payload_is_capped_at_20_mb() {
    let max = vec![0u8; MAX_ZIP_BYTES];
    assert_eq!(
        ipc::decode_zip(&ipc::encode_zip(&max)).unwrap().len(),
        MAX_ZIP_BYTES
    );

    let over = ipc::encode_zip(&vec![0u8; MAX_ZIP_BYTES + 3]);
    let error = Request::ImportBundle { zip_base64: over }
        .validate()
        .unwrap_err();
    assert!(error.to_string().contains("larger than 20 MB"));

    // A maximal import still fits in one frame.
    let frame = ipc::encode(&ClientMessage {
        id: u64::MAX,
        request: Request::ImportBundle {
            zip_base64: ipc::encode_zip(&max),
        },
    });
    assert!(frame.len() <= MAX_FRAME_BYTES);
}

#[test]
fn frame_buffer_splits_lines_across_reads() {
    let mut frames = FrameBuffer::default();
    frames.push(b"{\"a\":");
    assert_eq!(frames.next_frame(), Ok(None));
    frames.push(b"1}\r\n{\"b\":2}\n{\"c\"");
    assert_eq!(frames.next_frame(), Ok(Some(b"{\"a\":1}".to_vec())));
    assert_eq!(frames.next_frame(), Ok(Some(b"{\"b\":2}".to_vec())));
    assert_eq!(frames.next_frame(), Ok(None));
    frames.push(b":3}\n");
    assert_eq!(frames.next_frame(), Ok(Some(b"{\"c\":3}".to_vec())));
}

#[test]
fn a_peer_that_never_ends_a_line_is_cut_off() {
    let mut frames = FrameBuffer::default();
    frames.push(&vec![b'x'; MAX_FRAME_BYTES + 1]);
    assert_eq!(frames.next_frame(), Err(IpcError::TooLarge));
    assert_eq!(
        ipc::decode::<ClientMessage>(&vec![b' '; MAX_FRAME_BYTES + 1]),
        Err(IpcError::TooLarge)
    );
}

fn set_credentials(profile_ids: Vec<&str>, username: &str, password: &str) -> Request {
    Request::SetCredentials {
        bundle_id: "0123456789abcdef".into(),
        profile_ids: profile_ids.into_iter().map(String::from).collect(),
        username: username.into(),
        password: ipc::Secret::new(password),
    }
}

#[test]
fn set_credentials_round_trips_and_validates() {
    let request = set_credentials(vec![], "user+f1", "p@ss \"quoted\" \\ back");
    assert!(request.validate().is_ok());
    let message = ClientMessage { id: 1, request };
    let back: ClientMessage = ipc::decode(ipc::encode(&message).trim_ascii_end()).unwrap();
    assert_eq!(back, message);

    assert!(
        set_credentials(vec!["ua-ovpn-39"], "u", "p")
            .validate()
            .is_ok()
    );
    for (request, needle) in [
        (
            set_credentials(vec![], "", "p"),
            "username must be 1 to 256",
        ),
        (
            set_credentials(vec![], "u", ""),
            "password must be 1 to 256",
        ),
        (
            set_credentials(vec![], &"u".repeat(257), "p"),
            "username must be 1 to 256",
        ),
        (
            set_credentials(vec![], "u", "line\nbreak"),
            "password must not contain line breaks",
        ),
        (
            set_credentials(vec![], "u\r", "p"),
            "username must not contain line breaks",
        ),
        (
            set_credentials(vec![], "u", "nul\0"),
            "password must not contain line breaks or NUL",
        ),
        (set_credentials(vec![""], "u", "p"), "profile id must match"),
    ] {
        let error = request.validate().unwrap_err().to_string();
        assert!(error.contains(needle), "{error}");
    }
    let mut bad_bundle = set_credentials(vec![], "u", "p");
    if let Request::SetCredentials { bundle_id, .. } = &mut bad_bundle {
        *bundle_id = "../../etc".into();
    }
    assert!(bad_bundle.validate().is_err());
}

#[test]
fn profile_ids_follow_the_manifest_rule() {
    for bad in ["../../x", "UA-WG-01", "a/b", "a\\b", "a.b", ""] {
        let connect = Request::Connect {
            profile_id: Some(bad.into()),
        };
        assert!(connect.validate().is_err(), "connect {bad:?}");
        assert!(
            set_credentials(vec![bad], "u", "p").validate().is_err(),
            "set_credentials {bad:?}"
        );
    }
    assert!(
        Request::Connect {
            profile_id: Some("ua-wg-01".into())
        }
        .validate()
        .is_ok()
    );
}

#[test]
fn passwords_never_appear_in_debug_output() {
    let request = set_credentials(vec![], "alice", "hunter2-secret");
    let printed = format!("{request:?}");
    assert!(printed.contains("alice"));
    assert!(!printed.contains("hunter2-secret"), "{printed}");
    assert!(printed.contains("<redacted>"));
}

#[test]
fn secrets_are_wiped_when_dropped() {
    fn wiped_on_drop<T: zeroize::ZeroizeOnDrop>() {}
    wiped_on_drop::<ipc::Secret>();
    wiped_on_drop::<th_core::sanitize::Credentials>();

    let mut secret = ipc::Secret::new("hunter2");
    zeroize::Zeroize::zeroize(&mut secret);
    assert_eq!(secret.expose(), "");
}
