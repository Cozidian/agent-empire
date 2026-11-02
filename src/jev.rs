//! Jev, TypeSafe AI's typed-decision model, as an optional advisor.
//!
//! Everything here is opt-in and fails closed: with no `TYPESAFE_API_KEY`, a feature flag that
//! is off, any error, an unknown answer type or a missing field, callers get `None`/`Err` and the
//! game behaves exactly as it does without Jev. Jev only ever suggests.
//!
//! There is no HTTP crate. Requests go through `curl -K -`, with the whole config (URL, key,
//! body) on stdin, so the key never appears in argv. They run on a background thread.
//!
//! Env: `TYPESAFE_API_KEY` (required), `TYPESAFE_DEFAULT_MODEL` (default `jev-1.14.0`),
//! `TYPESAFE_BASE_URL` (default `https://api.typesafe.ai`), `EMPIRE_JEV_LOG=1` (record what Jev
//! suggested next to what you chose, labels only, in the state dir).
#![allow(dead_code)]

use serde_json::{Value, json};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::{Mutex, mpsc};
use std::time::Duration;

pub const DEFAULT_MODEL: &str = "jev-1.14.0";
pub const DEFAULT_BASE: &str = "https://api.typesafe.ai";
const PATH: &str = "/v1/systemone";
const TIMEOUT_SECS: &str = "10";
/// A `retry-after` longer than this is not waited for.
const MAX_RETRY_WAIT: u64 = 10;
/// Below this confidence Jev's answer is ignored.
pub const SUGGEST_MIN: f32 = 0.5;
/// From here on the answer is a strong suggestion (still never acted on by itself).
pub const CONFIDENT_MIN: f32 = 0.85;
/// Text sent as state is cut to this many characters.
pub const MAX_STATE_CHARS: usize = 4000;

// ── switches

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

/// A key that can be put in a curl config line without breaking out of it.
fn usable_key(key: &str) -> bool {
    !key.is_empty() && key.chars().all(|c| c.is_ascii_graphic() && c != '"' && c != '\\')
}

