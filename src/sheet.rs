//! fork: the question sheet, a local web page for one pane's asks.
//!
//! The board has room for one line per ask, which is rarely enough to answer
//! from. Each pane with asks therefore gets a page at
//! `http://127.0.0.1:<port>/<secret>/<pane id>` that shows every ask with the
//! detail and options its agent wrote (`crate::board`), keeps the user's
//! drafts, and sends the answers to the pane as the agent's next prompt. The
//! board links to it (`PaneBoard::sheet_url`). Images an ask's detail embeds
//! from local files (screenshots, mockups) are served too, but only the files
//! that pane's asks name.
//!
//! The page is served by the Herdr server but is only an API client: it reads
//! the board with `pane.get` and delivers answers with `agent.prompt`.
//!
//! Anything that can reach the port could otherwise type into an agent, so
//! the listener binds loopback only, every path starts with a secret kept in
//! a file only the user can read, the `Host` header must name the loopback
//! address (DNS rebinding), and answers must be same-origin JSON posts. The
//! port and secret are kept across restarts and live handoff so open pages
//! and links on boards keep working.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tracing::{debug, info, warn};

use crate::api::client::ApiClient;
use crate::api::schema::{AgentPromptParams, Method, PaneSendInputParams, PaneTarget, Request};

const PAGE: &str = include_str!("sheet/page.html");
const STATE_FILE: &str = "sheet.json";
const MAX_HEADER_BYTES: u64 = 16 * 1024;
const MAX_BODY_BYTES: usize = 256 * 1024;
const MAX_ANSWER_CHARS: usize = 8000;
const MAX_IMAGE_BYTES: u64 = 20 * 1024 * 1024;
const IO_TIMEOUT: Duration = Duration::from_secs(10);
/// How long the previous server may keep the port during a live handoff.
const REBIND_ATTEMPTS: u32 = 60;
const REBIND_INTERVAL: Duration = Duration::from_millis(250);

/// Where the sheets are served. `None` until `start` runs, and in tests.
static ENDPOINT: Mutex<Option<Endpoint>> = Mutex::new(None);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Endpoint {
    port: u16,
    secret: String,
}

impl Endpoint {
    fn url_for(&self, pane_id: &str) -> Option<String> {
        valid_pane_id(pane_id)
            .then(|| format!("http://127.0.0.1:{}/{}/{pane_id}", self.port, self.secret))
    }

    fn serves_host(&self, host: &str) -> bool {
        host == format!("127.0.0.1:{}", self.port) || host == format!("localhost:{}", self.port)
    }
}

/// The question sheet of a pane, if sheets are being served.
pub(crate) fn url_for(pane_id: &str) -> Option<String> {
    endpoint()?.url_for(pane_id)
}

fn endpoint() -> Option<Endpoint> {
    ENDPOINT
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

fn set_endpoint(endpoint: &Endpoint) {
    *ENDPOINT
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(endpoint.clone());
}

fn valid_pane_id(pane_id: &str) -> bool {
    !pane_id.is_empty()
        && pane_id.len() <= 64
        && pane_id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, ':' | '_' | '-'))
}

/// Starts serving sheets for this server. Failing to is not fatal: boards
/// then simply have no sheet link.
pub(crate) fn start() {
    let path = crate::session::data_dir().join(STATE_FILE);
    let saved = load(&path);
    let mut endpoint = saved.clone().unwrap_or_else(|| Endpoint {
        port: 0,
        secret: new_secret(),
    });
    let mut listener = None;
    if endpoint.port == 0 {
        match bind(0) {
            Ok((bound, port)) => {
                endpoint.port = port;
                listener = Some(bound);
            }
            Err(err) => {
                warn!(err = %err, "question sheets are unavailable: cannot listen on loopback");
                return;
            }
        }
    } else if let Ok((bound, _)) = bind(endpoint.port) {
        listener = Some(bound);
    }
    if saved.as_ref() != Some(&endpoint) {
        save(&path, &endpoint);
    }
    // Known before the port is ours again, so boards restored by a handoff
    // link to the sheet from their first snapshot.
    set_endpoint(&endpoint);
    let spawned = crate::thread_spawn::spawn_named("herdr-sheet", move || {
        let listener = match listener {
            Some(listener) => listener,
            None => match rebind(&mut endpoint, &path) {
                Some(listener) => listener,
                None => return,
            },
        };
        info!(port = endpoint.port, "question sheets listening");
        serve(&listener);
    });
    if let Err(err) = spawned {
        warn!(err = %err, "question sheets are unavailable: cannot start their thread");
    }
}

