//! Plain HTTPS requests: the update check, downloads, the model lookup, the
//! Google sign-in and the account of `VinPUT` Pro.
//!
//! They go through `WinHTTP`, the client that is part of Windows, so none of
//! it is carried in the executable. Windows checks the server's certificate
//! against its own store and finds the system's proxy. A request waits for
//! its answer on the thread that makes it; every caller is already on a
//! thread of its own.
//!
//! Only two kinds of address are ever asked: `https://`, and `http://` on
//! this computer (`127.0.0.1` or `localhost`), which is how tests and the
//! trial run of a release serve their files. Anything else is refused before
//! a connection is made, and a redirect from HTTPS to HTTP is not followed.

#[cfg(windows)]
mod winhttp;

use std::time::Duration;

/// What every request says it is.
const USER_AGENT: &str = concat!("vinput/", env!("CARGO_PKG_VERSION"));

/// A request to make.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Request<'a> {
    /// The address, `https://` or `http://` on this computer.
    pub url: &'a str,
    /// A form to post. Without one, and without [`json`](Self::json), the
    /// request is a `GET`.
    pub form: Option<&'a str>,
    /// JSON to post, for the servers that take that instead of a form. An
    /// empty one is a post without a body.
    pub json: Option<&'a str>,
    /// Extra header lines, as name and value.
    pub headers: &'a [(&'a str, &'a str)],
    /// How long finding and reaching the server may take.
    pub connect_timeout: Duration,
    /// How long the server may send nothing, before the answer starts or in
    /// the middle of it.
    pub stall_timeout: Duration,
}

impl<'a> Request<'a> {
    /// A `GET` of `url` in which every step may take `timeout`.
    pub(crate) fn get(url: &'a str, timeout: Duration) -> Self {
        Self {
            url,
            form: None,
            json: None,
            headers: &[],
            connect_timeout: timeout,
            stall_timeout: timeout,
        }
    }

    /// Sends the request and waits for the answer to start. Redirects are
    /// followed, except from HTTPS to HTTP.
    ///
    /// # Errors
    ///
    /// Returns why no answer arrived, in words for the log or the user.
    pub(crate) fn send(&self) -> Result<Response, String> {
        let target = Target::parse(self.url)?;
        #[cfg(windows)]
        {
            winhttp::send(self, &target).map(Response)
        }
        #[cfg(not(windows))]
        {
            let _ = target;
            Err("network requests are only available on Windows".to_owned())
        }
    }
}

/// The answer to a [`Request`]: its status, then its body as it arrives.
#[derive(Debug)]
pub(crate) struct Response(
    #[cfg(windows)] winhttp::Response,
    #[cfg(not(windows))] std::convert::Infallible,
);

impl Response {
    /// The HTTP status of the answer, such as 200.
    pub(crate) fn status(&self) -> u16 {
        #[cfg(windows)]
        {
            self.0.status()
        }
        #[cfg(not(windows))]
        {
            match self.0 {}
        }
    }

    /// Reads the next bytes of the body into `buffer` and returns how many
    /// arrived; zero means the body is complete.
    ///
    /// # Errors
    ///
    /// Returns why the body stopped arriving.
    pub(crate) fn read(&mut self, buffer: &mut [u8]) -> Result<usize, String> {
        #[cfg(windows)]
        {
            self.0.read(buffer)
        }
        #[cfg(not(windows))]
        {
            let _ = buffer;
            match self.0 {}
        }
    }

    /// Reads the whole body, which may be at most `limit` bytes, giving up
    /// when `deadline` has passed.
    ///
    /// # Errors
    ///
    /// Returns why the body could not be read to its end, which includes it
    /// being larger than `limit`.
    pub(crate) fn body(&mut self, limit: usize, deadline: Duration) -> Result<Vec<u8>, String> {
        let started = std::time::Instant::now();
        let mut body = Vec::new();
        let mut buffer = [0u8; 8192];
        loop {
            let read = self.read(&mut buffer)?;
            if read == 0 {
                return Ok(body);
            }
            body.extend_from_slice(&buffer[..read]);
            if body.len() > limit {
                return Err("the answer is too large".to_owned());
            }
            if started.elapsed() > deadline {
                return Err("the answer took too long".to_owned());
            }
        }
    }
}

