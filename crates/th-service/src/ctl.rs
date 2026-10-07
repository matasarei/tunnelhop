//! `th-service ctl`: drive the running service over its pipe, exactly as the UI will. It lets
//! the leak test rotate twenty times without a UI, and gives a quick look at the service.
//!
//! It runs as whoever starts it, connects with `pipe::connect` (so it refuses an impostor pipe),
//! and reads any file itself: the service only ever receives bytes.

use std::error::Error;

use th_core::ipc::{self, ClientMessage, PROTOCOL_VERSION, Request, Response, ServerMessage};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::pipe::{self, PIPE_NAME};

pub const USAGE: &str = "usage: th-service ctl <command>

  status                      the service's state
  import <bundle.zip>         import a bundle (prints its id)
  select <bundle id>          make an imported bundle the active one
  connect [profile]           connect to a profile, or a random one
  rotate                      rotate now
  disconnect                  disconnect and lift the kill-switch
  interval <minutes>          set the rotation interval
  auto <on|off>               turn automatic rotation on or off
  watch                       print the service's events as they happen";

/// The request a command line asks for, or why it is not one.
pub fn request(arguments: &[String]) -> Result<Option<Request>, String> {
    let words: Vec<&str> = arguments.iter().map(String::as_str).collect();
    let request = match words.as_slice() {
        ["status"] => Request::GetStatus,
        ["import", path] => {
            let bytes = std::fs::read(path).map_err(|e| format!("cannot read {path}: {e}"))?;
            Request::ImportBundle {
                zip_base64: ipc::encode_zip(&bytes),
            }
        }
        ["select", id] => Request::SelectBundle {
            bundle_id: id.to_string(),
        },
        ["connect"] => Request::Connect { profile_id: None },
        ["connect", profile] => Request::Connect {
            profile_id: Some(profile.to_string()),
        },
        ["rotate"] => Request::RotateNow,
        ["disconnect"] => Request::Disconnect,
        ["interval", minutes] => Request::SetInterval {
            minutes: minutes
                .parse()
                .map_err(|_| format!("\"{minutes}\" is not a number of minutes"))?,
        },
        ["auto", "on"] => Request::SetAutoRotate { enabled: true },
        ["auto", "off"] => Request::SetAutoRotate { enabled: false },
        ["watch"] => return Ok(None),
        _ => return Err(USAGE.to_string()),
    };
    request.validate().map_err(|e| e.to_string())?;
    Ok(Some(request))
}

/// Run one `ctl` command. Fails when the service answers with an error.
pub fn run(arguments: &[String]) -> Result<(), Box<dyn Error>> {
    let request = request(arguments)?;
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(talk(request))
}

async fn talk(request: Option<Request>) -> Result<(), Box<dyn Error>> {
    let pipe = pipe::connect(PIPE_NAME)
        .await
        .map_err(|e| format!("cannot reach the TunnelHop service: {e}"))?;
    let (reader, mut writer) = tokio::io::split(pipe);
    let mut lines = BufReader::new(reader).lines();

    let mut send = async |id: u64, request: Request| {
        writer
            .write_all(&ipc::encode(&ClientMessage { id, request }))
            .await
    };
    send(
        1,
        Request::Hello {
            version: PROTOCOL_VERSION,
        },
    )
    .await?;
    let Some(request) = request else {
        // `watch`: the greeting, then every event until the service goes away.
        while let Some(line) = lines.next_line().await? {
            if let Ok(ServerMessage::Event { event }) = ipc::decode(line.as_bytes()) {
                println!("{}", serde_json::to_string(&event)?);
            }
        }
        return Ok(());
    };
    send(2, request).await?;
    while let Some(line) = lines.next_line().await? {
        match ipc::decode::<ServerMessage>(line.as_bytes())? {
            ServerMessage::Response { id: 1, response } => {
                if let Response::Error { message } = response {
                    return Err(message.into());
                }
            }
            ServerMessage::Response { response, .. } => {
                println!("{}", serde_json::to_string_pretty(&response)?);
                return match response {
                    Response::Error { message } => Err(message.into()),
                    Response::ImportFailed { errors } => Err(errors.join("\n").into()),
                    _ => Ok(()),
                };
            }
            ServerMessage::Event { .. } => {}
        }
    }
    Err("the service closed the connection".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(line: &str) -> Vec<String> {
        line.split_whitespace().map(str::to_string).collect()
    }

    #[test]
    fn commands_become_requests() {
        assert_eq!(request(&args("status")), Ok(Some(Request::GetStatus)));
        assert_eq!(request(&args("rotate")), Ok(Some(Request::RotateNow)));
        assert_eq!(
            request(&args("connect wg-a")),
            Ok(Some(Request::Connect {
                profile_id: Some("wg-a".into())
            }))
        );
        assert_eq!(
            request(&args("connect")),
            Ok(Some(Request::Connect { profile_id: None }))
        );
        assert_eq!(
            request(&args("interval 30")),
            Ok(Some(Request::SetInterval { minutes: 30 }))
        );
        assert_eq!(
            request(&args("auto off")),
            Ok(Some(Request::SetAutoRotate { enabled: false }))
        );
        assert_eq!(request(&args("watch")), Ok(None));
    }

    #[test]
    fn bad_commands_are_refused_before_reaching_the_service() {
        for line in [
            "",
            "fly",
            "interval soon",
            "interval 0",
            "auto maybe",
            "select ../x",
            "connect UPPER",
            "import",
        ] {
            assert!(request(&args(line)).is_err(), "{line:?}");
        }
        assert!(request(&args("import C:\\no\\such\\file.zip")).is_err());
    }
}
