//! OpenVPN's management interface, from the client side — SPEC §4.3.
//!
//! The OpenVPN driver starts `openvpn.exe` with `--management 127.0.0.1 <port> <password file>
//! --management-hold --management-query-passwords` and then talks to it here: log in, ask for
//! state notifications, release the hold, answer the credential prompt, and in the end ask it to
//! exit. Everything OpenVPN sends is a line. Lines starting with `>` are notifications, which can
//! arrive at any time, including between a command and its `SUCCESS:` or `ERROR:`; they are
//! queued so a command's answer is never mistaken for one, or the other way round.
//!
//! The one exception to "a line" is the password prompt, `ENTER PASSWORD:`, which OpenVPN sends
//! without a line break and then waits.

use std::collections::VecDeque;
use std::io;
use std::net::{IpAddr, SocketAddr};

use th_core::ipc::Secret;
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use zeroize::Zeroizing;

/// More than any line OpenVPN sends; a peer sending more without a line break is not one.
const MAX_LINE: usize = 64 * 1024;
const PASSWORD_PROMPT: &str = "ENTER PASSWORD:";

/// Where the tunnel is, as `>STATE:` reports it. Only the states the driver acts on are named.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    Connected,
    Reconnecting,
    Exiting,
    /// CONNECTING, WAIT, AUTH, GET_CONFIG, ASSIGN_IP, ADD_ROUTES, RESOLVE, TCP_CONNECT, ...
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateChange {
    pub state: State,
    /// The state's description field, such as `SUCCESS` or `ping-restart`.
    pub detail: String,
    /// The tunnel's local address, once one is assigned.
    pub local_ip: Option<IpAddr>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Notification {
    State(StateChange),
    /// `>HOLD:`: OpenVPN waits for `hold release`.
    Hold,
    /// `>PASSWORD:Need '<realm>' username/password`.
    NeedCredentials {
        realm: String,
    },
    /// `>PASSWORD:Verification Failed: '<realm>'`: the server refused the credentials.
    CredentialsRefused {
        realm: String,
    },
    /// `>FATAL:`: OpenVPN is about to exit.
    Fatal(String),
    /// Anything else (`>INFO:`, `>LOG:`, ...), kept as the whole line.
    Other(String),
}

/// One line from OpenVPN, classified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Line {
    PasswordPrompt,
    Success(String),
    Error(String),
    Notification(Notification),
    /// Part of a multi-line answer, or anything unrecognised.
    Other(String),
}

pub fn parse_line(line: &str) -> Line {
    if line == PASSWORD_PROMPT {
        return Line::PasswordPrompt;
    }
    if let Some(rest) = line.strip_prefix("SUCCESS:") {
        return Line::Success(rest.trim().to_string());
    }
    if let Some(rest) = line.strip_prefix("ERROR:") {
        return Line::Error(rest.trim().to_string());
    }
    let Some(notification) = line.strip_prefix('>') else {
        return Line::Other(line.to_string());
    };
    let (kind, payload) = notification.split_once(':').unwrap_or((notification, ""));
    Line::Notification(match kind {
        "STATE" => parse_state(payload),
        "HOLD" => Notification::Hold,
        "PASSWORD" => parse_password(payload).unwrap_or_else(|| Notification::Other(line.into())),
        "FATAL" => Notification::Fatal(payload.to_string()),
        _ => Notification::Other(line.to_string()),
    })
}

/// `<unix time>,<state>,<description>,<local IPv4>,<remote address>,...`
fn parse_state(payload: &str) -> Notification {
    let fields: Vec<&str> = payload.split(',').collect();
    let field = |i: usize| fields.get(i).copied().unwrap_or("");
    let state = match field(1) {
        "CONNECTED" => State::Connected,
        "RECONNECTING" => State::Reconnecting,
        "EXITING" => State::Exiting,
        other => State::Other(other.to_string()),
    };
    Notification::State(StateChange {
        state,
        detail: field(2).to_string(),
        local_ip: field(3).parse().ok(),
    })
}

/// The realm between the first pair of single quotes.
fn realm(text: &str) -> Option<String> {
    let start = text.find('\'')? + 1;
    let length = text[start..].find('\'')?;
    Some(text[start..start + length].to_string())
}

fn parse_password(payload: &str) -> Option<Notification> {
    if payload.starts_with("Need ") {
        Some(Notification::NeedCredentials {
            realm: realm(payload)?,
        })
    } else if payload.starts_with("Verification Failed") {
        Some(Notification::CredentialsRefused {
            realm: realm(payload)?,
        })
    } else {
        None
    }
}