/// Posts a form to an HTTPS address and returns the status and the body of
/// the answer, which may be at most `limit` bytes. Waits for the answer.
///
/// # Errors
///
/// Returns what went wrong, in words, if no answer arrived within `timeout`.
pub(crate) fn post_form(
    url: &str,
    form: &str,
    timeout: Duration,
    limit: usize,
) -> Result<(u16, Vec<u8>), String> {
    // What is posted is a secret; not even this computer gets it in the clear.
    if !url.starts_with("https://") {
        return Err(format!("{url} is not an HTTPS address"));
    }
    let mut response = Request {
        form: Some(form),
        ..Request::get(url, timeout)
    }
    .send()?;
    Ok((response.status(), response.body(limit, timeout)?))
}

/// Where a request goes, taken apart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Target {
    /// Whether the connection is encrypted.
    pub secure: bool,
    /// The server's name.
    pub host: String,
    /// The port on the server.
    pub port: u16,
    /// The path and the query, starting with `/`.
    pub path: String,
}

impl Target {
    /// Takes an address apart and refuses the ones Vinput never asks:
    /// anything but HTTPS, except plain HTTP to this computer.
    ///
    /// # Errors
    ///
    /// Returns what is wrong with the address.
    pub(crate) fn parse(url: &str) -> Result<Self, String> {
        let refused = |why: &str| format!("{url} {why}");
        let (secure, rest) = if let Some(rest) = url.strip_prefix("https://") {
            (true, rest)
        } else if let Some(rest) = url.strip_prefix("http://") {
            (false, rest)
        } else {
            return Err(refused("is not an HTTPS address"));
        };
        let (authority, path) = match rest.find(['/', '?', '#']) {
            Some(end) => rest.split_at(end),
            None => (rest, ""),
        };
        // A name and a password in an address are only ever a trick, and
        // nothing Vinput asks is addressed by an IPv6 number.
        if authority.contains(['@', '[', '\\']) {
            return Err(refused("is not an address Vinput asks"));
        }
        let (host, port) = match authority.rsplit_once(':') {
            Some((host, port)) => (
                host,
                port.parse::<u16>()
                    .map_err(|_| refused("has no usable port"))?,
            ),
            None => (authority, if secure { 443 } else { 80 }),
        };
        if host.is_empty() || host.chars().any(|c| c.is_whitespace() || c.is_control()) {
            return Err(refused("names no server"));
        }
        if !secure && !matches!(host, "127.0.0.1" | "localhost") {
            return Err(refused("is not an HTTPS address"));
        }
        let path = path.split('#').next().unwrap_or_default();
        Ok(Self {
            secure,
            host: host.to_owned(),
            port,
            path: if path.starts_with('/') {
                path.to_owned()
            } else {
                format!("/{path}")
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    use super::*;

    #[test]
    fn addresses_are_taken_apart() {
        assert_eq!(
            Target::parse("https://huggingface.co/api/models/a/b?blobs=true#top"),
            Ok(Target {
                secure: true,
                host: "huggingface.co".into(),
                port: 443,
                path: "/api/models/a/b?blobs=true".into(),
            })
        );
        assert_eq!(
            Target::parse("http://127.0.0.1:18765/releases/latest.json"),
            Ok(Target {
                secure: false,
                host: "127.0.0.1".into(),
                port: 18765,
                path: "/releases/latest.json".into(),
            })
        );
        assert_eq!(
            Target::parse("https://example.com:8443").map(|target| (target.port, target.path)),
            Ok((8443, "/".into()))
        );
        assert_eq!(
            Target::parse("https://example.com?x=1").map(|target| target.path),
            Ok("/?x=1".into())
        );
    }

    #[test]
    fn only_https_and_this_computer_are_asked() {
        for refused in [
            "http://example.com/file",
            "http://127.0.0.1.example.com/file",
            "http://localhost.example.com:80/file",
            "http://127.0.0.1@example.com/file",
            "https://user:secret@example.com/file",
            "http://example.com\\@127.0.0.1/file",
            "ftp://example.com/file",
            "file:C:\\model.bin",
            "https:///file",
            "https://example.com:port/file",
            "https://[::1]/file",
            "example.com/file",
            "",
        ] {
            assert!(Target::parse(refused).is_err(), "{refused}");
            assert!(
                Request::get(refused, Duration::from_secs(1))
                    .send()
                    .is_err()
            );
        }
    }

    /// Serves one canned answer on a loopback port and hands back what was
    /// asked.
    fn serve(answer: &'static str) -> std::io::Result<(String, thread::JoinHandle<String>)> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let url = format!("http://{}", listener.local_addr()?);
        let server = thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else {
                return String::new();
            };
            let mut asked = Vec::new();
            let mut buffer = [0u8; 4096];
            // The request is complete when its head and, for a post, the body
            // the head announces have arrived.
            loop {
                let Ok(read) = stream.read(&mut buffer) else {
                    break;
                };
                asked.extend_from_slice(&buffer[..read]);
                let text = String::from_utf8_lossy(&asked);
                let Some(head) = text.find("\r\n\r\n") else {
                    if read == 0 {
                        break;
                    }
                    continue;
                };
                let announced = text
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")?
                            .trim()
                            .parse()
                            .ok()
                    })
                    .unwrap_or(0usize);
                if read == 0 || asked.len() >= head + 4 + announced {
                    break;
                }
            }
            let _ = stream.write_all(answer.as_bytes());
            String::from_utf8_lossy(&asked).into_owned()
        });
        Ok((url, server))
    }

