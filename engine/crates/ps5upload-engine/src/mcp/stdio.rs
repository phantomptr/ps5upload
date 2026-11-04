//! `ps5upload-engine mcp [--url http://127.0.0.1:19113]`: the stdio transport, for clients that
//! start a server as a child process.
//!
//! It is a pure bridge: each line of stdin is one JSON-RPC message, POSTed to the running engine's
//! `/mcp`, and the answer is one line of stdout. It never starts an engine: the running one owns
//! the consoles, and two would fight over them. When none answers, a request gets a JSON-RPC error
//! that says what to do, because that is what a user of the client will read.
//!
//! Nothing but JSON ever reaches stdout; logs go to stderr, and never panic on a closed one (see
//! `stderr_quiet` in `lib.rs`). Requests run side by side, each on its own thread, so a long
//! `jobs_wait` does not hold up a `ping`. On stdin's end the bridge lets what is in flight answer,
//! for a while, then exits 0.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::jsonrpc::{self, RpcError, INTERNAL_ERROR, PARSE_ERROR};

/// The engine's default port, as `run_cli` in `lib.rs` has it.
const DEFAULT_PORT: &str = "19113";
/// Requests in flight at once. Past this, the bridge stops reading stdin until one answers.
const MAX_IN_FLIGHT: usize = 64;
/// How long requests in flight get to answer after stdin ends.
const DRAIN_WAIT: Duration = Duration::from_secs(30);
/// How long to wait for the engine to accept a connection. On one machine this is instant or never.
const CONNECT_WAIT: Duration = Duration::from_secs(5);
/// How much of a non-JSON answer goes into an error message.
const MAX_QUOTED_ANSWER: usize = 400;

const USAGE: &str = "usage: ps5upload-engine mcp [--url http://127.0.0.1:19113]

Bridges an MCP client's stdio to a running PS5Upload engine.
The engine's URL comes from --url, else PS5UPLOAD_ENGINE_URL, else http://127.0.0.1 with the port
in PS5UPLOAD_ENGINE_PORT (19113 if unset).";

/// One answer from the engine.
pub(crate) struct Reply {
    pub status: u16,
    pub content_type: String,
    pub body: Box<dyn Read + Send>,
}