pub fn enabled_with(key: Option<&str>, flag: Option<&str>) -> bool {
    key.is_some_and(usable_key) && flag.is_some_and(|f| matches!(f.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
}

/// Whether a feature may ask Jev: a key is set and its own flag (e.g. `EMPIRE_JEV_ROUTE`) is on.
pub fn enabled(flag: &str) -> bool {
    enabled_with(env("TYPESAFE_API_KEY").as_deref(), env(flag).as_deref())
}

pub fn model() -> String {
    env("TYPESAFE_DEFAULT_MODEL").unwrap_or_else(|| DEFAULT_MODEL.into())
}

fn endpoint(base: Option<&str>) -> Option<String> {
    let base = base.unwrap_or(DEFAULT_BASE).trim_end_matches('/');
    let ok = (base.starts_with("https://") || base.starts_with("http://")) && base.chars().all(|c| c.is_ascii_graphic() && c != '"' && c != '\\');
    ok.then(|| format!("{base}{PATH}"))
}

// ── gating

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Gate {
    /// Below 0.5 confidence (or not a number): do nothing.
    Ignore,
    /// 0.5 up to 0.85: suggest or preselect.
    Suggest,
    /// 0.85 and up: a strong suggestion. Still only a suggestion.
    Confident,
}

pub fn gate(confidence: f32) -> Gate {
    if confidence >= CONFIDENT_MIN {
        Gate::Confident
    } else if confidence >= SUGGEST_MIN {
        Gate::Suggest
    } else {
        Gate::Ignore
    }
}

// ── building requests

pub fn choice_q(instructions: &str, criteria: &[(&str, &str)]) -> Value {
    let map: serde_json::Map<String, Value> = criteria.iter().map(|(k, v)| (k.to_string(), json!(v))).collect();
    json!({ "type": "choice", "instructions": instructions, "criteria": map })
}

/// `levels` are ordered from the low end; level numbers are their positions, from 0.
pub fn score_q(instructions: &str, levels: &[&str]) -> Value {
    json!({ "type": "score", "instructions": instructions, "criteria": levels })
}

pub fn noul_q(instructions: &str) -> Value {
    json!({ "type": "noul", "instructions": instructions })
}

pub fn request(state: Value, questions: Vec<(&str, Value)>) -> Value {
    let questions: serde_json::Map<String, Value> = questions.into_iter().map(|(k, v)| (k.to_string(), v)).collect();
    json!({ "model": model(), "state": state, "questions": questions })
}

/// Text for the state, cut to a sane size.
pub fn clip(s: &str) -> String {
    s.chars().take(MAX_STATE_CHARS).collect()
}

// ── reading answers (every reader returns None for anything it doesn't fully understand)

#[derive(Debug, Clone, PartialEq)]
pub struct Choice {
    pub label: String,
    pub confidence: f32,
    /// Probability of the chosen label.
    pub probability: f32,
}

fn unit(v: &Value) -> Option<f32> {
    v.as_f64().filter(|n| (0.0..=1.0).contains(n)).map(|n| n as f32)
}

/// A choice answer whose label is one of `allowed`.
pub fn choice(v: &Value, name: &str, allowed: &[&str]) -> Option<Choice> {
    let a = &v["answers"][name];
    if a["type"] != "choice" {
        return None;
    }
    let label = a["choice"].as_str().filter(|l| allowed.contains(l))?;
    Some(Choice { label: label.into(), confidence: unit(&a["confidence"])?, probability: unit(&a["probabilities"][label])? })
}

#[derive(Debug, Clone, PartialEq)]
pub struct Score {
    /// Expected level; can fall between levels, from 0 to `levels - 1`.
    pub score: f32,
    pub confidence: f32,
}

pub fn score(v: &Value, name: &str, levels: usize) -> Option<Score> {
    let a = &v["answers"][name];
    if a["type"] != "score" || levels < 2 {
        return None;
    }
    let s = a["score"].as_f64().filter(|s| *s >= 0.0 && *s <= (levels - 1) as f64)?;
    Some(Score { score: s as f32, confidence: unit(&a["confidence"])? })
}

/// Probability of yes. A noul has no confidence field.
pub fn noul(v: &Value, name: &str) -> Option<f32> {
    let a = &v["answers"][name];
    if a["type"] != "noul" {
        return None;
    }
    unit(&a["noul"])
}

// ── errors

#[derive(Debug, Clone, PartialEq)]
pub enum Error {
    /// No key, or the feature is off.
    Disabled,
    Transport(String),
    /// 429 or 529. `retry_after` is in seconds, when the server said.
    Busy { retry_after: Option<u64> },
    /// 400, 401, 413, 415, 422 and anything else unexpected.
    Status(u16, String),
    /// A 200 that isn't what we asked about.
    Malformed,
}

impl Error {
    pub fn describe(&self) -> String {
        match self {
            Error::Disabled => "Jev is off".into(),
            Error::Transport(e) => format!("could not reach Jev: {e}"),
            Error::Busy { .. } => "Jev is busy".into(),
            Error::Status(400, _) => "Jev rejected the request (400)".into(),
            Error::Status(401, _) => "Jev rejected the key (401)".into(),
            Error::Status(413, _) => "request too large for Jev (413)".into(),
            Error::Status(415, _) => "Jev wants JSON (415)".into(),
            Error::Status(422, d) => format!("Jev could not use the questions (422) {d}").trim().to_string(),
            Error::Status(c, _) => format!("Jev answered {c}"),
            Error::Malformed => "unreadable answer from Jev".into(),
        }
    }
}

// ── transport

/// Status, `retry-after` seconds and body from `curl -i` output (skipping 1xx blocks).
fn parse_http(raw: &[u8]) -> Option<(u16, Option<u64>, Vec<u8>)> {
    let mut rest = raw;
    loop {
        if !rest.starts_with(b"HTTP/") {
            return None;
        }
        let end = rest.windows(4).position(|w| w == b"\r\n\r\n")?;
        let head = String::from_utf8_lossy(&rest[..end]).into_owned();
        let body = &rest[end + 4..];
        let status: u16 = head.lines().next()?.split_whitespace().nth(1)?.parse().ok()?;
        if (100..200).contains(&status) {
            rest = body;
            continue;
        }
        let retry = head.lines().skip(1).find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.trim().eq_ignore_ascii_case("retry-after").then(|| v.trim().parse::<u64>().ok()).flatten()
        });
        return Some((status, retry, body.to_vec()));
    }
}

/// The curl config that goes on stdin. Quoted values escape `\` and `"`; JSON has no raw newlines.
fn config(url: &str, key: &str, body: &str) -> String {
    let quote = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
    format!(
        "url = \"{}\"\nproto = \"=http,https\"\nrequest = \"POST\"\nheader = \"Authorization: Bearer {key}\"\nheader = \"Content-Type: application/json\"\nheader = \"Expect:\"\ndata-binary = \"{}\"\n",
        quote(url),
        quote(body)
    )
}