fn bind(port: u16) -> std::io::Result<(TcpListener, u16)> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port))?;
    let port = listener.local_addr()?.port();
    Ok((listener, port))
}

/// Waits for the previous server to let go of the saved port, and moves to a
/// new one if it never does.
fn rebind(endpoint: &mut Endpoint, path: &Path) -> Option<TcpListener> {
    for _ in 0..REBIND_ATTEMPTS {
        std::thread::sleep(REBIND_INTERVAL);
        if let Ok((listener, _)) = bind(endpoint.port) {
            return Some(listener);
        }
    }
    match bind(0) {
        Ok((listener, port)) => {
            warn!(
                old = endpoint.port,
                new = port,
                "question sheet port was taken; links on open pages change"
            );
            endpoint.port = port;
            save(path, endpoint);
            set_endpoint(endpoint);
            Some(listener)
        }
        Err(err) => {
            warn!(err = %err, "question sheets are unavailable: cannot listen on loopback");
            None
        }
    }
}

fn load(path: &Path) -> Option<Endpoint> {
    let endpoint = serde_json::from_slice::<Endpoint>(&std::fs::read(path).ok()?).ok()?;
    let secret_ok =
        endpoint.secret.len() >= 32 && endpoint.secret.chars().all(|ch| ch.is_ascii_hexdigit());
    (secret_ok && endpoint.port != 0).then_some(endpoint)
}

fn save(path: &Path, endpoint: &Endpoint) {
    let result = (|| {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let temporary: PathBuf = path.with_extension("json.tmp");
        let mut file = create_private(&temporary)?;
        file.write_all(&serde_json::to_vec(endpoint).map_err(std::io::Error::other)?)?;
        file.sync_all()?;
        std::fs::rename(&temporary, path)
    })();
    if let Err(err) = result {
        warn!(path = %path.display(), err = %err, "could not save the question sheet endpoint");
    }
}

/// The secret is what keeps other local users away from the panes.
#[cfg(unix)]
fn create_private(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    let _ = std::fs::remove_file(path);
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}

#[cfg(not(unix))]
fn create_private(path: &Path) -> std::io::Result<std::fs::File> {
    // The data directory is inside the user's profile.
    std::fs::File::create(path)
}