/// How a message reaches the engine. In tests, not over a network.
pub(crate) trait Transport: Send + Sync + 'static {
    /// POSTs one message. `Err` is why no answer came, for the log.
    fn post(&self, body: &[u8], headers: &[(&'static str, String)]) -> Result<Reply, String>;
    /// The engine's URL, as the user would recognise it.
    fn url(&self) -> &str;
}

/// The real transport.
struct HttpTransport {
    agent: ureq::Agent,
    base: String,
    endpoint: String,
}

impl HttpTransport {
    fn new(base: String) -> Self {
        let agent = ureq::Agent::config_builder()
            // The engine is on this computer. An `http_proxy` in the environment would otherwise
            // take even 127.0.0.1 to a proxy, which reads as "the engine is not running".
            .proxy(None)
            .http_status_as_error(false)
            .timeout_connect(Some(CONNECT_WAIT))
            .build()
            .into();
        let endpoint = format!("{base}/mcp");
        Self {
            agent,
            base,
            endpoint,
        }
    }
}

impl Transport for HttpTransport {
    fn post(&self, body: &[u8], headers: &[(&'static str, String)]) -> Result<Reply, String> {
        let mut request = self
            .agent
            .post(&self.endpoint)
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream");
        for (name, value) in headers {
            request = request.header(*name, value.as_str());
        }
        let response = request.send(body).map_err(|e| e.to_string())?;
        let status = response.status().as_u16();
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string();
        Ok(Reply {
            status,
            content_type,
            body: Box::new(response.into_body().into_reader()),
        })
    }

    fn url(&self) -> &str {
        &self.base
    }
}

/// The CLI entry: `args` are what follows `mcp`. The process's exit code.
pub fn run_cli(args: Vec<String>) -> i32 {
    let url = match engine_url(&args, |name| std::env::var(name).ok()) {
        Ok(Some(url)) => url,
        Ok(None) => {
            log_line(&mut io::stderr(), USAGE);
            return 0;
        }
        Err(problem) => {
            log_line(&mut io::stderr(), &format!("{problem}\n\n{USAGE}"));
            return 2;
        }
    };
    log_line(
        &mut io::stderr(),
        &format!("[ps5upload-engine mcp] bridging stdio to {url}/mcp"),
    );
    run_bridge(
        io::stdin().lock(),
        io::stdout(),
        io::stderr(),
        Arc::new(HttpTransport::new(url)),
    )
}

/// The engine's base URL. `Ok(None)`: help was asked for.
fn engine_url(
    args: &[String],
    env: impl Fn(&str) -> Option<String>,
) -> Result<Option<String>, String> {
    let mut url = None;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => return Ok(None),
            "--url" => {
                let Some(value) = args.next() else {
                    return Err("--url needs a URL.".to_string());
                };
                url = Some(value.clone());
            }
            other => match other.strip_prefix("--url=") {
                Some(value) => url = Some(value.to_string()),
                None => return Err(format!("Unknown argument: {other}")),
            },
        }
    }
    let present = |name: &str| env(name).filter(|value| !value.trim().is_empty());
    let url = url
        .or_else(|| present("PS5UPLOAD_ENGINE_URL"))
        .unwrap_or_else(|| {
            let port = present("PS5UPLOAD_ENGINE_PORT").unwrap_or_else(|| DEFAULT_PORT.to_string());
            // The engine answers under this prefix when it has one (`healthcheck` in lib.rs).
            let base = present("PS5UPLOAD_BASE_URL").unwrap_or_default();
            let base = base.trim().trim_matches('/');
            if base.is_empty() {
                format!("http://127.0.0.1:{}", port.trim())
            } else {
                format!("http://127.0.0.1:{}/{base}", port.trim())
            }
        });
    let url = url.trim().trim_end_matches('/');
    let url = url.strip_suffix("/mcp").unwrap_or(url);
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err(format!(
            "The engine's URL must start with http:// or https://, but it is {url:?}."
        ));
    }
    Ok(Some(url.to_string()))
}

/// Requests still answering.
#[derive(Default)]
struct InFlight {
    count: Mutex<usize>,
    changed: Condvar,
}

impl InFlight {
    fn lock(&self) -> std::sync::MutexGuard<'_, usize> {
        self.count.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Counts one more, waiting while there are too many.
    fn enter(&self) {
        let mut count = self.lock();
        while *count >= MAX_IN_FLIGHT {
            count = self.changed.wait(count).unwrap_or_else(|e| e.into_inner());
        }
        *count += 1;
    }

    fn leave(&self) {
        *self.lock() -= 1;
        self.changed.notify_all();
    }

    /// Waits for all to answer, for at most `limit`.
    fn drain(&self, limit: Duration) {
        let deadline = Instant::now() + limit;
        let mut count = self.lock();
        while *count > 0 {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return;
            }
            count = self
                .changed
                .wait_timeout(count, left)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }
}

/// The two writers, shared by the threads.
struct Output {
    lines: Mutex<Box<dyn Write + Send>>,
    log: Mutex<Box<dyn Write + Send>>,
}

impl Output {
    /// One line of stdout: one JSON-RPC message. A client that went away is not an error here: its
    /// closing stdin ends the bridge.
    fn message(&self, message: &Value) {
        let mut lines = self.lines.lock().unwrap_or_else(|e| e.into_inner());
        let _ = writeln!(lines, "{message}");
        let _ = lines.flush();
    }

    fn log(&self, text: &str) {
        log_line(
            &mut *self.log.lock().unwrap_or_else(|e| e.into_inner()),
            &format!("[ps5upload-engine mcp] {text}"),
        );
    }
}

fn log_line(out: &mut dyn Write, text: &str) {
    let _ = writeln!(out, "{text}");
}

/// Bridges until `input` ends. The process's exit code: 0.
pub(crate) fn run_bridge(
    mut input: impl BufRead,
    output: impl Write + Send + 'static,
    log: impl Write + Send + 'static,
    transport: Arc<dyn Transport>,
) -> i32 {
    let output = Arc::new(Output {
        lines: Mutex::new(Box::new(output)),
        log: Mutex::new(Box::new(log)),
    });
    let in_flight = Arc::new(InFlight::default());
    let mut line = Vec::new();
    loop {
        line.clear();
        match input.read_until(b'\n', &mut line) {
            Ok(0) => break,
            Ok(_) => {}
            Err(e) => {
                output.log(&format!("reading stdin failed: {e}"));
                break;
            }
        }
        let text = line.trim_ascii();
        if text.is_empty() {
            continue;
        }
        let message: Value = match serde_json::from_slice(text) {
            Ok(message) => message,
            Err(e) => {
                let error = RpcError::new(PARSE_ERROR, format!("That line is not JSON: {e}"));
                output.message(&jsonrpc::failure(None, &error));
                continue;
            }
        };
        let body = text.to_vec();
        let streaming =
            message.get("method").and_then(Value::as_str) == Some("subscriptions/listen");
        let (transport, output, tracker) = (transport.clone(), output.clone(), in_flight.clone());
        // A stream stays open for as long as the client: it is not something to wait for.
        if !streaming {
            tracker.enter();
        }
        std::thread::spawn(move || {
            forward(&*transport, &output, &message, &body);
            if !streaming {
                tracker.leave();
            }
        });
    }
    in_flight.drain(DRAIN_WAIT);
    0
}

/// Sends one message and writes what comes back.
fn forward(transport: &dyn Transport, output: &Output, message: &Value, body: &[u8]) {
    let ids = request_ids(message);
    match transport.post(body, &mirror_headers(message)) {
        Ok(reply) => relay(transport, output, reply, &ids),
        Err(detail) => {
            let url = transport.url();
            output.log(&format!("could not reach the engine at {url}: {detail}"));
            let mut error = RpcError::new(
                INTERNAL_ERROR,
                format!(
                    "PS5Upload's engine is not running at {url}. Open the PS5Upload app, or run \
                     ps5upload-engine, then try again."
                ),
            );
            error.data = Some(json!({ "url": url, "detail": detail }));
            for id in &ids {
                output.message(&jsonrpc::failure(Some(id), &error));
            }
        }
    }
}

/// Writes what the engine answered: the stream's messages, or the one message, as lines.
fn relay(transport: &dyn Transport, output: &Output, reply: Reply, ids: &[Value]) {
    let status = reply.status;
    if reply.content_type.starts_with("text/event-stream") {
        relay_events(output, reply.body);
        return;
    }
    let mut text = Vec::new();
    if let Err(e) = reply.body.take(u64::MAX).read_to_end(&mut text) {
        output.log(&format!("reading the engine's answer failed: {e}"));
    }
    if ids.is_empty() {
        // A notification or a client's answer: nothing to say back, whatever came.
        if !(200..300).contains(&status) {
            output.log(&format!(
                "the engine answered a notification with HTTP {status}"
            ));
        }
        return;
    }
    match serde_json::from_slice::<Value>(&text) {
        Ok(mut answer) if answer.is_array() || answer.get("jsonrpc").is_some() => {
            // An error the engine could not tie to a request (the origin guard's, say): this one
            // line of stdin is the request it answers.
            if let ([id], Some(fields)) = (ids, answer.as_object_mut()) {
                fields.entry("id").or_insert_with(|| id.clone());
            }
            output.message(&answer);
        }
        _ => {
            let error = not_an_answer(transport.url(), status, &text);
            output.log(&error.message);
            for id in ids {
                output.message(&jsonrpc::failure(Some(id), &error));
            }
        }
    }
}

/// The error for an answer that is not JSON-RPC: something else stands at that URL, or something
/// refused the request before the MCP server saw it (the loopback guard does, in plain text).
fn not_an_answer(url: &str, status: u16, body: &[u8]) -> RpcError {
    let quoted: String = String::from_utf8_lossy(body)
        .trim()
        .chars()
        .take(MAX_QUOTED_ANSWER)
        .collect();
    let message = match status {
        404 => format!(
            "The engine at {url} has no MCP endpoint (HTTP 404): it is older than this bridge, or \
             something else answers at that URL. Update PS5Upload, or check the URL."
        ),
        _ if quoted.is_empty() => {
            format!("The engine at {url} answered HTTP {status} with nothing to read.")
        }
        _ => format!("The engine at {url} answered HTTP {status}: {quoted}"),
    };
    RpcError::new(INTERNAL_ERROR, message)
}

/// Writes each message of an event stream as one line, as it comes.
fn relay_events(output: &Output, body: Box<dyn Read + Send>) {
    let reader = BufReader::new(body);
    let mut data = String::new();
    for line in reader.lines() {
        let Ok(line) = line else {
            break;
        };
        if line.is_empty() {
            write_event(output, &mut data);
        } else if let Some(field) = line.strip_prefix("data:") {
            data.push_str(field.strip_prefix(' ').unwrap_or(field));
        }
        // Comments (the keep-alive) and other fields are for the HTTP stream only.
    }
    write_event(output, &mut data);
}

fn write_event(output: &Output, data: &mut String) {
    if !data.is_empty() {
        if let Ok(message) = serde_json::from_str::<Value>(data) {
            output.message(&message);
        }
        data.clear();
    }
}

/// The ids of a message's requests: what must get an answer.
fn request_ids(message: &Value) -> Vec<Value> {
    let id_of = |item: &Value| {
        item.get("method")?;
        item.get("id").filter(|id| !id.is_null()).cloned()
    };
    match message {
        Value::Array(items) => items.iter().filter_map(id_of).collect(),
        _ => id_of(message).into_iter().collect(),
    }
}

/// The headers a modern request must repeat from its body. A legacy one needs none: without a
/// version header it is taken as legacy, which it is.
fn mirror_headers(message: &Value) -> Vec<(&'static str, String)> {
    let empty = Value::Null;
    let params = message.get("params").unwrap_or(&empty);
    let Some(version) = jsonrpc::meta_protocol_version(params).and_then(Value::as_str) else {
        return Vec::new();
    };
    let Some(method) = message.get("method").and_then(Value::as_str) else {
        return Vec::new();
    };
    let mut headers = vec![
        ("MCP-Protocol-Version", version.to_string()),
        ("Mcp-Method", method.to_string()),
    ];
    if let Some(subject) = jsonrpc::named_subject(method, params) {
        headers.push(("Mcp-Name", jsonrpc::encode_header_name(subject)));
    }
    headers
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A shared buffer, for what the bridge writes.
    #[derive(Clone, Default)]
    struct Sink(Arc<Mutex<Vec<u8>>>);

    impl Write for Sink {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Sink {
        fn text(&self) -> String {
            String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
        }

        /// Every line, each of which must be JSON.
        fn messages(&self) -> Vec<Value> {
            self.text()
                .lines()
                .map(|line| serde_json::from_str(line).expect("only JSON on stdout"))
                .collect()
        }
    }

    type Answer = fn(&Value) -> Result<(u16, &'static str, String), String>;
    /// A message and the headers it came with.
    type Sent = (Value, Vec<(&'static str, String)>);

    /// Answers by `answer`, and keeps what it was sent.
    struct Fake {
        answer: Answer,
        seen: Mutex<Vec<Sent>>,
    }

    impl Fake {
        fn new(answer: Answer) -> Arc<Self> {
            Arc::new(Self {
                answer,
                seen: Mutex::new(Vec::new()),
            })
        }
    }

    impl Transport for Fake {
        fn post(&self, body: &[u8], headers: &[(&'static str, String)]) -> Result<Reply, String> {
            let message: Value = serde_json::from_slice(body).unwrap();
            let (status, content_type, text) = (self.answer)(&message)?;
            self.seen.lock().unwrap().push((message, headers.to_vec()));
            Ok(Reply {
                status,
                content_type: content_type.to_string(),
                body: Box::new(Cursor::new(text.into_bytes())),
            })
        }

        fn url(&self) -> &str {
            "http://127.0.0.1:19113"
        }
    }

    /// The way the real endpoint behaves: a request gets a result, anything else a 202.
    fn engine(message: &Value) -> Result<(u16, &'static str, String), String> {
        Ok(match message.get("id") {
            Some(id) => (
                200,
                "application/json",
                json!({"jsonrpc": "2.0", "id": id, "result": {"echo": message["method"]}})
                    .to_string(),
            ),
            None => (202, "", String::new()),
        })
    }

    fn bridge(input: &str, transport: Arc<dyn Transport>) -> (i32, Sink, Sink) {
        let (out, log) = (Sink::default(), Sink::default());
        let code = run_bridge(
            Cursor::new(input.as_bytes().to_vec()),
            out.clone(),
            log.clone(),
            transport,
        );
        (code, out, log)
    }

    fn by_id(messages: Vec<Value>, id: i64) -> Value {
        messages
            .into_iter()
            .find(|m| m["id"] == id)
            .unwrap_or_else(|| panic!("no answer to {id}"))
    }

    #[test]
    fn a_request_gets_one_line_back_and_a_notification_none() {
        let input = concat!(
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
            "\n",
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            "\r\n",
            "\n",
            "   \n",
            r#"{"jsonrpc":"2.0","id":2,"method":"ping"}"#,
        );
        let (code, out, _) = bridge(input, Fake::new(engine));
        assert_eq!(code, 0);
        let messages = out.messages();
        assert_eq!(messages.len(), 2, "{}", out.text());
        assert_eq!(by_id(messages.clone(), 1)["result"]["echo"], "tools/list");
        // The last line has no newline: it still counts.
        assert_eq!(by_id(messages, 2)["result"]["echo"], "ping");
    }

    #[test]
    fn a_line_that_is_not_json_gets_a_parse_error_and_the_bridge_goes_on() {
        let input = "not json\n{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"ping\"}\n";
        let (code, out, _) = bridge(input, Fake::new(engine));
        assert_eq!(code, 0);
        let messages = out.messages();
        assert_eq!(messages.len(), 2);
        let parse_error = messages.iter().find(|m| m.get("error").is_some()).unwrap();
        assert_eq!(parse_error["error"]["code"], PARSE_ERROR);
        assert!(parse_error.get("id").is_none());
        assert_eq!(by_id(messages, 3)["result"]["echo"], "ping");
    }

    #[test]
    fn bytes_that_are_not_text_are_a_parse_error_too() {
        let (out, log) = (Sink::default(), Sink::default());
        let input = b"\xff\xfe\n".to_vec();
        let code = run_bridge(Cursor::new(input), out.clone(), log, Fake::new(engine));
        assert_eq!(code, 0);
        assert_eq!(out.messages()[0]["error"]["code"], PARSE_ERROR);
    }

    fn unreachable_engine(_: &Value) -> Result<(u16, &'static str, String), String> {
        Err("Connection refused".to_string())
    }

    #[test]
    fn no_engine_is_a_json_rpc_error_that_says_what_to_do() {
        let input = concat!(
            r#"{"jsonrpc":"2.0","id":"a","method":"tools/list"}"#,
            "\n",
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            "\n",
        );
        let (code, out, log) = bridge(input, Fake::new(unreachable_engine));
        assert_eq!(code, 0);
        let messages = out.messages();
        // The request gets the error; the notification gets nothing.
        assert_eq!(messages.len(), 1, "{}", out.text());
        assert_eq!(messages[0]["id"], "a");
        assert_eq!(messages[0]["error"]["code"], INTERNAL_ERROR);
        assert_eq!(
            messages[0]["error"]["message"],
            "PS5Upload's engine is not running at http://127.0.0.1:19113. Open the PS5Upload app, \
             or run ps5upload-engine, then try again."
        );
        // The detail goes to the log, for both.
        assert!(log.text().contains("Connection refused"), "{}", log.text());
    }

    #[test]
    fn a_json_rpc_error_status_passes_through_as_the_engine_said_it() {
        fn refuses(_: &Value) -> Result<(u16, &'static str, String), String> {
            Ok((
                400,
                "application/json",
                json!({"jsonrpc": "2.0", "id": 5, "error": {"code": -32020, "message": "no"}})
                    .to_string(),
            ))
        }
        let (_, out, _) = bridge(
            "{\"jsonrpc\":\"2.0\",\"id\":5,\"method\":\"ping\"}\n",
            Fake::new(refuses),
        );
        assert_eq!(out.messages()[0]["error"]["code"], -32020);
    }

    #[test]
    fn an_answer_with_no_id_is_tied_to_its_request() {
        fn origin_guard(_: &Value) -> Result<(u16, &'static str, String), String> {
            Ok((
                403,
                "application/json",
                json!({"jsonrpc": "2.0", "error": {"code": -32600, "message": "no"}}).to_string(),
            ))
        }
        let (_, out, _) = bridge(
            "{\"jsonrpc\":\"2.0\",\"id\":6,\"method\":\"ping\"}\n",
            Fake::new(origin_guard),
        );
        assert_eq!(out.messages()[0]["id"], 6);
    }

    #[test]
    fn a_plain_text_refusal_becomes_an_error_with_its_words() {
        fn guard(_: &Value) -> Result<(u16, &'static str, String), String> {
            Ok((
                403,
                "text/plain",
                "loopback only: this engine refused 10.0.0.2.\nTo allow it...".to_string(),
            ))
        }
        let (_, out, log) = bridge(
            "{\"jsonrpc\":\"2.0\",\"id\":7,\"method\":\"ping\"}\n",
            Fake::new(guard),
        );
        let messages = out.messages();
        assert_eq!(messages[0]["id"], 7);
        assert_eq!(messages[0]["error"]["code"], INTERNAL_ERROR);
        let message = messages[0]["error"]["message"].as_str().unwrap();
        assert!(
            message.contains("HTTP 403") && message.contains("loopback only"),
            "{message}"
        );
        assert!(log.text().contains("loopback only"));
    }

    #[test]
    fn an_older_engine_is_told_apart() {
        fn old(_: &Value) -> Result<(u16, &'static str, String), String> {
            Ok((404, "", String::new()))
        }
        let (_, out, _) = bridge(
            "{\"jsonrpc\":\"2.0\",\"id\":8,\"method\":\"ping\"}\n",
            Fake::new(old),
        );
        let message = out.messages()[0]["error"]["message"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(message.contains("no MCP endpoint"), "{message}");
    }

    #[test]
    fn a_stream_becomes_one_line_per_message() {
        fn stream(_: &Value) -> Result<(u16, &'static str, String), String> {
            Ok((
                200,
                "text/event-stream",
                concat!(
                    "data: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/subscriptions/acknowledged\"}\n\n",
                    ":\n\n",
                    "data:{\"jsonrpc\":\"2.0\",\"method\":\"notifications/resources/updated\"}\n\n",
                    "data: {\"jsonrpc\":\"2.0\",\"id\":9,\"result\":{}}",
                )
                .to_string(),
            ))
        }
        let input = concat!(
            r#"{"jsonrpc":"2.0","id":9,"method":"subscriptions/listen","params":{}}"#,
            "\n",
        );
        let out = Sink::default();
        let transport = Fake::new(stream);
        let code = run_bridge(
            Cursor::new(input.as_bytes().to_vec()),
            out.clone(),
            Sink::default(),
            transport.clone(),
        );
        assert_eq!(code, 0);
        // A stream is not waited for on exit, so here it may still be running.
        let deadline = Instant::now() + Duration::from_secs(10);
        while out.text().lines().count() < 3 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        let messages = out.messages();
        assert_eq!(messages.len(), 3, "{}", out.text());
        assert_eq!(
            messages[0]["method"],
            "notifications/subscriptions/acknowledged"
        );
        assert_eq!(messages[1]["method"], "notifications/resources/updated");
        assert_eq!(messages[2]["id"], 9);
    }

    #[test]
    fn a_modern_requests_headers_are_mirrored_from_its_body() {
        let message = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "files_list",
                "_meta": { jsonrpc::META_PROTOCOL_VERSION: jsonrpc::MODERN_VERSION },
            },
        });
        assert_eq!(
            mirror_headers(&message),
            [
                ("MCP-Protocol-Version", jsonrpc::MODERN_VERSION.to_string()),
                ("Mcp-Method", "tools/call".to_string()),
                ("Mcp-Name", "files_list".to_string()),
            ]
        );
        let message = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "resources/read",
            "params": {
                "uri": "ps5upload://ゲーム",
                "_meta": { jsonrpc::META_PROTOCOL_VERSION: jsonrpc::MODERN_VERSION },
            },
        });
        let headers = mirror_headers(&message);
        assert_eq!(headers[1], ("Mcp-Method", "resources/read".to_string()));
        assert!(headers[2].1.starts_with("=?base64?"), "{headers:?}");
        assert_eq!(
            jsonrpc::decode_header_name(&headers[2].1).as_deref(),
            Some("ps5upload://ゲーム")
        );
    }

    #[test]
    fn a_legacy_request_gets_no_headers() {
        let message = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {}});
        assert!(mirror_headers(&message).is_empty());
        assert!(mirror_headers(&json!({"jsonrpc": "2.0", "id": 1, "method": "ping"})).is_empty());
    }

    #[test]
    fn the_transport_sees_the_mirrored_headers() {
        let transport = Fake::new(engine);
        let line = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "server/discover",
            "params": {"_meta": {jsonrpc::META_PROTOCOL_VERSION: jsonrpc::MODERN_VERSION}},
        });
        let (code, _, _) = bridge(&format!("{line}\n"), transport.clone());
        assert_eq!(code, 0);
        let seen = transport.seen.lock().unwrap();
        assert_eq!(seen[0].1[1], ("Mcp-Method", "server/discover".to_string()));
    }

    #[test]
    fn a_batch_answers_every_request_when_there_is_no_engine() {
        let input = concat!(
            r#"[{"jsonrpc":"2.0","id":1,"method":"ping"},{"jsonrpc":"2.0","method":"x"},{"jsonrpc":"2.0","id":2,"method":"ping"}]"#,
            "\n",
        );
        let (_, out, _) = bridge(input, Fake::new(unreachable_engine));
        let messages = out.messages();
        assert_eq!(messages.len(), 2, "{}", out.text());
        assert!(messages
            .iter()
            .all(|m| m["error"]["code"] == INTERNAL_ERROR));
    }

    #[test]
    fn a_slow_request_does_not_hold_up_a_quick_one() {
        static SLOW_DONE: AtomicUsize = AtomicUsize::new(0);
        fn answer(message: &Value) -> Result<(u16, &'static str, String), String> {
            if message["method"] == "slow" {
                // Answers only once the quick request has been answered.
                let deadline = Instant::now() + Duration::from_secs(20);
                while SLOW_DONE.load(Ordering::SeqCst) == 0 && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(5));
                }
            } else {
                SLOW_DONE.store(1, Ordering::SeqCst);
            }
            engine(message)
        }
        let input = concat!(
            r#"{"jsonrpc":"2.0","id":1,"method":"slow"}"#,
            "\n",
            r#"{"jsonrpc":"2.0","id":2,"method":"quick"}"#,
            "\n",
        );
        let started = Instant::now();
        let (code, out, _) = bridge(input, Fake::new(answer));
        assert_eq!(code, 0);
        assert_eq!(out.messages().len(), 2, "{}", out.text());
        assert!(started.elapsed() < Duration::from_secs(15));
    }

    #[test]
    fn at_stdin_end_it_waits_only_so_long() {
        let flight = InFlight::default();
        flight.enter();
        let started = Instant::now();
        flight.drain(Duration::from_millis(20));
        assert!(started.elapsed() >= Duration::from_millis(20));
        flight.leave();
        flight.drain(Duration::from_secs(60));
    }

    fn url_of(args: &[&str], env: &[(&str, &str)]) -> Result<Option<String>, String> {
        let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
        engine_url(&args, |name| {
            env.iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| value.to_string())
        })
    }

    #[test]
    fn the_engine_url_comes_from_the_flag_then_the_environment() {
        let url = |args: &[&str], env: &[(&str, &str)]| url_of(args, env).unwrap().unwrap();
        assert_eq!(url(&[], &[]), "http://127.0.0.1:19113");
        assert_eq!(
            url(&[], &[("PS5UPLOAD_ENGINE_PORT", "20000")]),
            "http://127.0.0.1:20000"
        );
        assert_eq!(
            url(
                &[],
                &[
                    ("PS5UPLOAD_ENGINE_PORT", "20000"),
                    ("PS5UPLOAD_BASE_URL", "/ps5/")
                ]
            ),
            "http://127.0.0.1:20000/ps5"
        );
        assert_eq!(
            url(
                &[],
                &[
                    ("PS5UPLOAD_ENGINE_URL", "http://nas:19113/"),
                    ("PS5UPLOAD_ENGINE_PORT", "20000")
                ]
            ),
            "http://nas:19113"
        );
        assert_eq!(
            url(
                &["--url", "http://a:1"],
                &[("PS5UPLOAD_ENGINE_URL", "http://b:2")]
            ),
            "http://a:1"
        );
        assert_eq!(url(&["--url=http://a:1/"], &[]), "http://a:1");
        // Pasting the endpoint itself works too.
        assert_eq!(url(&["--url", "http://a:1/mcp"], &[]), "http://a:1");
        // A blank variable counts as unset.
        assert_eq!(
            url(&[], &[("PS5UPLOAD_ENGINE_URL", "  ")]),
            "http://127.0.0.1:19113"
        );
    }

    #[test]
    fn a_bad_command_line_says_so() {
        assert!(url_of(&["--url"], &[]).is_err());
        assert!(url_of(&["--port", "1"], &[]).is_err());
        assert!(url_of(&["--url", "127.0.0.1:19113"], &[]).is_err());
        assert_eq!(url_of(&["--help"], &[]), Ok(None));
    }
}