    #[cfg(windows)]
    #[test]
    fn a_get_carries_its_headers_and_reads_the_answer() -> std::io::Result<()> {
        let (url, server) = serve(
            "HTTP/1.1 206 Partial Content\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello",
        )?;
        let url = format!("{url}/models/a.bin?download=true");
        let response = Request {
            headers: &[("Range", "bytes=10-")],
            ..Request::get(&url, Duration::from_secs(10))
        }
        .send();
        assert!(response.is_ok(), "{response:?}");
        let Ok(mut response) = response else {
            return Ok(());
        };
        assert_eq!(response.status(), 206);
        assert_eq!(
            response.body(100, Duration::from_secs(10)),
            Ok(b"hello".to_vec())
        );
        let asked = server.join().unwrap_or_default();
        assert!(
            asked.starts_with("GET /models/a.bin?download=true HTTP/1.1\r\n"),
            "{asked}"
        );
        assert!(asked.contains("\r\nRange: bytes=10-\r\n"), "{asked}");
        assert!(
            asked.contains(&format!("\r\nUser-Agent: {USER_AGENT}\r\n")),
            "{asked}"
        );
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn a_form_is_posted_and_the_status_comes_back() -> std::io::Result<()> {
        // `post_form` itself takes HTTPS only, like the sign-in it is for.
        assert!(
            post_form(
                "http://example.com/token",
                "a=b",
                Duration::from_secs(1),
                10
            )
            .is_err()
        );
        let (url, server) =
            serve("HTTP/1.1 400 Bad Request\r\nContent-Length: 2\r\nConnection: close\r\n\r\nno")?;
        let response = Request {
            form: Some("grant_type=refresh_token&x=a%2Fb"),
            ..Request::get(&url, Duration::from_secs(10))
        }
        .send();
        assert!(response.is_ok(), "{response:?}");
        let Ok(mut response) = response else {
            return Ok(());
        };
        assert_eq!(response.status(), 400);
        assert_eq!(
            response.body(100, Duration::from_secs(10)),
            Ok(b"no".to_vec())
        );
        let asked = server.join().unwrap_or_default();
        assert!(asked.starts_with("POST / HTTP/1.1\r\n"), "{asked}");
        assert!(
            asked.contains("\r\nContent-Type: application/x-www-form-urlencoded\r\n"),
            "{asked}"
        );
        assert!(
            asked.ends_with("\r\n\r\ngrant_type=refresh_token&x=a%2Fb"),
            "{asked}"
        );
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn json_is_posted_as_json_and_an_empty_one_without_a_body() -> std::io::Result<()> {
        let answer = "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}";
        let (url, server) = serve(answer)?;
        let response = Request {
            json: Some(r#"{"plan":"monthly"}"#),
            headers: &[("Authorization", "Bearer abc")],
            ..Request::get(&url, Duration::from_secs(10))
        }
        .send();
        assert_eq!(response.map(|response| response.status()).ok(), Some(200));
        let asked = server.join().unwrap_or_default();
        assert!(asked.starts_with("POST / HTTP/1.1\r\n"), "{asked}");
        assert!(
            asked.contains("\r\nContent-Type: application/json\r\n"),
            "{asked}"
        );
        assert!(
            asked.contains("\r\nAuthorization: Bearer abc\r\n"),
            "{asked}"
        );
        assert!(asked.ends_with("\r\n\r\n{\"plan\":\"monthly\"}"), "{asked}");

        // A post without a body still says how long its body is: servers
        // and what stands in front of them refuse one that does not.
        let (url, server) = serve(answer)?;
        let response = Request {
            json: Some(""),
            ..Request::get(&url, Duration::from_secs(10))
        }
        .send();
        assert_eq!(response.map(|response| response.status()).ok(), Some(200));
        let asked = server.join().unwrap_or_default();
        assert!(asked.starts_with("POST / HTTP/1.1\r\n"), "{asked}");
        assert!(asked.contains("\r\nContent-Length: 0\r\n"), "{asked}");
        assert!(!asked.contains("Content-Type"), "{asked}");
        assert!(asked.ends_with("\r\n\r\n"), "{asked}");
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn an_answer_over_its_limit_is_refused() -> std::io::Result<()> {
        let (url, _server) =
            serve("HTTP/1.1 200 OK\r\nContent-Length: 11\r\nConnection: close\r\n\r\nhello world")?;
        let response = Request::get(&url, Duration::from_secs(10)).send();
        assert!(response.is_ok(), "{response:?}");
        let Ok(mut response) = response else {
            return Ok(());
        };
        assert!(response.body(5, Duration::from_secs(10)).is_err());
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn a_silent_server_and_a_closed_port_are_errors() -> std::io::Result<()> {
        // Accepts and then says nothing.
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let url = format!("http://{}/", listener.local_addr()?);
        thread::spawn(move || {
            let connection = listener.accept();
            thread::sleep(Duration::from_secs(20));
            drop(connection);
        });
        let started = std::time::Instant::now();
        let silent = Request::get(&url, Duration::from_secs(1)).send();
        assert!(silent.is_err());
        // Windows looks at its timeouts every few seconds, not to the millisecond.
        assert!(started.elapsed() < Duration::from_secs(10));

        // Port 9 on loopback has no listener.
        assert!(
            Request::get("http://127.0.0.1:9/", Duration::from_secs(2))
                .send()
                .is_err()
        );
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn a_redirect_from_this_computer_is_not_followed() -> std::io::Result<()> {
        let (url, _server) = serve(
            "HTTP/1.1 302 Found\r\nLocation: http://example.com/elsewhere\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        )?;
        let response = Request::get(&url, Duration::from_secs(10)).send();
        // The redirect is handed back as the answer instead of being followed
        // to a server that is not this computer.
        assert_eq!(response.map(|response| response.status()).ok(), Some(302));
        Ok(())
    }
}
