//! `th-service ctl`: drive the running service over its pipe, exactly as the UI will. It lets
//! the leak test rotate twenty times without a UI, and gives a quick look at the service.
//!
//! It runs as whoever starts it, connects with `pipe::connect` (so it refuses an impostor pipe),
//! and reads any file itself: the service only ever receives bytes.

use std::error::Error;

use th_core::ipc::{
    self, ClientMessage, PROTOCOL_VERSION, ProfileFile, Request, Response, ServerMessage,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::pipe::{self, PIPE_NAME};

pub const USAGE: &str = "usage: th-service ctl <command>

  status                      the service's state
  import <bundle.zip>         import a bundle (prints its id)
  select <bundle id>          make an imported bundle the active one
  bundles                     every installed bundle, and the active one
  delete <bundle id>          delete an installed bundle and its credentials
  create <name> <file>...     make a bundle of .conf/.ovpn files (prints its id)
  add <bundle id> <file>...   add .conf/.ovpn files as servers (prints the new bundle id)
  remove <bundle id> <id>...  remove servers by profile id (prints the new bundle id)
  connect [profile]           connect to a profile, or a random one
  rotate                      rotate now
  disconnect                  disconnect and lift the kill-switch
  interval <minutes>          set the rotation interval
  auto <on|off>               turn automatic rotation on or off
  watch                       print the service's events as they happen";

/// The config files at `paths`, read here and each under its own name.
fn config_files(paths: &[&str]) -> Result<Vec<ProfileFile>, String> {
    paths
        .iter()
        .map(|path| {
            let bytes = std::fs::read(path).map_err(|e| format!("cannot read {path}: {e}"))?;
            let file_name = std::path::Path::new(path)
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| format!("{path} has no usable file name"))?;
            Ok(ProfileFile::new("", file_name, &bytes))
        })
        .collect()
}

/// The request a command line asks for, or why it is not one.
pub fn request(arguments: &[String]) -> Result<Option<Request>, String> {
    let words: Vec<&str> = arguments.iter().map(String::as_str).collect();
    let request = match words.as_slice() {
        ["status"] => Request::GetStatus,
        ["import", path] => {
            let bytes = std::fs::read(path).map_err(|e| format!("cannot read {path}: {e}"))?;
            Request::ImportBundle {
                zip_base64: ipc::encode_zip(&bytes),
                password: None,
            }
        }
        ["select", id] => Request::SelectBundle {
            bundle_id: id.to_string(),
        },
        ["bundles"] => Request::ListBundles,
        ["delete", id] => Request::DeleteBundle {
            bundle_id: id.to_string(),
        },
        ["create", name, paths @ ..] if !paths.is_empty() => Request::CreateBundle {
            name: name.to_string(),
            files: config_files(paths)?,
        },
        ["add", id, paths @ ..] if !paths.is_empty() => Request::AddProfiles {
            bundle_id: id.to_string(),
            files: config_files(paths)?,
        },
        ["remove", id, profiles @ ..] if !profiles.is_empty() => Request::RemoveProfiles {
            bundle_id: id.to_string(),
            profile_ids: profiles.iter().map(|p| p.to_string()).collect(),
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
        assert_eq!(request(&args("bundles")), Ok(Some(Request::ListBundles)));
        assert_eq!(
            request(&args("delete 0123456789abcdef")),
            Ok(Some(Request::DeleteBundle {
                bundle_id: "0123456789abcdef".into()
            }))
        );
        assert_eq!(
            request(&args("remove 0123456789abcdef wg-a ovpn")),
            Ok(Some(Request::RemoveProfiles {
                bundle_id: "0123456789abcdef".into(),
                profile_ids: vec!["wg-a".into(), "ovpn".into()],
            }))
        );
        assert_eq!(request(&args("watch")), Ok(None));
    }

    #[test]
    fn add_reads_the_files_and_sends_their_names_without_the_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nl-1.conf");
        std::fs::write(&path, b"[Interface]\n").unwrap();
        let line = vec![
            "add".to_string(),
            "0123456789abcdef".to_string(),
            path.to_str().unwrap().to_string(),
        ];
        assert_eq!(
            request(&line),
            Ok(Some(Request::AddProfiles {
                bundle_id: "0123456789abcdef".into(),
                files: vec![ProfileFile::new("", "nl-1.conf", b"[Interface]\n")],
            }))
        );
        let line = vec![
            "create".to_string(),
            "Proton UA".to_string(),
            path.to_str().unwrap().to_string(),
        ];
        assert_eq!(
            request(&line),
            Ok(Some(Request::CreateBundle {
                name: "Proton UA".into(),
                files: vec![ProfileFile::new("", "nl-1.conf", b"[Interface]\n")],
            }))
        );
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
            "delete ../x",
            "add 0123456789abcdef",
            "add 0123456789abcdef C:\\no\\such\\file.conf",
            "create Lab",
            "create Lab C:\\no\\such\\file.conf",
            "remove 0123456789abcdef",
            "remove 0123456789abcdef UPPER",
            "delete",
            "connect UPPER",
            "import",
        ] {
            assert!(request(&args(line)).is_err(), "{line:?}");
        }
        assert!(request(&args("import C:\\no\\such\\file.zip")).is_err());
    }
}
