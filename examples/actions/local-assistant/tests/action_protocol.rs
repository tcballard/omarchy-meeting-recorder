//! Exercises the production executable and real curl against a local HTTP fixture.
//! The fixture is not a model and makes no claim about inference quality.
use serde_json::{json, Value};
use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::thread;
use std::time::Duration;

#[derive(Default)]
struct State {
    requests: Vec<(String, Value)>,
    remote: bool,
    fail_generate: bool,
    change_transcript: Option<PathBuf>,
}

struct Server {
    state: Arc<Mutex<State>>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

impl Server {
    fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:11434")
            .expect("This protocol test needs port 11434 unused; stop Ollama while testing.");
        listener.set_nonblocking(true).unwrap();
        let state = Arc::new(Mutex::new(State::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let (shared, stopping) = (state.clone(), stop.clone());
        let worker = thread::spawn(move || {
            while !stopping.load(Ordering::SeqCst) {
                let (mut stream, _) = match listener.accept() {
                    Ok(pair) => pair,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(e) => panic!("fixture accept: {e}"),
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut bytes = Vec::new();
                let mut buffer = [0; 4096];
                let header_end = loop {
                    let n = stream.read(&mut buffer).unwrap();
                    assert!(n > 0);
                    bytes.extend_from_slice(&buffer[..n]);
                    if let Some(i) = bytes.windows(4).position(|x| x == b"\r\n\r\n") {
                        break i + 4;
                    }
                    assert!(bytes.len() < 32_000);
                };
                let headers = String::from_utf8(bytes[..header_end].to_vec()).unwrap();
                let route = headers
                    .lines()
                    .next()
                    .unwrap()
                    .split_whitespace()
                    .nth(1)
                    .unwrap()
                    .to_owned();
                let length = headers
                    .lines()
                    .find_map(|line| {
                        let (key, value) = line.split_once(':')?;
                        key.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                assert!(length <= 1_000_000);
                while bytes.len() < header_end + length {
                    let n = stream.read(&mut buffer).unwrap();
                    assert!(n > 0);
                    bytes.extend_from_slice(&buffer[..n]);
                }
                let body = if length == 0 {
                    Value::Null
                } else {
                    serde_json::from_slice(&bytes[header_end..header_end + length]).unwrap()
                };
                let mut state = shared.lock().unwrap();
                state.requests.push((route.clone(), body.clone()));
                let response = match route.as_str() {
                    "/api/tags" => json!({"models":[
                        {"name":"gemma4:e2b","remote_model":if state.remote { json!("cloud-alias") } else { Value::Null }},
                        {"name":"embeddinggemma-2:270m"}
                    ]}),
                    "/api/show" => json!({"model_info":{"general.architecture":"gemma"}}),
                    "/api/embed" => {
                        json!({"embeddings":body["input"].as_array().unwrap().iter().map(|_| vec![1., 0., 0.]).collect::<Vec<_>>()})
                    }
                    "/api/generate" => {
                        if let Some(path) = state.change_transcript.take() {
                            fs::write(path, "**[01:23] Chloé:** Edited while running.\n").unwrap();
                        }
                        if state.fail_generate {
                            json!({"error":"fixture inference failure"})
                        } else {
                            json!({"response":"## Summary\n\nChloé agreed to review on Tuesday [01:23].\n\n## Agreed actions\n\n- Review: Chloé; date: Tuesday [01:23].\n"})
                        }
                    }
                    _ => panic!("unexpected local route: {route}"),
                };
                drop(state);
                let body = serde_json::to_vec(&response).unwrap();
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
                stream.write_all(&body).unwrap();
            }
        });
        Self {
            state,
            stop,
            worker: Some(worker),
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.worker.take().unwrap().join().unwrap();
    }
}

fn invoke(home: &Path, meeting: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_meeting-local-assistant"))
        .args(args)
        .current_dir(meeting)
        .env("HOME", home)
        .env("MEETING_DIR", meeting)
        .env("MEETING_TRANSCRIPT", meeting.join("transcript.md"))
        .env("MEETING_MANIFEST", meeting.join("Weekly.meeting-recorder"))
        .env("MEETING_TITLE", "Weekly Δ")
        .env("MEETING_DATE", "2026-10-10 10:00")
        .env("MEETING_STARTED_AT", "0")
        .env("MEETING_DURATION", "120")
        .env("MEETING_LANGUAGE", "en")
        .env("MEETING_SPEAKERS", "Chloé\nOwen")
        .env("HTTP_PROXY", "http://127.0.0.1:1")
        .env("http_proxy", "http://127.0.0.1:1")
        .env("ALL_PROXY", "http://127.0.0.1:1")
        .env("all_proxy", "http://127.0.0.1:1")
        .output()
        .unwrap()
}

fn reports(dir: &Path) -> Vec<PathBuf> {
    let mut paths: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .map(|x| x.unwrap().path())
        .filter(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("assistant-")
        })
        .collect();
    paths.sort();
    paths
}

#[test]
fn action_runs_summary_questions_notes_and_rag_without_mutating_the_recording() {
    let server = Server::new();
    let sandbox = tempfile::tempdir().unwrap();
    let home = sandbox.path().join("home");
    let meeting = sandbox.path().join("202610101000 Weekly Δ");
    fs::create_dir(&home).unwrap();
    fs::create_dir(&meeting).unwrap();
    // The production transport must ignore personal curl configuration and proxies.
    fs::write(
        home.join(".curlrc"),
        "url = http://127.0.0.1:1/never\nlocation\n",
    )
    .unwrap();
    let manifest = r#"{"app":"omarchy-meeting-recorder","version":1,"title":"Weekly Δ","started_at":0,"duration_secs":120,"format":"mono","language":"en","speakers":["Chloé","Owen"],"chapters":[]}"#;
    fs::write(meeting.join("Weekly.meeting-recorder"), manifest).unwrap();
    let transcript = "# Weekly Δ\n\n## Transcript\n\n**[01:23] Chloé:** I'll review on Tuesday.\n\n**[1:01:24] Owen:** That works.\n";
    fs::write(meeting.join("transcript.md"), transcript).unwrap();
    fs::write(meeting.join("audio.ogg"), b"untouched audio fixture").unwrap();

    let summary = invoke(&home, &meeting, &["summary"]);
    assert!(
        summary.status.success(),
        "{}",
        String::from_utf8_lossy(&summary.stderr)
    );
    assert!(String::from_utf8_lossy(&summary.stdout).contains("Saved assistant-summary-"));
    let first = reports(&meeting).remove(0);
    assert_eq!(
        fs::metadata(&first).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let again = invoke(&home, &meeting, &["summary"]);
    assert!(again.status.success());
    assert_eq!(reports(&meeting).len(), 2);
    assert!(invoke(
        &home,
        &meeting,
        &["ask", "--question", "Who reviews, and when?"]
    )
    .status
    .success());
    let notes = sandbox.path().join("notes.md");
    fs::write(&notes, "Chloé → review; Tue?").unwrap();
    assert!(invoke(
        &home,
        &meeting,
        &["expand-notes", "--notes", notes.to_str().unwrap()]
    )
    .status
    .success());
    let knowledge = sandbox.path().join("Deadline.md");
    fs::write(&knowledge, "The release freeze is Thursday.").unwrap();
    assert!(invoke(
        &home,
        &meeting,
        &[
            "ask",
            "--question",
            "How does Tuesday fit the release freeze?",
            "--knowledge",
            knowledge.to_str().unwrap()
        ]
    )
    .status
    .success());
    assert!(reports(&meeting)
        .iter()
        .any(|p| fs::read_to_string(p).unwrap().contains("### [D1] Deadline")));
    assert_eq!(reports(&meeting).len(), 5);
    assert_eq!(
        fs::read_to_string(meeting.join("transcript.md")).unwrap(),
        transcript
    );
    assert_eq!(
        fs::read_to_string(meeting.join("Weekly.meeting-recorder")).unwrap(),
        manifest
    );
    assert_eq!(
        fs::read(meeting.join("audio.ogg")).unwrap(),
        b"untouched audio fixture"
    );
    {
        let state = server.state.lock().unwrap();
        let prompts: Vec<_> = state
            .requests
            .iter()
            .filter(|(route, _)| route == "/api/generate")
            .map(|(_, body)| body)
            .collect();
        assert!(prompts.iter().all(|body| body["system"]
            .as_str()
            .unwrap()
            .contains("untrusted evidence")));
        assert!(prompts[0]["prompt"]
            .as_str()
            .unwrap()
            .contains("**[1:01:24] Owen:**"));
        let embeds: Vec<_> = state
            .requests
            .iter()
            .filter(|(route, _)| route == "/api/embed")
            .map(|(_, body)| body["input"][0].as_str().unwrap())
            .collect();
        assert!(embeds
            .iter()
            .any(|s| s.starts_with("title: Deadline | text:")));
        assert!(embeds
            .iter()
            .any(|s| s.starts_with("task: question answering | query:")));
    }
    // A long meeting includes every part, including the final turn, in the requests.
    // The native recorder's live path uses an immutable snapshot and never
    // creates a second transcript/report. Exercise the same real transport.
    let report_count = reports(&meeting).len();
    let control = meeting_local_assistant::process::Control::new(
        Arc::new(AtomicBool::new(false)),
        Duration::from_secs(10),
    );
    let live = meeting_local_assistant::live_answer(
        "Weekly",
        "[01:23] You: I'll review on Tuesday.",
        "Who reviews?",
        "gemma4:e2b",
        &control,
    )
    .unwrap();
    assert!(live.contains("[01:23]"));
    assert_eq!(reports(&meeting).len(), report_count);
    let state = server.state.lock().unwrap();
    let prompt = state
        .requests
        .iter()
        .rev()
        .find(|(route, _)| route == "/api/generate")
        .unwrap()
        .1["prompt"]
        .as_str()
        .unwrap();
    assert!(prompt.contains("RECENT, PROVISIONAL"));
    assert!(prompt.contains("Earlier discussion and the rest of the meeting are not available"));
    drop(state);
    let long = format!(
        "{}**[1:59:59] Owen:** END_OF_MEETING_SENTINEL.\n",
        transcript.repeat(200)
    );
    fs::write(meeting.join("transcript.md"), &long).unwrap();
    assert!(invoke(&home, &meeting, &["summary"]).status.success());
    assert!(server
        .state
        .lock()
        .unwrap()
        .requests
        .iter()
        .any(|(_, body)| body["prompt"]
            .as_str()
            .is_some_and(|s| s.contains("END_OF_MEETING_SENTINEL"))));
    fs::write(meeting.join("transcript.md"), transcript).unwrap();
    let before = reports(&meeting).len();
    // Refuse a remote alias before sending any transcript to generate.
    {
        let mut state = server.state.lock().unwrap();
        state.remote = true;
        state.requests.clear();
    }
    let cloud = invoke(&home, &meeting, &["summary"]);
    assert!(!cloud.status.success());
    assert!(String::from_utf8_lossy(&cloud.stderr).contains("REMOTE_MODEL"));
    assert!(!server
        .state
        .lock()
        .unwrap()
        .requests
        .iter()
        .any(|(route, _)| route == "/api/generate"));
    assert_eq!(reports(&meeting).len(), before);
    {
        let mut state = server.state.lock().unwrap();
        state.remote = false;
        state.fail_generate = true;
    }
    assert!(!invoke(&home, &meeting, &["summary"]).status.success());
    assert_eq!(reports(&meeting).len(), before);
    {
        let mut state = server.state.lock().unwrap();
        state.fail_generate = false;
        state.change_transcript = Some(meeting.join("transcript.md"));
    }
    let stale = invoke(&home, &meeting, &["summary"]);
    assert!(!stale.status.success());
    assert!(String::from_utf8_lossy(&stale.stderr).contains("MEETING_CHANGED"));
    assert_eq!(reports(&meeting).len(), before);
}