/// curl's arguments. Nothing secret or request-specific: that all goes on stdin.
const CURL_ARGS: [&str; 6] = ["-sS", "-i", "--max-time", TIMEOUT_SECS, "-K", "-"];

fn post_once(url: &str, key: &str, body: &str) -> Result<(u16, Option<u64>, Vec<u8>), Error> {
    let mut child = Command::new("curl")
        .args(CURL_ARGS)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| Error::Transport(format!("curl: {e}")))?;
    let mut stdin = child.stdin.take().ok_or_else(|| Error::Transport("curl: no stdin".into()))?;
    let written = stdin.write_all(config(url, key, body).as_bytes());
    drop(stdin);
    let out = child.wait_with_output().map_err(|e| Error::Transport(e.to_string()))?;
    if !out.status.success() {
        // curl's own message never contains the key (it is not in argv or the URL)
        let msg = String::from_utf8_lossy(&out.stderr).trim().chars().take(120).collect::<String>();
        return Err(Error::Transport(if msg.is_empty() { written.err().map_or("curl failed".into(), |e| e.to_string()) } else { msg }));
    }
    parse_http(&out.stdout).ok_or(Error::Malformed)
}

fn detail(body: &[u8]) -> String {
    serde_json::from_slice::<Value>(body).ok().map(|v| v["detail"].to_string()).filter(|d| d != "null").unwrap_or_default().chars().take(120).collect()
}

static CACHE: Mutex<Option<HashMap<u64, Value>>> = Mutex::new(None);
const CACHE_MAX: usize = 256;

pub fn digest(body: &Value) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    body.to_string().hash(&mut h);
    h.finish()
}

fn cached(hash: u64) -> Option<Value> {
    CACHE.lock().ok()?.as_ref()?.get(&hash).cloned()
}

fn remember(hash: u64, v: &Value) {
    if let Ok(mut c) = CACHE.lock() {
        let m = c.get_or_insert_with(HashMap::new);
        if m.len() >= CACHE_MAX {
            m.clear();
        }
        m.insert(hash, v.clone());
    }
}

/// Ask Jev (blocking: call it off the UI thread). Answers are cached by a hash of the whole
/// request, which is the model, the questions and the state. On 429/529 a `retry-after` of at
/// most ten seconds is waited out once.
pub fn post(request: &Value) -> Result<Value, Error> {
    let key = env("TYPESAFE_API_KEY").filter(|k| usable_key(k)).ok_or(Error::Disabled)?;
    let url = endpoint(env("TYPESAFE_BASE_URL").as_deref()).ok_or_else(|| Error::Transport("TYPESAFE_BASE_URL is not an http(s) URL".into()))?;
    post_with(&url, &key, request)
}

fn post_with(url: &str, key: &str, request: &Value) -> Result<Value, Error> {
    let hash = digest(request);
    if let Some(v) = cached(hash) {
        return Ok(v);
    }
    let body = request.to_string();
    let mut tries = 0;
    loop {
        let (status, retry_after, bytes) = post_once(url, key, &body)?;
        match status {
            200 => {
                let v: Value = serde_json::from_slice(&bytes).map_err(|_| Error::Malformed)?;
                remember(hash, &v);
                return Ok(v);
            }
            429 | 529 => {
                tries += 1;
                match retry_after {
                    Some(s) if s <= MAX_RETRY_WAIT && tries < 2 => std::thread::sleep(Duration::from_secs(s)),
                    _ => return Err(Error::Busy { retry_after }),
                }
            }
            other => return Err(Error::Status(other, detail(&bytes))),
        }
    }
}

/// Ask on a background thread and read the answer with `read`; the receiver gets the outcome.
pub fn spawn<T: Send + 'static>(request: Value, read: impl FnOnce(&Value) -> Option<T> + Send + 'static) -> mpsc::Receiver<Result<T, Error>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(post(&request).and_then(|v| read(&v).ok_or(Error::Malformed)));
    });
    rx
}

// ── what Jev suggested next to what you chose

/// Append a record to `jev.log` in the state dir (created 0600), only when `EMPIRE_JEV_LOG=1`.
/// Keep records to labels and numbers: never the key, never task text.
pub fn log(feature: &str, record: Value) {
    if env("EMPIRE_JEV_LOG").as_deref() != Some("1") {
        return;
    }
    let _ = append_log(&crate::daemon::dirs_state().join("jev.log"), feature, record);
}

