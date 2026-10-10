use meeting_local_assistant::{
    model::{ApiError, Config},
    process::{self, Control},
    Meeting, Task, DEFAULT_EMBEDDING_MODEL, DEFAULT_MODEL,
};
use std::path::PathBuf;
use std::sync::{atomic::AtomicBool, Arc};
use std::time::Duration;

fn main() {
    process::install_signals();
    if let Err(error) = execute() {
        eprintln!("Local assistant failed: {error}");
        std::process::exit(1);
    }
}

fn execute() -> Result<(), ApiError> {
    let mut args = std::env::args().skip(1);
    let command = args.next().unwrap_or_else(|| "summary".into());
    if command == "--version" {
        println!("meeting-local-assistant {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if ["--help", "-h"].contains(&command.as_str()) {
        println!("Usage: meeting-local-assistant [summary|ask|expand-notes|check] [meeting folder]\n  --model NAME             Installed local Ollama model (default: {DEFAULT_MODEL})\n  --embedding-model NAME   Local embedding model (default: {DEFAULT_EMBEDDING_MODEL})\n  --question TEXT          For ask; maximum 1,000 bytes\n  --notes FILE             For expand-notes; maximum 4,000 bytes\n  --knowledge PATH         Opt-in local Markdown, text, PDF or folder (repeatable)\nWhen no meeting folder is given, MEETING_DIR supplies it. Each run saves a new Markdown report.\ncheck verifies the chosen local models without reading a meeting.");
        return Ok(());
    }
    if !["summary", "ask", "expand-notes", "check"].contains(&command.as_str()) {
        return Err(ApiError::new("USAGE", "Unknown command; use --help."));
    }
    let mut model = DEFAULT_MODEL.to_owned();
    let mut config = Config {
        embedding_model: DEFAULT_EMBEDDING_MODEL.into(),
    };
    let mut folder = None;
    let mut question = None;
    let mut notes = None;
    let mut knowledge = Vec::new();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--model" | "--embedding-model" | "--question" | "--notes" | "--knowledge" => {
                let value = args
                    .next()
                    .ok_or_else(|| ApiError::new("USAGE", format!("{arg} needs a value.")))?;
                match arg.as_str() {
                    "--model" => model = value,
                    "--embedding-model" => config.embedding_model = value,
                    "--question" => question = Some(value),
                    "--notes" => notes = Some(PathBuf::from(value)),
                    _ => knowledge.push(value),
                }
            }
            _ if !arg.starts_with('-') && folder.is_none() => folder = Some(PathBuf::from(arg)),
            _ => {
                return Err(ApiError::new(
                    "USAGE",
                    format!("Unexpected argument {arg}; use --help."),
                ))
            }
        }
    }
    if command != "ask" && question.is_some() {
        return Err(ApiError::new("USAGE", "--question is only for ask."));
    }
    if command != "expand-notes" && notes.is_some() {
        return Err(ApiError::new("USAGE", "--notes is only for expand-notes."));
    }
    if knowledge.len() > 16 {
        return Err(ApiError::new("USAGE", "Select at most 16 knowledge paths."));
    }
    let control = Control::new(Arc::new(AtomicBool::new(false)), Duration::from_secs(480));
    if command == "check" {
        meeting_local_assistant::ollama::verify_local(&model, &control)?;
        if !knowledge.is_empty() {
            meeting_local_assistant::ollama::verify_local(&config.embedding_model, &control)?;
        }
        println!("Local model ready: {model}");
        return Ok(());
    }
    let task = match command.as_str() {
        "ask" => Task::Question(
            question.ok_or_else(|| ApiError::new("USAGE", "ask needs --question TEXT."))?,
        ),
        "expand-notes" => {
            let path =
                notes.ok_or_else(|| ApiError::new("USAGE", "expand-notes needs --notes FILE."))?;
            let bytes = meeting_local_assistant::storage::read_limited(&path, 4_000)?;
            Task::ExpandNotes(
                String::from_utf8(bytes)
                    .map_err(|_| ApiError::new("NOTES", "Notes must be UTF-8."))?,
            )
        }
        _ => Task::Summary,
    };
    let folder = folder
        .or_else(|| std::env::var_os("MEETING_DIR").map(PathBuf::from))
        .ok_or_else(|| {
            ApiError::new(
                "USAGE",
                "Select a meeting folder or run this from the recorder's Actions menu.",
            )
        })?;
    let meeting = Meeting::open(&folder)?;
    let path =
        meeting_local_assistant::run(&meeting, &task, &model, &config, &knowledge, &control)?;
    println!(
        "Saved {} — use Open folder to read it",
        path.file_name().unwrap().to_string_lossy()
    );
    Ok(())
}
