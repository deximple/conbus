//! One HTTP/1.1 `POST` over a `TcpStream`, and what the collector's answer
//! means (OTLP 1.11.0, "OTLP/HTTP Response"): `200` is success, and a `200`
//! whose body carries `partialSuccess` with a count above zero means some
//! points were dropped; a `4xx` or `5xx` carries a `Status` whose `message` is
//! the part a person can act on. The request asks the collector to close the
//! connection, and a complete answer is read as the end whether or not it
//! does.

use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use serde_json::Value;

use super::Endpoint;

/// How long a collector on this machine is given.
#[derive(Clone, Copy, Debug)]
pub struct Bounds {
    /// To accept the connection.
    pub connect: Duration,
    /// For each read and write after that, the answer included.
    pub answer: Duration,
}

/// What the pipe gives a collector on this machine: a local one accepts at
/// once or is not there, and answers once its pipeline has taken the points.
pub const BOUNDS: Bounds = Bounds {
    connect: Duration::from_secs(2),
    answer: Duration::from_secs(10),
};

/// The most of an answer that is read. OTLP says a client must bound it and
/// suggests 4 MiB; an acknowledgement is a few bytes and an error a line.
const ANSWER_LIMIT: usize = 64 * 1024;

/// How much of what a collector said goes into an error.
const DETAIL_CHARS: usize = 200;

/// What a collector said to a request it took: a 2xx.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Answer {
    pub status: u16,
    pub reason: String,
    /// `partialSuccess`: how many points it rejected, and what it said,
    /// when it took the request but not all of it.
    pub rejected: Option<(u64, String)>,
}

/// Send `body` to the collector and read its answer. Anything but a 2xx is an
/// error that says what the collector said.
pub fn post(endpoint: &Endpoint, body: &[u8], bounds: &Bounds) -> io::Result<Answer> {
    let url = endpoint.url();
    let mut stream = connect(endpoint, bounds)?;
    let exchange = |stream: &mut TcpStream| -> io::Result<Vec<u8>> {
        stream.set_read_timeout(Some(bounds.answer))?;
        stream.set_write_timeout(Some(bounds.answer))?;
        stream.write_all(head(endpoint, body.len()).as_bytes())?;
        stream.write_all(body)?;
        stream.flush()?;
        read_answer(stream)
    };
    let raw = exchange(&mut stream).map_err(|err| lost(&url, err, bounds))?;
    judge(&url, &raw)
}

fn connect(endpoint: &Endpoint, bounds: &Bounds) -> io::Result<TcpStream> {
    let mut last = io::Error::from(io::ErrorKind::AddrNotAvailable);
    for addr in &endpoint.addrs {
        match TcpStream::connect_timeout(addr, bounds.connect) {
            Ok(stream) => return Ok(stream),
            Err(err) => last = err,
        }
    }
    let at = endpoint
        .addrs
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(" or ");
    let message = match last.kind() {
        io::ErrorKind::ConnectionRefused => format!(
            "no collector is listening at {at}; start one with an OTLP/HTTP receiver \
             (the OpenTelemetry Collector, Grafana Alloy and the Datadog Agent take it \
             on 127.0.0.1:4318), or check the URL"
        ),
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => format!(
            "the collector at {at} did not accept a connection within {:?}",
            bounds.connect
        ),
        _ => format!("could not reach the collector at {at}: {last}"),
    };
    Err(io::Error::new(last.kind(), message))
}

/// The request line and headers. `User-Agent` names the program the way the
/// OTLP exporter specification asks; nothing in it names the machine.
fn head(endpoint: &Endpoint, length: usize) -> String {
    format!(
        "POST {} HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {length}\r\nUser-Agent: conbus/{}\r\nConnection: close\r\n\r\n",
        endpoint.path,
        endpoint.authority,
        crate::build_info::VERSION
    )
}

/// What went wrong between connecting and the answer.
fn lost(url: &str, err: io::Error, bounds: &Bounds) -> io::Error {
    let message = match err.kind() {
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => format!(
            "the collector at {url} did not answer within {:?}; the points were not \
             retried, since a collector that did take them would count them twice",
            bounds.answer
        ),
        _ => format!("the connection to the collector at {url} failed before it answered: {err}"),
    };
    io::Error::new(err.kind(), message)
}