fn append_log(path: &std::path::Path, feature: &str, record: Value) -> std::io::Result<()> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut f = std::fs::OpenOptions::new().create(true).append(true).mode(0o600).open(path)?;
    f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    let at = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    writeln!(f, "{}", json!({ "at": at, "feature": feature, "model": model(), "record": record }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn choice_answer(label: &str) -> Value {
        json!({ "model": "jev-1.14.0", "answers": { "pick": {
            "type": "choice", "choice": label, "confidence": 0.72,
            "probabilities": { "claude": 0.2, "codex": 0.7, "grok": 0.1 } } } })
    }

    #[test]
    fn needs_a_key_and_the_feature_flag() {
        assert!(enabled_with(Some("k_123"), Some("1")));
        assert!(enabled_with(Some("k_123"), Some("true")));
        assert!(!enabled_with(None, Some("1")), "no key");
        assert!(!enabled_with(Some(""), Some("1")), "empty key");
        assert!(!enabled_with(Some("k_123"), None), "flag unset");
        assert!(!enabled_with(Some("k_123"), Some("0")));
        assert!(!enabled_with(Some("k_123"), Some("")));
        assert!(!enabled_with(Some("bad\"key"), Some("1")), "a key that could break out of the config line");
        assert!(!enabled_with(Some("two words"), Some("1")));
    }

    #[test]
    fn model_is_pinned_and_endpoint_can_move() {
        assert_eq!(DEFAULT_MODEL, "jev-1.14.0");
        assert_eq!(endpoint(None).as_deref(), Some("https://api.typesafe.ai/v1/systemone"));
        assert_eq!(endpoint(Some("http://localhost:8080/")).as_deref(), Some("http://localhost:8080/v1/systemone"));
        assert_eq!(endpoint(Some("file:///etc/passwd")), None);
        assert_eq!(endpoint(Some("https://x\"y")), None);
    }

    #[test]
    fn confidence_gates() {
        assert_eq!(gate(0.0), Gate::Ignore);
        assert_eq!(gate(0.49), Gate::Ignore);
        assert_eq!(gate(0.5), Gate::Suggest);
        assert_eq!(gate(0.84), Gate::Suggest);
        assert_eq!(gate(0.85), Gate::Confident);
        assert_eq!(gate(1.0), Gate::Confident);
        assert_eq!(gate(f32::NAN), Gate::Ignore);
    }

    #[test]
    fn requests_have_the_documented_shape() {
        let req = request(
            json!({ "task": "fix it" }),
            vec![
                ("pick", choice_q("Which?", &[("a", "first"), ("b", "second")])),
                ("sev", score_q("How bad?", &["info", "low", "high", "critical"])),
                ("out", noul_q("Outside the project?")),
            ],
        );
        assert_eq!(req["model"], model());
        assert_eq!(req["state"]["task"], "fix it");
        assert_eq!(req["questions"]["pick"]["type"], "choice");
        assert_eq!(req["questions"]["pick"]["criteria"]["b"], "second");
        assert_eq!(req["questions"]["sev"]["criteria"], json!(["info", "low", "high", "critical"]), "score levels are an ordered array");
        assert_eq!(req["questions"]["out"]["type"], "noul");
        assert!(req["questions"]["out"].get("criteria").is_none());
    }

    #[test]
    fn reads_choice_answers_and_refuses_anything_odd() {
        let ok = choice(&choice_answer("codex"), "pick", &["claude", "codex"]).unwrap();
        assert_eq!(ok, Choice { label: "codex".into(), confidence: 0.72, probability: 0.7 });
        assert_eq!(choice(&choice_answer("grok"), "pick", &["claude", "codex"]), None, "a label we didn't offer");
        assert_eq!(choice(&choice_answer("codex"), "other", &["codex"]), None, "no such answer");
        let mut wrong_type = choice_answer("codex");
        wrong_type["answers"]["pick"]["type"] = json!("box");
        assert_eq!(choice(&wrong_type, "pick", &["codex"]), None, "unknown type");
        let mut no_conf = choice_answer("codex");
        no_conf["answers"]["pick"].as_object_mut().unwrap().remove("confidence");
        assert_eq!(choice(&no_conf, "pick", &["codex"]), None, "missing field");
        let mut no_probs = choice_answer("codex");
        no_probs["answers"]["pick"].as_object_mut().unwrap().remove("probabilities");
        assert_eq!(choice(&no_probs, "pick", &["codex"]), None);
        let mut silly = choice_answer("codex");
        silly["answers"]["pick"]["confidence"] = json!(7);
        assert_eq!(choice(&silly, "pick", &["codex"]), None, "out of range");
        assert_eq!(choice(&json!({ "detail": [] }), "pick", &["codex"]), None);
    }

    #[test]
    fn reads_score_and_noul_answers() {
        let v = json!({ "answers": {
            "sev": { "type": "score", "score": 1.43, "confidence": 0.35, "legend": {}, "probabilities": {} },
            "out": { "type": "noul", "noul": 0.98 },
            "over": { "type": "score", "score": 4.0, "confidence": 0.9 },
        } });
        assert_eq!(score(&v, "sev", 4), Some(Score { score: 1.43, confidence: 0.35 }));
        assert_eq!(score(&v, "over", 4), None, "beyond the top level");
        assert_eq!(score(&v, "out", 4), None, "wrong type");
        assert_eq!(noul(&v, "out"), Some(0.98));
        assert_eq!(noul(&v, "sev"), None);
        assert_eq!(noul(&json!({ "answers": { "out": { "type": "noul" } } }), "out"), None);
    }

    #[test]
    fn splits_a_curl_response() {
        let ok = b"HTTP/2 200 \r\ncontent-type: application/json\r\n\r\n{\"answers\":{}}";
        assert_eq!(parse_http(ok), Some((200, None, b"{\"answers\":{}}".to_vec())));
        let busy = b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 429 Too Many Requests\r\nRetry-After: 7\r\n\r\n{\"detail\":\"slow down\"}";
        let (s, r, b) = parse_http(busy).unwrap();
        assert_eq!((s, r), (429, Some(7)));
        assert_eq!(detail(&b), "\"slow down\"");
        assert_eq!(parse_http(b"HTTP/2 529 \r\nretry-after: soon\r\n\r\n").map(|(s, r, _)| (s, r)), Some((529, None)));
        assert_eq!(parse_http(b"garbage"), None);
    }

    #[test]
    fn the_key_stays_out_of_argv_and_the_body_is_escaped() {
        assert!(!CURL_ARGS.iter().any(|a| a.contains("secret")));
        let body = json!({ "state": "say \"hi\" \\ there\nnext" }).to_string();
        let cfg = config("https://api.typesafe.ai/v1/systemone", "secret", &body);
        assert!(cfg.contains("header = \"Authorization: Bearer secret\"\n"));
        assert!(cfg.contains("proto = \"=http,https\""));
        let line = cfg.lines().find(|l| l.starts_with("data-binary")).unwrap();
        assert!(line.contains("\\\"state\\\""), "quotes are escaped for curl's config parser: {line}");
        assert!(line.contains("\\\\n"), "the JSON's own backslash-n stays two characters: {line}");
        assert_eq!(cfg.lines().count(), 7, "the body cannot add config lines");
    }

    #[test]
    fn errors_read_plainly_and_never_carry_the_key() {
        assert_eq!(Error::Status(401, "x".into()).describe(), "Jev rejected the key (401)");
        for code in [400, 401, 413, 415, 422, 500] {
            assert!(!Error::Status(code, String::new()).describe().is_empty());
        }
        assert_eq!(Error::Disabled.describe(), "Jev is off");
    }

    #[test]
    fn identical_requests_share_a_cache_entry() {
        let a = request(json!("same"), vec![("q", noul_q("Yes?"))]);
        let b = request(json!("other"), vec![("q", noul_q("Yes?"))]);
        assert_eq!(digest(&a), digest(&a.clone()));
        assert_ne!(digest(&a), digest(&b));
        remember(digest(&a), &json!({ "answers": {} }));
        assert_eq!(cached(digest(&a)), Some(json!({ "answers": {} })));
        assert_eq!(cached(digest(&b)), None);
    }

    #[test]
    fn the_log_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("empire-jev-log-{}", std::process::id()));
        let path = dir.join("jev.log");
        append_log(&path, "route", json!({ "suggested": "codex", "chose": "claude" })).unwrap();
        append_log(&path, "route", json!({ "suggested": "grok", "chose": "grok" })).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.lines().count(), 2);
        assert!(text.contains("\"suggested\":\"codex\""));
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A one-shot local server: replies to each connection with the next canned response and
    /// hands back the raw requests it saw.
    fn serve(replies: Vec<&'static str>) -> (String, std::thread::JoinHandle<Vec<String>>) {
        use std::io::Read;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            let mut seen = Vec::new();
            for reply in replies {
                let (mut s, _) = listener.accept().unwrap();
                let mut buf = vec![0u8; 65536];
                let mut got = 0;
                loop {
                    let n = s.read(&mut buf[got..]).unwrap();
                    got += n;
                    let text = String::from_utf8_lossy(&buf[..got]).into_owned();
                    let have_body = text.split_once("\r\n\r\n").is_some_and(|(h, b)| {
                        let want: usize = h.lines().find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").and_then(|v| v.trim().parse().ok())).unwrap_or(0);
                        b.len() >= want
                    });
                    if n == 0 || have_body {
                        seen.push(text);
                        break;
                    }
                }
                s.write_all(reply.as_bytes()).unwrap();
            }
            seen
        });
        (url, handle)
    }

    #[test]
    fn talks_to_a_server_through_curl_and_honors_retry_after() {
        let (url, server) = serve(vec![
            "HTTP/1.1 429 Too Many Requests\r\nretry-after: 0\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 59\r\nconnection: close\r\n\r\n{\"answers\":{\"x\":{\"type\":\"noul\",\"noul\":0.25}},\"model\":\"m\"}   ",
        ]);
        let req = request(json!({ "task": "say \"hi\"\\" }), vec![("x", noul_q("Yes?"))]);
        let v = post_with(&format!("{url}{PATH}"), "test-key-123", &req).expect("retried after the 429");
        assert_eq!(noul(&v, "x"), Some(0.25));
        let seen = server.join().unwrap();
        assert_eq!(seen.len(), 2, "one retry");
        assert!(seen[0].starts_with("POST /v1/systemone"));
        assert!(seen[0].contains("Bearer test-key-123"));
        let body = seen[0].split_once("\r\n\r\n").unwrap().1;
        assert_eq!(serde_json::from_str::<Value>(body).unwrap(), req, "the body arrives intact");
        // and the second call is served from the cache without the server
        assert_eq!(post_with("http://127.0.0.1:9", "k", &req), Ok(v));
    }

    #[test]
    fn error_statuses_and_a_long_retry_after_fail_closed() {
        let (url, server) = serve(vec![
            "HTTP/1.1 401 Unauthorized\r\ncontent-length: 20\r\nconnection: close\r\n\r\n{\"detail\":\"nope!!!\"}",
            "HTTP/1.1 529 Overloaded\r\nretry-after: 600\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
            "HTTP/1.1 200 OK\r\ncontent-length: 8\r\nconnection: close\r\n\r\nnot json",
        ]);
        let url = format!("{url}{PATH}");
        let q = |t: &str| request(json!(t), vec![("x", noul_q("Yes?"))]);
        assert!(matches!(post_with(&url, "k", &q("a")), Err(Error::Status(401, _))));
        assert_eq!(post_with(&url, "k", &q("b")), Err(Error::Busy { retry_after: Some(600) }));
        assert_eq!(post_with(&url, "k", &q("c")), Err(Error::Malformed));
        server.join().unwrap();
        assert!(matches!(post_with("http://127.0.0.1:9/v1/systemone", "k", &q("d")), Err(Error::Transport(_))), "nothing listening");
    }

    #[test]
    fn nothing_is_asked_without_a_key() {
        // an unset key is a plain Disabled; only meaningful where the key really is unset
        if std::env::var_os("TYPESAFE_API_KEY").is_none() {
            assert_eq!(post(&json!({})), Err(Error::Disabled));
        }
    }

    /// Against the real API: `TYPESAFE_API_KEY=... cargo test jev_live -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn jev_live() {
        let req = request(
            json!({ "task": "Rename a variable in a Rust function", "project": "demo" }),
            vec![
                ("pick", choice_q("Which agent should do this task?", &[("claude", "Claude Code"), ("codex", "Codex CLI"), ("grok", "Grok CLI")])),
                ("sev", score_q("How serious is this task?", &["trivial", "routine", "important", "urgent"])),
                ("out", noul_q("Does the task touch anything outside the project?")),
            ],
        );
        let v = post(&req).expect("live answer");
        println!("{v:#}");
        assert!(choice(&v, "pick", &["claude", "codex", "grok"]).is_some());
        assert!(score(&v, "sev", 4).is_some());
        assert!(noul(&v, "out").is_some());
    }
}
