//! The public-IP probe — SPEC §4.5 step 6.
//!
//! Each URL in turn, 5 s each, until one answers with nothing but an IP address. The URLs come
//! from `settings.json` (`probe_urls`), so tests can point them at the lab's own "what's my IP"
//! endpoint: a public service sees the lab's address with or without a tunnel.

use std::net::IpAddr;
use std::time::Duration;

use async_trait::async_trait;
use th_core::rotation::Probe;

pub const DEFAULT_URLS: [&str; 2] = ["https://api.ipify.org", "https://ipinfo.io/ip"];
const TIMEOUT: Duration = Duration::from_secs(5);
/// More than any address in text; a longer answer is not one.
const MAX_BODY: usize = 64;
/// More than enough fallbacks; a longer list is a mistake in settings.json.
pub const MAX_URLS: usize = 4;

/// An answer that is an IP address and nothing else, surrounding space aside.
pub fn parse_ip(body: &[u8]) -> Option<IpAddr> {
    std::str::from_utf8(body).ok()?.trim().parse().ok()
}

/// Whether `url` may be a probe URL: plain http or https, with a host. Checked when settings are
/// loaded, so a bad entry is dropped instead of failing every probe.
pub fn is_valid_url(url: &str) -> bool {
    reqwest::Url::parse(url).is_ok_and(|u| {
        matches!(u.scheme(), "http" | "https") && u.host().is_some() && u.username().is_empty()
    })
}

pub struct HttpProbe {
    client: reqwest::Client,
    urls: Vec<String>,
}

impl HttpProbe {
    /// The given URLs, or the defaults when there are none.
    pub fn new(urls: Option<Vec<String>>) -> Result<HttpProbe, String> {
        let urls = urls
            .filter(|u| !u.is_empty())
            .unwrap_or_else(|| DEFAULT_URLS.iter().map(|u| u.to_string()).collect());
        let client = reqwest::Client::builder()
            .timeout(TIMEOUT)
            // A redirect would send the probe somewhere the settings did not name.
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| format!("cannot set up the IP probe: {e}"))?;
        Ok(HttpProbe { client, urls })
    }

    async fn ask(&self, url: &str) -> Result<IpAddr, String> {
        let mut response = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|e| with_causes(&e))?;
        if !response.status().is_success() {
            return Err(format!("answered {}", response.status()));
        }
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|e| e.to_string())? {
            body.extend_from_slice(&chunk);
            if body.len() > MAX_BODY {
                return Err("the answer is too long to be an address".into());
            }
        }
        parse_ip(&body).ok_or_else(|| "the answer is not an IP address".into())
    }
}

/// An error with its causes: reqwest's own message ("error sending request") names none of them,
/// and the cause (refused, timed out, reset) is what tells a dead tunnel from a slow one.
fn with_causes(error: &dyn std::error::Error) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        text.push_str(&format!(": {cause}"));
        source = cause.source();
    }
    text
}

#[async_trait]
impl Probe for HttpProbe {
    async fn public_ip(&self) -> Result<IpAddr, String> {
        let mut failures = Vec::new();
        for url in &self.urls {
            match self.ask(url).await {
                Ok(ip) => return Ok(ip),
                Err(e) => failures.push(format!("{url}: {e}")),
            }
        }
        Err(format!("no probe answered: {}", failures.join("; ")))
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use super::*;

    #[test]
    fn an_answer_is_an_address_and_nothing_else() {
        assert_eq!(
            parse_ip(b"203.0.113.7"),
            Some("203.0.113.7".parse().unwrap())
        );
        assert_eq!(
            parse_ip(b"  203.0.113.7\n"),
            Some("203.0.113.7".parse().unwrap())
        );
        assert_eq!(
            parse_ip(b"2001:db8::7"),
            Some("2001:db8::7".parse().unwrap())
        );
        for bad in [
            &b""[..],
            b"hello",
            b"203.0.113.7 extra",
            b"<html>203.0.113.7</html>",
            b"\xff",
        ] {
            assert_eq!(parse_ip(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn only_plain_http_and_https_urls_are_accepted() {
        for good in [
            "https://api.ipify.org",
            "http://198.18.77.2:8080/",
            "https://ipinfo.io/ip",
        ] {
            assert!(is_valid_url(good), "{good}");
        }
        for bad in [
            "",
            "ftp://x/",
            "file:///C:/x",
            "https://user:pw@x/",
            "api.ipify.org",
            "http://",
        ] {
            assert!(!is_valid_url(bad), "{bad}");
        }
    }

    /// A one-request HTTP server on localhost that answers with `body` and `status`.
    async fn serve(status: &'static str, body: &'static str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 1024];
            let _ = socket.read(&mut request).await;
            let reply = format!(
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = socket.write_all(reply.as_bytes()).await;
        });
        format!("http://{addr}/")
    }

    /// 100 digits: longer than any address.
    const TOO_LONG: &str = "9999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999";

    /// A URL on localhost where nothing listens.
    async fn closed() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        format!("http://{addr}/")
    }

    #[tokio::test]
    async fn the_first_url_that_answers_with_an_address_wins() {
        let urls = vec![
            closed().await,
            serve("200 OK", "not an address").await,
            serve("500 Internal Server Error", "203.0.113.1").await,
            serve("200 OK", "203.0.113.9\n").await,
        ];
        let probe = HttpProbe::new(Some(urls)).unwrap();
        assert_eq!(probe.public_ip().await, Ok("203.0.113.9".parse().unwrap()));
    }

    #[tokio::test]
    async fn every_failure_is_reported_when_none_answers() {
        let urls = vec![closed().await, serve("200 OK", TOO_LONG).await];
        let error = HttpProbe::new(Some(urls))
            .unwrap()
            .public_ip()
            .await
            .unwrap_err();
        assert!(error.contains("no probe answered"), "{error}");
        assert!(error.contains("too long"), "{error}");
    }

    #[test]
    fn no_urls_means_the_defaults() {
        let probe = HttpProbe::new(None).unwrap();
        assert_eq!(probe.urls, DEFAULT_URLS);
        let probe = HttpProbe::new(Some(vec![])).unwrap();
        assert_eq!(probe.urls, DEFAULT_URLS);
    }
}