/// The answer, read until it is complete, the collector hangs up, or the limit.
fn read_answer(stream: &mut impl Read) -> io::Result<Vec<u8>> {
    let mut raw = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => return Ok(raw),
            Ok(n) => {
                raw.extend_from_slice(&chunk[..n]);
                if raw.len() >= ANSWER_LIMIT || complete(&raw) {
                    return Ok(raw);
                }
            }
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            // A collector that answered and kept the connection open anyway,
            // with a body whose end it did not mark: the status is what counts.
            Err(_) if Head::parse(&raw).is_some() => return Ok(raw),
            Err(err) => return Err(err),
        }
    }
}

/// Whether `raw` holds a whole answer — or enough to say it is not HTTP.
fn complete(raw: &[u8]) -> bool {
    if raw.len() >= 5 && !raw.starts_with(b"HTTP/") {
        return true;
    }
    let Some(head) = Head::parse(raw) else {
        return false;
    };
    let rest = &raw[head.len..];
    if head.chunked() {
        return unchunk(rest).is_some();
    }
    head.content_length()
        .is_some_and(|length| rest.len() >= length)
}

fn judge(url: &str, raw: &[u8]) -> io::Result<Answer> {
    let Some(head) = Head::parse(raw) else {
        let message = if raw.is_empty() {
            format!("the collector at {url} closed the connection without answering")
        } else {
            format!(
                "the collector at {url} answered with something that is not HTTP: {}",
                detail(raw).unwrap_or_default()
            )
        };
        return Err(io::Error::new(io::ErrorKind::InvalidData, message));
    };
    let body = head.body(raw);
    if !(200..300).contains(&head.status) {
        let said = detail(&body)
            .map(|text| format!(": {text}"))
            .unwrap_or_default();
        let retry = head
            .header("retry-after")
            .filter(|_| matches!(head.status, 429 | 503))
            .map(|after| {
                let after = if after.bytes().all(|b| b.is_ascii_digit()) {
                    format!("{after}s")
                } else {
                    after.to_string()
                };
                format!("; it asks to be tried again after {after}")
            })
            .unwrap_or_default();
        let status = format!("{} {}", head.status, head.reason);
        return Err(io::Error::other(format!(
            "the collector at {url} answered {}{said}{retry}",
            status.trim_end()
        )));
    }
    Ok(Answer {
        status: head.status,
        reason: head.reason,
        rejected: rejected(&body),
    })
}

/// `partialSuccess.rejectedDataPoints` above zero, and the collector's
/// `errorMessage`. The count is an int64, so a decimal string in OTLP/JSON —
/// though receivers accept a number, and so does this.
fn rejected(body: &[u8]) -> Option<(u64, String)> {
    let answer: Value = serde_json::from_slice(body).ok()?;
    let partial = answer.get("partialSuccess")?;
    let count = match partial.get("rejectedDataPoints")? {
        Value::String(text) => text.parse().ok()?,
        other => other.as_u64()?,
    };
    let said = partial
        .get("errorMessage")
        .and_then(Value::as_str)
        .unwrap_or_default();
    (count > 0).then(|| (count, said.to_string()))
}

/// What the collector said, on one line: a `Status` message when it sent one,
/// else the start of whatever it sent, with anything that is not text dropped.
fn detail(body: &[u8]) -> Option<String> {
    let status = serde_json::from_slice::<Value>(body).ok();
    let text = match status.as_ref().and_then(|v| v.get("message")?.as_str()) {
        Some(message) => message.to_string(),
        None => String::from_utf8_lossy(body).into_owned(),
    };
    let line = text
        .split(|c: char| c.is_whitespace() || c.is_control())
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    (!line.is_empty()).then(|| line.chars().take(DETAIL_CHARS).collect())
}

/// An answer's status line and headers.
struct Head {
    status: u16,
    reason: String,
    headers: Vec<(String, String)>,
    /// Bytes up to and including the blank line.
    len: usize,
}