/// A command argument in double quotes, with `\` and `"` escaped, so a username or password
/// containing them stays one argument instead of ending the line's quoting early. Line breaks
/// never get this far: the IPC layer and `send_credentials` refuse credentials containing them.
pub fn quote(value: &str) -> String {
    let mut quoted = String::with_capacity(value.len() + 2);
    quoted.push('"');
    for c in value.chars() {
        if matches!(c, '\\' | '"') {
            quoted.push('\\');
        }
        quoted.push(c);
    }
    quoted.push('"');
    quoted
}

#[derive(Debug, Error)]
pub enum ManagementError {
    #[error("OpenVPN refused the management password")]
    BadPassword,
    /// Names only the command's first word: the rest may be a credential.
    #[error("OpenVPN refused \"{command}\": {message}")]
    Refused { command: String, message: String },
    /// A line break would end the command early and start another one.
    #[error("a username or password contains a line break or NUL")]
    InvalidCredential,
    #[error("the management connection closed")]
    Closed,
    #[error("a line from OpenVPN is longer than {MAX_LINE} bytes")]
    TooLong,
    #[error("unexpected from OpenVPN: {0}")]
    Unexpected(String),
    #[error(transparent)]
    Io(#[from] io::Error),
}

pub struct Management<S> {
    stream: S,
    buffer: Vec<u8>,
    pending: VecDeque<Notification>,
}

impl Management<TcpStream> {
    /// Connect to the management port and log in. Nothing here times out: a peer that accepts
    /// and then stays silent is waited on forever, so the caller bounds this with a timeout.
    pub async fn connect(addr: SocketAddr, password: &Secret) -> Result<Self, ManagementError> {
        let stream = TcpStream::connect(addr).await?;
        Self::login(stream, password).await
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin> Management<S> {
    /// Answer the password prompt. Anything but `SUCCESS:` afterwards is a refusal. Like
    /// `connect`, unbounded in time: the caller adds the timeout.
    pub async fn login(stream: S, password: &Secret) -> Result<Self, ManagementError> {
        let mut management = Management {
            stream,
            buffer: Vec::new(),
            pending: VecDeque::new(),
        };
        match parse_line(&management.read_line().await?) {
            Line::PasswordPrompt => {}
            // Our OpenVPN always asks: a port that does not is not ours.
            other => return Err(ManagementError::Unexpected(format!("{other:?}"))),
        }
        let mut answer = Zeroizing::new(password.expose().to_string());
        answer.push('\n');
        management.stream.write_all(answer.as_bytes()).await?;
        loop {
            match parse_line(&management.read_line().await?) {
                Line::Success(_) => return Ok(management),
                Line::Error(_) | Line::PasswordPrompt => return Err(ManagementError::BadPassword),
                Line::Notification(n) => management.pending.push_back(n),
                Line::Other(_) => {}
            }
        }
    }

    /// Turn on state notifications, then let OpenVPN start connecting.
    pub async fn start(&mut self) -> Result<(), ManagementError> {
        self.command("state on").await?;
        self.command("hold release").await?;
        Ok(())
    }

    /// Answer a `>PASSWORD:Need '<realm>'` prompt. The IPC layer already refuses line breaks;
    /// checking again here keeps a credential from becoming a command whatever the source.
    pub async fn send_credentials(
        &mut self,
        realm: &str,
        username: &str,
        password: &Secret,
    ) -> Result<(), ManagementError> {
        let breaks_line = |value: &str| value.contains(['\r', '\n', '\0']);
        if breaks_line(username) || breaks_line(password.expose()) {
            return Err(ManagementError::InvalidCredential);
        }
        let realm = quote(realm);
        self.command(&format!("username {realm} {}", quote(username)))
            .await?;
        let line = Zeroizing::new(format!("password {realm} {}", quote(password.expose())));
        self.command(&line).await?;
        Ok(())
    }

    /// Ask OpenVPN to exit cleanly: it then removes its routes and closes the tunnel.
    pub async fn sigterm(&mut self) -> Result<(), ManagementError> {
        self.command("signal SIGTERM").await.map(|_| ())
    }

    /// The next notification: one that arrived during a command, else the next one read.
    pub async fn next_notification(&mut self) -> Result<Notification, ManagementError> {
        if let Some(queued) = self.pending.pop_front() {
            return Ok(queued);
        }
        loop {
            match parse_line(&self.read_line().await?) {
                Line::Notification(n) => return Ok(n),
                Line::PasswordPrompt => {
                    return Err(ManagementError::Unexpected(PASSWORD_PROMPT.into()));
                }
                _ => {}
            }
        }
    }

    /// Send one command and wait for its `SUCCESS:`, keeping notifications for later. Only for
    /// commands answered that way: a multi-line answer (`status`, `state` without an argument)
    /// ends in `END` instead, and this would wait for a `SUCCESS:` that never comes.
    pub async fn command(&mut self, command: &str) -> Result<String, ManagementError> {
        let mut line = Zeroizing::new(command.to_string());
        line.push('\n');
        self.stream.write_all(line.as_bytes()).await?;
        loop {
            match parse_line(&self.read_line().await?) {
                Line::Success(message) => return Ok(message),
                Line::Error(message) => {
                    return Err(ManagementError::Refused {
                        command: command.split(' ').next().unwrap_or("").to_string(),
                        message,
                    });
                }
                Line::Notification(n) => self.pending.push_back(n),
                Line::PasswordPrompt => {
                    return Err(ManagementError::Unexpected(PASSWORD_PROMPT.into()));
                }
                Line::Other(_) => {}
            }
        }
    }

    /// The next line without its line break; the password prompt as soon as it is complete.
    async fn read_line(&mut self) -> Result<String, ManagementError> {
        loop {
            if self.buffer.starts_with(PASSWORD_PROMPT.as_bytes()) {
                self.buffer.drain(..PASSWORD_PROMPT.len());
                return Ok(PASSWORD_PROMPT.to_string());
            }
            if let Some(end) = self.buffer.iter().position(|&b| b == b'\n') {
                let mut line: Vec<u8> = self.buffer.drain(..=end).collect();
                line.pop();
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                return Ok(String::from_utf8_lossy(&line).into_owned());
            }
            if self.buffer.len() > MAX_LINE {
                return Err(ManagementError::TooLong);
            }
            let mut chunk = [0u8; 4096];
            let read = self.stream.read(&mut chunk).await?;
            if read == 0 {
                return Err(ManagementError::Closed);
            }
            self.buffer.extend_from_slice(&chunk[..read]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn states_are_parsed_with_their_local_address() {
        assert_eq!(
            parse_line(">STATE:1700000000,CONNECTED,SUCCESS,10.77.3.2,192.168.77.1,1194,,"),
            Line::Notification(Notification::State(StateChange {
                state: State::Connected,
                detail: "SUCCESS".into(),
                local_ip: Some("10.77.3.2".parse().unwrap()),
            }))
        );
        assert_eq!(
            parse_line(">STATE:1700000001,RECONNECTING,ping-restart,,,,,"),
            Line::Notification(Notification::State(StateChange {
                state: State::Reconnecting,
                detail: "ping-restart".into(),
                local_ip: None,
            }))
        );
        assert_eq!(
            parse_line(">STATE:1700000002,ASSIGN_IP,,10.77.3.2,,,,"),
            Line::Notification(Notification::State(StateChange {
                state: State::Other("ASSIGN_IP".into()),
                detail: String::new(),
                local_ip: Some("10.77.3.2".parse().unwrap()),
            }))
        );
        // Truncated or garbled: still a state, never a panic.
        assert!(matches!(
            parse_line(">STATE:"),
            Line::Notification(Notification::State(_))
        ));
    }

    #[test]
    fn password_notifications_carry_their_realm() {
        assert_eq!(
            parse_line(">PASSWORD:Need 'Auth' username/password"),
            Line::Notification(Notification::NeedCredentials {
                realm: "Auth".into()
            })
        );
        assert_eq!(
            parse_line(">PASSWORD:Verification Failed: 'Auth'"),
            Line::Notification(Notification::CredentialsRefused {
                realm: "Auth".into()
            })
        );
        assert!(matches!(
            parse_line(">PASSWORD:something new"),
            Line::Notification(Notification::Other(_))
        ));
    }

    #[test]
    fn answers_and_other_lines_are_told_apart() {
        assert_eq!(parse_line("ENTER PASSWORD:"), Line::PasswordPrompt);
        assert_eq!(
            parse_line("SUCCESS: hold release succeeded"),
            Line::Success("hold release succeeded".into())
        );
        assert_eq!(
            parse_line("ERROR: unknown command"),
            Line::Error("unknown command".into())
        );
        assert_eq!(
            parse_line(">HOLD:Waiting for hold release:0"),
            Line::Notification(Notification::Hold)
        );
        assert!(
            matches!(parse_line(">FATAL:cannot open TUN"), Line::Notification(Notification::Fatal(m)) if m == "cannot open TUN")
        );
        assert!(matches!(
            parse_line(">INFO:OpenVPN Management Interface Version 5"),
            Line::Notification(Notification::Other(_))
        ));
        assert_eq!(parse_line("END"), Line::Other("END".into()));
    }

    #[test]
    fn quoting_keeps_quotes_and_backslashes_inside_one_argument() {
        assert_eq!(quote("alice"), r#""alice""#);
        assert_eq!(quote(r#"p"w"#), r#""p\"w""#);
        assert_eq!(quote(r"C:\path\"), r#""C:\\path\\""#);
        assert_eq!(quote(r#"\""#), r#""\\\"""#);
        assert_eq!(quote(""), r#""""#);
        assert_eq!(quote("пароль з пробілом"), "\"пароль з пробілом\"");
    }
}