/// 128 bits nobody else on the machine can guess. `RandomState` is keyed from
/// the operating system's random source.
fn new_secret() -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut hasher = Sha256::new();
    for _ in 0..4 {
        let random = std::collections::hash_map::RandomState::new()
            .build_hasher()
            .finish();
        hasher.update(random.to_le_bytes());
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    hasher.update(now.as_nanos().to_le_bytes());
    hasher.update(std::process::id().to_le_bytes());
    hasher.finalize()[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn serve(listener: &TcpListener) {
    for stream in listener.incoming() {
        let Ok(stream) = stream else {
            // Out of descriptors or similar; do not spin.
            std::thread::sleep(Duration::from_millis(200));
            continue;
        };
        let spawned = crate::thread_spawn::spawn_named("herdr-sheet-conn", move || {
            if let Err(err) = handle_connection(stream) {
                debug!(err = %err, "question sheet connection failed");
            }
        });
        if let Err(err) = spawned {
            warn!(err = %err, "dropped a question sheet connection: cannot start a thread");
        }
    }
}

fn handle_connection(mut stream: TcpStream) -> std::io::Result<()> {
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    let response = match (read_request(&stream), endpoint()) {
        (Ok(request), Some(endpoint)) => respond(&request, &endpoint, &mut HerdrApi),
        (Err(err), _) if err.kind() == std::io::ErrorKind::InvalidData => {
            HttpResponse::error(400, "bad request")
        }
        (Err(err), _) => return Err(err),
        (_, None) => HttpResponse::error(503, "not ready"),
    };
    response.write_to(&mut stream)
}

#[derive(Debug, Default)]
struct HttpRequest {
    method: String,
    path: String,
    query: String,
    host: String,
    origin: Option<String>,
    content_type: String,
    body: Vec<u8>,
}

fn read_request(stream: impl Read) -> std::io::Result<HttpRequest> {
    let invalid = |message: &str| std::io::Error::new(std::io::ErrorKind::InvalidData, message);
    let mut head = BufReader::new(stream.take(MAX_HEADER_BYTES));
    let mut line = String::new();
    head.read_line(&mut line)?;
    let mut parts = line.split_whitespace();
    let (Some(method), Some(target)) = (parts.next(), parts.next()) else {
        return Err(invalid("request line"));
    };
    let mut request = HttpRequest {
        method: method.to_string(),
        path: target.split(['?', '#']).next().unwrap_or("").to_string(),
        query: target
            .split('#')
            .next()
            .and_then(|target| target.split_once('?'))
            .map(|(_, query)| query.to_string())
            .unwrap_or_default(),
        ..HttpRequest::default()
    };
    let mut content_length = 0usize;
    loop {
        line.clear();
        if head.read_line(&mut line)? == 0 {
            return Err(invalid("headers too long or cut short"));
        }
        let header = line.trim_end();
        if header.is_empty() {
            break;
        }
        let Some((name, value)) = header.split_once(':') else {
            return Err(invalid("header"));
        };
        let value = value.trim();
        match name.to_ascii_lowercase().as_str() {
            "host" => request.host = value.to_string(),
            "origin" => request.origin = Some(value.to_string()),
            "content-type" => request.content_type = value.to_ascii_lowercase(),
            "content-length" => {
                content_length = value.parse().map_err(|_| invalid("content-length"))?;
            }
            "transfer-encoding" => return Err(invalid("chunked bodies are not accepted")),
            _ => {}
        }
    }
    if content_length > MAX_BODY_BYTES {
        return Err(invalid("body too large"));
    }
    // The body may run past the header limit; lift it for exactly its length.
    let mut body = head.buffer().to_vec();
    let mut rest = head.into_inner();
    rest.set_limit(content_length.saturating_sub(body.len()) as u64);
    rest.read_to_end(&mut body)?;
    if body.len() < content_length {
        return Err(invalid("body cut short"));
    }
    body.truncate(content_length);
    request.body = body;
    Ok(request)
}

struct HttpResponse {
    status: u16,
    content_type: &'static str,
    body: Vec<u8>,
}

impl HttpResponse {
    fn json(status: u16, value: &Value) -> Self {
        Self {
            status,
            content_type: "application/json; charset=utf-8",
            body: value.to_string().into_bytes(),
        }
    }

    fn error(status: u16, message: &str) -> Self {
        Self::json(status, &json!({ "error": message }))
    }

    fn write_to(&self, stream: &mut impl Write) -> std::io::Result<()> {
        let reason = match self.status {
            200 => "OK",
            400 => "Bad Request",
            403 => "Forbidden",
            404 => "Not Found",
            405 => "Method Not Allowed",
            409 => "Conflict",
            415 => "Unsupported Media Type",
            503 => "Service Unavailable",
            _ => "Error",
        };
        // The URL carries the secret, so it must not leak through Referer,
        // caches, or frames, and the page loads nothing from elsewhere.
        write!(
            stream,
            "HTTP/1.1 {} {reason}\r\n\
             Content-Type: {}\r\n\
             Content-Length: {}\r\n\
             Cache-Control: no-store\r\n\
             Referrer-Policy: no-referrer\r\n\
             X-Content-Type-Options: nosniff\r\n\
             Content-Security-Policy: default-src 'none'; script-src 'unsafe-inline'; \
             style-src 'unsafe-inline'; img-src 'self' https: data:; connect-src 'self'; \
             frame-ancestors 'none'; \
             base-uri 'none'; form-action 'none'\r\n\
             Connection: close\r\n\r\n",
            self.status,
            self.content_type,
            self.body.len()
        )?;
        stream.write_all(&self.body)?;
        stream.flush()
    }
}

/// What the sheet needs from Herdr, behind a trait for tests.
trait SheetApi {
    /// The pane as `pane.get` reports it, or `None` when it is gone.
    fn pane(&mut self, pane_id: &str) -> Result<Option<Value>, String>;
    /// Sends `text` to the pane's agent as its next prompt.
    fn deliver(&mut self, pane_id: &str, text: &str) -> Result<(), String>;
}

fn respond(request: &HttpRequest, endpoint: &Endpoint, api: &mut dyn SheetApi) -> HttpResponse {
    if !endpoint.serves_host(&request.host) {
        return HttpResponse::error(403, "forbidden");
    }
    let mut segments = request.path.trim_matches('/').split('/');
    let (Some(secret), Some(pane_id), action, None) = (
        segments.next(),
        segments.next(),
        segments.next(),
        segments.next(),
    ) else {
        return HttpResponse::error(404, "not found");
    };
    if secret != endpoint.secret || !valid_pane_id(pane_id) {
        return HttpResponse::error(404, "not found");
    }
    match (request.method.as_str(), action) {
        ("GET", None) => HttpResponse {
            status: 200,
            content_type: "text/html; charset=utf-8",
            body: PAGE.as_bytes().to_vec(),
        },
        ("GET", Some("data")) => match api.pane(pane_id) {
            Ok(Some(pane)) => HttpResponse::json(200, &sheet_data(&pane)),
            Ok(None) => HttpResponse::error(404, "pane not found"),
            Err(err) => HttpResponse::error(503, &err),
        },
        ("GET", Some("file")) => image(request, pane_id, api),
        ("POST", Some("answer")) => answer(request, endpoint, pane_id, api),
        (_, None | Some("data" | "answer" | "file")) => {
            HttpResponse::error(405, "method not allowed")
        }
        _ => HttpResponse::error(404, "not found"),
    }
}

/// The part of a pane the page shows.
fn sheet_data(pane: &Value) -> Value {
    let field = |name: &str| pane.get(name).cloned().unwrap_or(Value::Null);
    json!({
        "pane": {
            "pane_id": field("pane_id"),
            "label": field("label"),
            "title": field("title"),
            "agent": pane.get("display_agent").or_else(|| pane.get("agent")).cloned().unwrap_or(Value::Null),
            "cwd": field("cwd"),
        },
        "board": pane.get("board").cloned().unwrap_or_else(|| json!({})),
    })
}

/// A local image one of the pane's asks embeds. Nothing else on disk is
/// served, whatever path is asked for.
fn image(request: &HttpRequest, pane_id: &str, api: &mut dyn SheetApi) -> HttpResponse {
    let Some(path) = request
        .query
        .split('&')
        .find_map(|pair| pair.strip_prefix("path="))
        .map(percent_decode)
    else {
        return HttpResponse::error(400, "path is required");
    };
    let Some(content_type) = crate::board::image_content_type(&path) else {
        return HttpResponse::error(404, "not an image");
    };
    let pane = match api.pane(pane_id) {
        Ok(Some(pane)) => pane,
        Ok(None) => return HttpResponse::error(404, "pane not found"),
        Err(err) => return HttpResponse::error(503, &err),
    };
    let named = pane
        .pointer("/board/asks")
        .and_then(Value::as_array)
        .is_some_and(|asks| {
            asks.iter()
                .filter_map(|ask| ask.get("detail").and_then(Value::as_str))
                .any(|detail| crate::board::image_targets(detail).contains(&path.as_str()))
        });
    if !named {
        return HttpResponse::error(404, "not an image of this sheet");
    }
    let too_large = std::fs::metadata(&path).is_ok_and(|file| file.len() > MAX_IMAGE_BYTES);
    match std::fs::read(&path) {
        Ok(body) if !too_large => HttpResponse {
            status: 200,
            content_type,
            body,
        },
        _ => HttpResponse::error(404, "image not found"),
    }
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        let hex = (bytes[at] == b'%')
            .then(|| text.get(at + 1..at + 3))
            .flatten()
            .and_then(|hex| u8::from_str_radix(hex, 16).ok());
        match hex {
            Some(byte) => {
                decoded.push(byte);
                at += 3;
            }
            None => {
                decoded.push(bytes[at]);
                at += 1;
            }
        }
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

#[derive(Debug, Deserialize)]
struct AnswerBody {
    #[serde(default)]
    answers: Vec<Answer>,
    #[serde(default)]
    note: String,
}

#[derive(Debug, Deserialize)]
struct Answer {
    question: String,
    answer: String,
}

fn answer(
    request: &HttpRequest,
    endpoint: &Endpoint,
    pane_id: &str,
    api: &mut dyn SheetApi,
) -> HttpResponse {
    // A JSON content type cannot be sent cross-origin without a preflight,
    // which this server never grants.
    if !request.content_type.starts_with("application/json") {
        return HttpResponse::error(415, "answers must be posted as application/json");
    }
    let same_origin = request.origin.as_deref().is_none_or(|origin| {
        origin
            .strip_prefix("http://")
            .is_some_and(|host| endpoint.serves_host(host))
    });
    if !same_origin {
        return HttpResponse::error(403, "forbidden");
    }
    let Ok(body) = serde_json::from_slice::<AnswerBody>(&request.body) else {
        return HttpResponse::error(400, "回答の形式が正しくありません。");
    };
    let Some(message) = answer_message(&body) else {
        return HttpResponse::error(400, "回答が空です。");
    };
    match api.deliver(pane_id, &message) {
        Ok(()) => HttpResponse::json(200, &json!({ "ok": true })),
        Err(err) => HttpResponse::error(409, &err),
    }
}

/// The prompt the agent receives. Each answer repeats its question, so it
/// still reads correctly if the board changed while the user was writing.
fn answer_message(body: &AnswerBody) -> Option<String> {
    let clip = |text: &str| {
        text.trim()
            .chars()
            .filter(|ch| !ch.is_control() || matches!(ch, '\n' | '\t'))
            .take(MAX_ANSWER_CHARS)
            .collect::<String>()
    };
    let mut parts = body
        .answers
        .iter()
        .map(|answer| (clip(&answer.question), clip(&answer.answer)))
        .filter(|(_, answer)| !answer.is_empty())
        .map(|(question, answer)| format!("■ {question}\n{answer}"))
        .collect::<Vec<_>>();
    let note = clip(&body.note);
    if !note.is_empty() {
        parts.push(format!("補足:\n{note}"));
    }
    (!parts.is_empty()).then(|| format!("質問シートへの回答です。\n\n{}", parts.join("\n\n")))
}

struct HerdrApi;

impl HerdrApi {
    fn request(method: Method) -> Result<Value, String> {
        ApiClient::local()
            .request_value(&Request {
                id: "sheet".into(),
                method,
            })
            .map_err(|err| format!("herdr に接続できません: {err}"))
    }
}

fn error_code(response: &Value) -> Option<&str> {
    response.pointer("/error/code").and_then(Value::as_str)
}

impl SheetApi for HerdrApi {
    fn pane(&mut self, pane_id: &str) -> Result<Option<Value>, String> {
        let response = Self::request(Method::PaneGet(PaneTarget {
            pane_id: pane_id.to_string(),
        }))?;
        Ok(response.pointer("/result/pane").cloned())
    }

    fn deliver(&mut self, pane_id: &str, text: &str) -> Result<(), String> {
        let response = Self::request(Method::AgentPrompt(AgentPromptParams {
            target: pane_id.to_string(),
            text: text.to_string(),
            wait: None,
        }))?;
        let response = match error_code(&response) {
            // Herdr does not see an agent it can prompt there (for example a
            // session it cannot identify), so type the answer into the pane.
            Some("agent_not_found" | "agent_not_ready") => {
                Self::request(Method::PaneSendInput(PaneSendInputParams {
                    pane_id: pane_id.to_string(),
                    text: text.to_string(),
                    keys: vec!["Enter".into()],
                }))?
            }
            _ => response,
        };
        match error_code(&response) {
            None => Ok(()),
            Some("agent_blocked") => Err(
                "エージェントが許可待ちなどで入力を受け付けていません。ペインで対応してから送り直してください。"
                    .into(),
            ),
            Some("pane_not_found") => Err("このペインはもうありません。".into()),
            Some(code) => Err(format!("エージェントに送れませんでした（{code}）。")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct FakeApi {
        pane: Option<Value>,
        delivered: Vec<(String, String)>,
        refuse: Option<String>,
    }

    impl SheetApi for FakeApi {
        fn pane(&mut self, _: &str) -> Result<Option<Value>, String> {
            Ok(self.pane.clone())
        }

        fn deliver(&mut self, pane_id: &str, text: &str) -> Result<(), String> {
            if let Some(reason) = &self.refuse {
                return Err(reason.clone());
            }
            self.delivered.push((pane_id.into(), text.into()));
            Ok(())
        }
    }

    fn endpoint() -> Endpoint {
        Endpoint {
            port: 4100,
            secret: "0123456789abcdef0123456789abcdef".into(),
        }
    }

    fn get(path: &str) -> HttpRequest {
        HttpRequest {
            method: "GET".into(),
            path: path.into(),
            host: "127.0.0.1:4100".into(),
            ..HttpRequest::default()
        }
    }

    fn post(path: &str, body: Value) -> HttpRequest {
        HttpRequest {
            method: "POST".into(),
            content_type: "application/json".into(),
            origin: Some("http://127.0.0.1:4100".into()),
            body: body.to_string().into_bytes(),
            ..get(path)
        }
    }

    fn body(response: &HttpResponse) -> Value {
        serde_json::from_slice(&response.body).unwrap()
    }

    const SHEET: &str = "/0123456789abcdef0123456789abcdef/w1:p2";

    #[test]
    fn sheet_urls_name_the_pane_and_only_for_plain_pane_ids() {
        assert_eq!(
            endpoint().url_for("w1:p2").as_deref(),
            Some("http://127.0.0.1:4100/0123456789abcdef0123456789abcdef/w1:p2")
        );
        for pane_id in ["", "w1/p2", "w1 p2", "w1?p2", "../x"] {
            assert_eq!(endpoint().url_for(pane_id), None, "{pane_id}");
        }
    }

    #[test]
    fn the_page_and_its_data_need_the_secret_and_a_loopback_host() {
        let mut api = FakeApi {
            pane: Some(json!({
                "pane_id": "w1:p2", "agent": "claude", "cwd": "/repo", "terminal_id": "t1",
                "board": {"asks": [{"kind": "decide", "text": "Q1", "detail": "## 背景"}]},
            })),
            ..FakeApi::default()
        };
        let page = respond(&get(SHEET), &endpoint(), &mut api);
        assert_eq!(
            (page.status, page.content_type),
            (200, "text/html; charset=utf-8")
        );

        let data = respond(&get(&format!("{SHEET}/data")), &endpoint(), &mut api);
        assert_eq!(data.status, 200);
        let data = body(&data);
        assert_eq!(data["board"]["asks"][0]["detail"], "## 背景");
        assert_eq!(data["pane"]["agent"], "claude");
        assert!(data["pane"].get("terminal_id").is_none());

        let wrong_secret = "/ffffffffffffffffffffffffffffffff/w1:p2/data";
        assert_eq!(
            respond(&get(wrong_secret), &endpoint(), &mut api).status,
            404
        );
        assert_eq!(respond(&get("/"), &endpoint(), &mut api).status, 404);
        let rebound = HttpRequest {
            host: "evil.example:4100".into(),
            ..get(SHEET)
        };
        assert_eq!(respond(&rebound, &endpoint(), &mut api).status, 403);

        api.pane = None;
        let gone = respond(&get(&format!("{SHEET}/data")), &endpoint(), &mut api);
        assert_eq!(gone.status, 404);
    }

    #[test]
    fn only_images_the_panes_asks_embed_are_served() {
        let dir = std::env::temp_dir().join(format!("herdr-sheet-img-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let shown = dir.join("案 A.png");
        let other = dir.join("other.png");
        std::fs::write(&shown, b"png-bytes").unwrap();
        std::fs::write(&other, b"secret").unwrap();
        // The agent's path has no spaces; the query still arrives encoded.
        let shown_plain = dir.join("a.png");
        std::fs::rename(&shown, &shown_plain).unwrap();
        let mut api = FakeApi {
            pane: Some(json!({"board": {"asks": [
                {"text": "Q1", "detail": format!("![案A]({})", shown_plain.display())},
            ]}})),
            ..FakeApi::default()
        };
        let file = |path: &std::path::Path| HttpRequest {
            query: format!("path={}", path.display().to_string().replace('/', "%2F")),
            ..get(&format!("{SHEET}/file"))
        };
        let served = respond(&file(&shown_plain), &endpoint(), &mut api);
        assert_eq!((served.status, served.content_type), (200, "image/png"));
        assert_eq!(served.body, b"png-bytes");
        assert_eq!(respond(&file(&other), &endpoint(), &mut api).status, 404);
        let passwd = file(std::path::Path::new("/etc/passwd"));
        assert_eq!(respond(&passwd, &endpoint(), &mut api).status, 404);
        let missing = get(&format!("{SHEET}/file"));
        assert_eq!(respond(&missing, &endpoint(), &mut api).status, 400);
        assert_eq!(percent_decode("%E6%A1%88%20a%2Fb%zz"), "案 a/b%zz");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn answers_reach_the_pane_as_one_prompt_that_repeats_each_question() {
        let mut api = FakeApi::default();
        let response = respond(
            &post(
                &format!("{SHEET}/answer"),
                json!({"answers": [
                    {"question": "Q1 削除してよいか", "answer": "はい\n ただし backup は残す "},
                    {"question": "Q2 名前", "answer": "  "},
                ], "note": "急ぎません"}),
            ),
            &endpoint(),
            &mut api,
        );
        assert_eq!(response.status, 200, "{}", body(&response));
        assert_eq!(
            api.delivered,
            [(
                "w1:p2".to_string(),
                "質問シートへの回答です。\n\n■ Q1 削除してよいか\nはい\n ただし backup は残す\n\n補足:\n急ぎません"
                    .to_string()
            )]
        );
    }

    #[test]
    fn answers_from_other_origins_or_without_content_are_refused() {
        let mut api = FakeApi::default();
        let path = format!("{SHEET}/answer");
        let answers = json!({"answers": [{"question": "Q1", "answer": "はい"}]});

        let form = HttpRequest {
            content_type: "text/plain".into(),
            ..post(&path, answers.clone())
        };
        assert_eq!(respond(&form, &endpoint(), &mut api).status, 415);
        let foreign = HttpRequest {
            origin: Some("http://evil.example".into()),
            ..post(&path, answers.clone())
        };
        assert_eq!(respond(&foreign, &endpoint(), &mut api).status, 403);
        let empty = post(
            &path,
            json!({"answers": [{"question": "Q1", "answer": ""}]}),
        );
        assert_eq!(respond(&empty, &endpoint(), &mut api).status, 400);
        assert_eq!(respond(&get(&path), &endpoint(), &mut api).status, 405);
        assert!(api.delivered.is_empty());

        api.refuse = Some("エージェントが許可待ち".into());
        let refused = respond(&post(&path, answers), &endpoint(), &mut api);
        assert_eq!(refused.status, 409);
        assert_eq!(body(&refused)["error"], "エージェントが許可待ち");
    }

    #[test]
    fn requests_are_read_with_their_body_and_oversized_ones_are_rejected() {
        let raw = "POST /s/w1:p2/answer?x=1 HTTP/1.1\r\nHost: 127.0.0.1:4100\r\n\
                   Origin: http://127.0.0.1:4100\r\nContent-Type: Application/JSON\r\n\
                   Content-Length: 7\r\n\r\n{\"a\":1}trailing";
        let request = read_request(raw.as_bytes()).unwrap();
        assert_eq!(request.method, "POST");
        assert_eq!(request.path, "/s/w1:p2/answer");
        assert_eq!(request.query, "x=1");
        assert_eq!(request.host, "127.0.0.1:4100");
        assert_eq!(request.content_type, "application/json");
        assert_eq!(request.body, b"{\"a\":1}");

        // A body longer than the header limit still arrives whole.
        let long = "x".repeat(MAX_HEADER_BYTES as usize * 2);
        let raw = format!(
            "POST /a HTTP/1.1\r\nContent-Length: {}\r\n\r\n{long}",
            long.len()
        );
        assert_eq!(read_request(raw.as_bytes()).unwrap().body.len(), long.len());

        for raw in [
            format!(
                "POST /a HTTP/1.1\r\nContent-Length: {}\r\n\r\n",
                MAX_BODY_BYTES + 1
            ),
            "POST /a HTTP/1.1\r\nContent-Length: 9\r\n\r\nshort".to_string(),
            format!(
                "GET /a HTTP/1.1\r\nX: {}\r\n\r\n",
                "y".repeat(MAX_HEADER_BYTES as usize)
            ),
            "\r\n".to_string(),
        ] {
            assert!(read_request(raw.as_bytes()).is_err());
        }
    }

    #[test]
    fn the_endpoint_file_round_trips_and_bad_files_are_replaced() {
        let dir = std::env::temp_dir().join(format!("herdr-sheet-test-{}", std::process::id()));
        let path = dir.join(STATE_FILE);
        let saved = Endpoint {
            port: 4100,
            secret: new_secret(),
        };
        assert_eq!(saved.secret.len(), 32);
        assert_ne!(saved.secret, new_secret());
        save(&path, &saved);
        assert_eq!(load(&path), Some(saved));
        std::fs::write(&path, r#"{"port":4100,"secret":"short"}"#).unwrap();
        assert_eq!(load(&path), None);
        let _ = std::fs::remove_dir_all(dir);
    }
}