impl Head {
    /// The final answer's head. An interim `1xx` before it is read past, as a
    /// client must even when it asked for none (RFC 9110, section 15.2).
    fn parse(raw: &[u8]) -> Option<Head> {
        let head = Self::parse_one(raw)?;
        if !(100..200).contains(&head.status) {
            return Some(head);
        }
        let last = Self::parse(&raw[head.len..])?;
        Some(Head {
            len: head.len + last.len,
            ..last
        })
    }

    fn parse_one(raw: &[u8]) -> Option<Head> {
        let end = raw.windows(4).position(|w| w == b"\r\n\r\n")?;
        let text = std::str::from_utf8(&raw[..end]).ok()?;
        let mut lines = text.split("\r\n");
        let mut status_line = lines.next()?.splitn(3, ' ');
        if !status_line.next()?.starts_with("HTTP/1.") {
            return None;
        }
        let status = status_line.next()?.parse().ok()?;
        let reason = status_line.next().unwrap_or_default().to_string();
        let headers = lines
            .filter_map(|line| line.split_once(':'))
            .map(|(name, value)| (name.trim().to_string(), value.trim().to_string()))
            .collect();
        Some(Head {
            status,
            reason,
            headers,
            len: end + 4,
        })
    }

    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    fn content_length(&self) -> Option<usize> {
        self.header("content-length")?.parse().ok()
    }

    fn chunked(&self) -> bool {
        self.header("transfer-encoding")
            .is_some_and(|coding| coding.to_ascii_lowercase().contains("chunked"))
    }

    /// The body as sent, decoded when it was chunked.
    fn body(&self, raw: &[u8]) -> Vec<u8> {
        let rest = raw.get(self.len..).unwrap_or_default();
        if self.chunked() {
            return unchunk(rest).unwrap_or_default();
        }
        match self.content_length() {
            Some(length) => rest[..length.min(rest.len())].to_vec(),
            None => rest.to_vec(),
        }
    }
}

/// A chunked body, decoded; none until its last chunk has arrived.
fn unchunk(mut body: &[u8]) -> Option<Vec<u8>> {
    let mut decoded = Vec::new();
    loop {
        let line_end = body.windows(2).position(|w| w == b"\r\n")?;
        let size_line = std::str::from_utf8(&body[..line_end]).ok()?;
        let size_text = size_line.split(';').next().unwrap_or_default().trim();
        let size = usize::from_str_radix(size_text, 16).ok()?;
        body = &body[line_end + 2..];
        if size == 0 {
            return Some(decoded);
        }
        decoded.extend_from_slice(body.get(..size)?);
        body = body.get(size + 2..)?;
    }
}

/// A collector on 127.0.0.1 for tests: it takes one request, answers it, and
/// hands the request back.
#[cfg(test)]
pub(super) mod fake {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread::{self, JoinHandle};

    use super::super::Endpoint;

    /// An HTTP answer with a JSON body and the length that goes with it.
    pub fn answer(status: &str, body: &str) -> String {
        format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
    }

    pub struct Collector {
        pub endpoint: Endpoint,
        request: JoinHandle<Vec<u8>>,
    }

    impl Collector {
        /// Answer the first request with `answer`, then keep the connection
        /// open until the client closes it: a client that waited for the
        /// collector to hang up would wait here.
        pub fn answering(answer: &str) -> Self {
            Self::serve(Some(answer.to_string()), true)
        }

        /// Answer the first request with `answer` and hang up at once.
        pub fn hanging_up(answer: &str) -> Self {
            Self::serve(Some(answer.to_string()), false)
        }

        /// Take the request and never answer it.
        pub fn silent() -> Self {
            Self::serve(None, true)
        }

        fn serve(answer: Option<String>, linger: bool) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
            let port = listener.local_addr().expect("addr").port();
            let request = thread::spawn(move || {
                let Ok((mut stream, _)) = listener.accept() else {
                    return Vec::new();
                };
                let request = read_request(&mut stream);
                if let Some(answer) = answer {
                    let _ = stream.write_all(answer.as_bytes());
                }
                if linger {
                    let _ = stream.read_to_end(&mut Vec::new());
                }
                request
            });
            Collector {
                endpoint: Endpoint::parse(&format!("http://127.0.0.1:{port}")).expect("endpoint"),
                request,
            }
        }

        /// The request as the collector received it, head and body.
        pub fn request(self) -> Vec<u8> {
            self.request.join().expect("the collector thread")
        }
    }

    /// One request: its head, then as many bytes as its `Content-Length`.
    fn read_request(stream: &mut impl Read) -> Vec<u8> {
        let mut raw = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let Ok(n) = stream.read(&mut chunk) else {
                return raw;
            };
            if n == 0 {
                return raw;
            }
            raw.extend_from_slice(&chunk[..n]);
            if let Some(end) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&raw[..end]).to_ascii_lowercase();
                let length = head
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length:"))
                    .and_then(|value| value.trim().parse::<usize>().ok())
                    .unwrap_or(0);
                if raw.len() >= end + 4 + length {
                    return raw;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::{Collector, answer};
    use super::*;
    use std::time::Instant;

    const BODY: &[u8] = br#"{"resourceMetrics":[]}"#;

    #[test]
    fn a_post_is_one_request_that_asks_the_collector_to_close() {
        let collector = Collector::answering(&answer("200 OK", "{}"));

        let began = Instant::now();
        let answer = post(&collector.endpoint, BODY, &BOUNDS).expect("taken");

        // The collector keeps the connection open after answering: a complete
        // answer is the end, not the collector hanging up.
        assert!(
            began.elapsed() < Duration::from_secs(5),
            "{:?}",
            began.elapsed()
        );
        assert_eq!(
            answer,
            Answer {
                status: 200,
                reason: "OK".into(),
                rejected: None
            }
        );
        let request = String::from_utf8(collector.request()).expect("utf8");
        let (head, body) = request.split_once("\r\n\r\n").expect("a head");
        let port = collector_port(head);
        assert_eq!(
            head,
            format!(
                "POST /v1/metrics HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nUser-Agent: conbus/{}\r\nConnection: close",
                BODY.len(),
                crate::build_info::VERSION
            )
        );
        assert_eq!(body.as_bytes(), BODY);
    }

    fn collector_port(head: &str) -> String {
        head.lines()
            .find_map(|line| line.strip_prefix("Host: 127.0.0.1:"))
            .expect("a Host header")
            .to_string()
    }

    #[test]
    fn a_closed_port_is_an_error_that_says_nobody_is_listening() {
        let port = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
            listener.local_addr().expect("addr").port()
        };
        let endpoint = Endpoint::parse(&format!("http://127.0.0.1:{port}")).expect("endpoint");

        let err = post(&endpoint, BODY, &BOUNDS).expect_err("nobody is there");

        assert_eq!(err.kind(), io::ErrorKind::ConnectionRefused);
        let text = err.to_string();
        assert!(
            text.contains(&format!("no collector is listening at 127.0.0.1:{port}")),
            "{text}"
        );
        assert!(
            text.contains("127.0.0.1:4318"),
            "names where collectors listen: {text}"
        );
    }

    #[test]
    fn an_answer_other_than_2xx_is_an_error_that_says_what_the_collector_said() {
        // OTLP's failure body is a `Status` message; its `message` is the
        // part a person can act on.
        let collector = Collector::answering(&answer(
            "400 Bad Request",
            r#"{"code":3,"message":"unknown field \"datapoints\""}"#,
        ));
        let err = post(&collector.endpoint, BODY, &BOUNDS).expect_err("refused");
        collector.request();
        let text = err.to_string();
        assert!(text.contains("answered 400 Bad Request"), "{text}");
        assert!(text.contains("unknown field \"datapoints\""), "{text}");
        assert!(text.contains("/v1/metrics"), "names the URL: {text}");

        let busy = Collector::answering(
            "HTTP/1.1 503 Service Unavailable\r\nRetry-After: 30\r\nContent-Length: 4\r\n\r\nbusy",
        );
        let err = post(&busy.endpoint, BODY, &BOUNDS).expect_err("busy");
        busy.request();
        let text = err.to_string();
        assert!(
            text.contains("answered 503 Service Unavailable: busy"),
            "{text}"
        );
        assert!(text.contains("after 30s"), "passes on Retry-After: {text}");

        let redirect = Collector::answering(
            "HTTP/1.1 301 Moved Permanently\r\nLocation: http://elsewhere/\r\nContent-Length: 0\r\n\r\n",
        );
        let err = post(&redirect.endpoint, BODY, &BOUNDS).expect_err("not followed");
        redirect.request();
        assert!(err.to_string().contains("answered 301"), "{err}");
    }

    #[test]
    fn a_partial_success_is_read_whichever_way_the_count_is_written() {
        // int64 is a decimal string in OTLP/JSON, and receivers accept a
        // number too; either can come back.
        for rejected in [r#""3""#, "3"] {
            let collector = Collector::answering(&answer(
                "200 OK",
                &format!(
                    r#"{{"partialSuccess":{{"rejectedDataPoints":{rejected},"errorMessage":"too old"}}}}"#
                ),
            ));
            let answer = post(&collector.endpoint, BODY, &BOUNDS).expect("taken");
            collector.request();
            assert_eq!(
                answer.rejected,
                Some((3, "too old".to_string())),
                "{rejected}"
            );
        }
        // Zero rejected is a full success, whatever else it says.
        let collector = Collector::answering(&answer(
            "200 OK",
            r#"{"partialSuccess":{"rejectedDataPoints":"0","errorMessage":"a warning"}}"#,
        ));
        let answer = post(&collector.endpoint, BODY, &BOUNDS).expect("taken");
        collector.request();
        assert_eq!(answer.rejected, None);
    }

    #[test]
    fn an_interim_answer_is_read_past_to_the_final_one() {
        let collector = Collector::answering(&format!(
            "HTTP/1.1 100 Continue\r\n\r\n{}",
            answer("200 OK", "{}")
        ));
        let answer = post(&collector.endpoint, BODY, &BOUNDS).expect("taken");
        collector.request();
        assert_eq!((answer.status, answer.reason.as_str()), (200, "OK"));
    }

    #[test]
    fn a_chunked_answer_is_read_like_any_other() {
        assert_eq!(
            unchunk(b"5\r\nhello\r\n6;name=x\r\n world\r\n0\r\n\r\n"),
            Some(b"hello world".to_vec())
        );
        assert_eq!(unchunk(b"5\r\nhel"), None, "not all of it yet");
        let body = r#"{"partialSuccess":{"rejectedDataPoints":"1"}}"#;
        let collector = Collector::answering(&format!(
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n{body}\r\n0\r\n\r\n",
            body.len()
        ));
        let answer = post(&collector.endpoint, BODY, &BOUNDS).expect("taken");
        collector.request();
        assert_eq!(answer.rejected, Some((1, String::new())));
    }

    #[test]
    fn a_collector_that_never_answers_is_given_up_on() {
        // A short bound rather than the shipping ten seconds, so the test
        // costs a tenth of a second.
        let collector = Collector::silent();
        let bounds = Bounds {
            connect: Duration::from_secs(1),
            answer: Duration::from_millis(100),
        };

        let began = Instant::now();
        let err = post(&collector.endpoint, BODY, &bounds).expect_err("no answer");

        assert!(
            began.elapsed() < Duration::from_secs(5),
            "{:?}",
            began.elapsed()
        );
        let text = err.to_string();
        assert!(text.contains("did not answer within 100ms"), "{text}");
        assert!(text.contains("not retried"), "{text}");
        collector.request();
    }

    #[test]
    fn a_collector_that_hangs_up_or_speaks_something_else_is_an_error() {
        let hung_up = Collector::hanging_up("");
        let err = post(&hung_up.endpoint, BODY, &BOUNDS).expect_err("no answer");
        hung_up.request();
        assert!(
            err.to_string()
                .contains("closed the connection without answering"),
            "{err}"
        );

        // Something that is not HTTP is not waited on for the rest of it.
        let garbled = Collector::answering("SSH-2.0-OpenSSH_9.8\r\n");
        let began = Instant::now();
        let err = post(&garbled.endpoint, BODY, &BOUNDS).expect_err("not HTTP");
        assert!(
            began.elapsed() < Duration::from_secs(5),
            "{:?}",
            began.elapsed()
        );
        garbled.request();
        assert!(err.to_string().contains("not HTTP"), "{err}");
    }
}
