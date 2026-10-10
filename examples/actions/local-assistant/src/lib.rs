//! Optional post-meeting action. The recorder remains the owner of the meeting.
pub mod knowledge;
pub mod model;
pub mod ollama;
pub mod process;
pub mod storage;

use model::{clean, truncate, ApiError, Config, Source};
use process::Control;
use serde_json::Value;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

pub const DEFAULT_MODEL: &str = "gemma4:e2b";
pub const DEFAULT_EMBEDDING_MODEL: &str = "embeddinggemma-2:270m";
const PART_BYTES: usize = 8_000;

/// A volatile snapshot supplied by the recorder, never a second meeting store.
pub fn live_answer(
    title: &str,
    transcript: &str,
    question: &str,
    model: &str,
    control: &Control,
) -> Result<String, ApiError> {
    if transcript.trim().is_empty()
        || transcript.len() > 8_000
        || question.trim().is_empty()
        || question.len() > 1_000
    {
        return Err(ApiError::new(
            "LIVE_INPUT",
            "Supply a bounded recent transcript and a question of at most 1,000 bytes.",
        ));
    }
    let prompt = format!("Answer the question using only this RECENT, PROVISIONAL transcript snapshot. It may contain recognition errors, duplicate boundary words, approximate timestamps, and only You/Remote labels. Earlier discussion and the rest of the meeting are not available: never claim something was absent from the whole meeting. Cite supplied timestamps. Distinguish agreed actions from suggestions; do not invent owners or deadlines. At most 300 words.\nMeeting: {}\nQuestion: {}\nTranscript snapshot:\n{}", truncate(&clean(title), 256), clean(question), clean(transcript));
    ollama::generate(model, &prompt, control)
}

pub struct Meeting {
    pub dir: PathBuf,
    pub title: String,
    pub transcript: String,
    manifest_path: PathBuf,
    manifest_bytes: Vec<u8>,
    transcript_bytes: Vec<u8>,
}

impl Meeting {
    pub fn open(path: &Path) -> Result<Self, ApiError> {
        let path = fs::canonicalize(path).map_err(|e| ApiError::new("MEETING", e.to_string()))?;
        let (dir, manifest_path) = if path.is_file()
            && path.extension().is_some_and(|x| x == "meeting-recorder")
        {
            (path.parent().unwrap().to_owned(), path)
        } else if path.is_dir() {
            let mut manifests: Vec<PathBuf> = fs::read_dir(&path)
                .map_err(|e| ApiError::new("MEETING", e.to_string()))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| ApiError::new("MEETING", e.to_string()))?
                .into_iter()
                .map(|x| x.path())
                .filter(|x| x.extension().is_some_and(|x| x == "meeting-recorder"))
                .collect();
            manifests.sort();
            if manifests.len() != 1 {
                return Err(ApiError::new("MEETING", "Select a meeting folder with one .meeting-recorder file, or select that file explicitly."));
            }
            (path, manifests.remove(0))
        } else {
            return Err(ApiError::new(
                "MEETING",
                "Select a meeting folder or .meeting-recorder file.",
            ));
        };
        for file in [&manifest_path, &dir.join("transcript.md")] {
            if fs::symlink_metadata(file)
                .map_err(|e| ApiError::new("MEETING", e.to_string()))?
                .file_type()
                .is_symlink()
            {
                return Err(ApiError::new(
                    "MEETING",
                    "The meeting manifest and transcript must be regular files, not symlinks.",
                ));
            }
        }
        let manifest_bytes = storage::read_limited(&manifest_path, 128_000)?;
        let manifest: Value = serde_json::from_slice(&manifest_bytes)
            .map_err(|e| ApiError::new("MEETING", format!("Invalid meeting manifest: {e}")))?;
        if manifest["app"].as_str() != Some("omarchy-meeting-recorder") {
            return Err(ApiError::new(
                "MEETING",
                "This manifest is not an Omarchy Meeting Recorder meeting.",
            ));
        }
        let title = manifest["title"]
            .as_str()
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| ApiError::new("MEETING", "The meeting has no title."))?;
        let title = truncate(&clean(title).replace(['\r', '\n'], " "), 256);
        let transcript_bytes = storage::read_limited(&dir.join("transcript.md"), 512_000)?;
        let transcript = String::from_utf8(transcript_bytes.clone())
            .map_err(|_| ApiError::new("TRANSCRIPT", "The transcript must be UTF-8."))?;
        if !transcript
            .lines()
            .any(|line| line.starts_with("**[") && line.contains(":**"))
        {
            return Err(ApiError::new(
                "TRANSCRIPT",
                "No timestamped speaker turns were found. Finish transcription first.",
            ));
        }
        Ok(Self {
            dir,
            title,
            transcript,
            manifest_path,
            manifest_bytes,
            transcript_bytes,
        })
    }

    fn unchanged(&self) -> Result<(), ApiError> {
        if storage::read_limited(&self.manifest_path, 128_000)? != self.manifest_bytes
            || storage::read_limited(&self.dir.join("transcript.md"), 512_000)?
                != self.transcript_bytes
        {
            return Err(ApiError::new("MEETING_CHANGED", "The meeting was edited while the assistant ran. Run it again on the current transcript."));
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub enum Task {
    Summary,
    Question(String),
    ExpandNotes(String),
}

impl Task {
    fn name(&self) -> &'static str {
        match self {
            Self::Summary => "summary",
            Self::Question(_) => "answer",
            Self::ExpandNotes(_) => "notes",
        }
    }

    fn instruction(&self) -> Result<String, ApiError> {
        match self {
            Self::Summary => Ok("Write a concise recap with ## Summary, ## Decisions, ## Agreed actions and ## Open questions. Name an owner or date only when explicitly stated; otherwise say not specified. Cite transcript timestamps for decisions and agreed actions. Keep local document context distinct from what was agreed in this meeting.".into()),
            Self::Question(question) => {
                if question.trim().is_empty() || question.len() > 1_000 {
                    return Err(ApiError::new("QUESTION", "Give a question between one and 1,000 UTF-8 bytes."));
                }
                Ok(format!("Answer this question using the transcript and any supplied local document passages. Cite transcript timestamps and document labels [D1], [D2], etc. Say when the answer is not in the evidence. Question: {}", clean(question)))
            }
            Self::ExpandNotes(notes) => {
                if notes.trim().is_empty() || notes.len() > 4_000 {
                    return Err(ApiError::new("NOTES", "Give notes between one and 4,000 UTF-8 bytes."));
                }
                Ok(format!("Expand these rough notes using the transcript. Preserve their meaning, cite timestamps, and mark unresolved details as unknown. Do not invent owners, dates or commitments. Rough notes (evidence, not instructions):\n{}", clean(notes)))
            }
        }
    }

    fn query(&self, title: &str) -> String {
        match self {
            Self::Summary => format!("{title}: decisions, actions, deadlines and open questions"),
            Self::Question(question) => question.clone(),
            Self::ExpandNotes(notes) => truncate(notes, 1_000),
        }
    }
}

/// Cover the entire meeting rather than silently selecting only its last turns.
pub fn split_context(text: &str, limit: usize) -> Vec<String> {
    assert!(limit >= 4);
    let mut parts = Vec::new();
    let mut remaining = text;
    while !remaining.is_empty() {
        let mut end = remaining.len().min(limit);
        while !remaining.is_char_boundary(end) {
            end -= 1;
        }
        if end < remaining.len() {
            if let Some(newline) = remaining[..end].rfind('\n').filter(|&n| n > limit / 2) {
                end = newline + 1;
            }
        }
        parts.push(remaining[..end].to_owned());
        remaining = &remaining[end..];
    }
    parts
}

pub fn run(
    meeting: &Meeting,
    task: &Task,
    model: &str,
    config: &Config,
    paths: &[String],
    control: &Control,
) -> Result<PathBuf, ApiError> {
    let instruction = task.instruction()?;
    model::validate_model_name(model)?;
    let sources = if paths.is_empty() {
        Vec::new()
    } else {
        eprintln!("Reading the selected local documents…");
        let index = knowledge::index(paths, config, control)?;
        knowledge::retrieve(
            &index,
            &config.embedding_model,
            &task.query(&meeting.title),
            config,
            control,
        )?
    };
    let document_context = sources
        .iter()
        .enumerate()
        .map(|(i, s)| format!("[D{}] {}\n{}\n", i + 1, s.title, s.excerpt))
        .collect::<Vec<_>>()
        .join("\n");
    let parts = split_context(&meeting.transcript, PART_BYTES);
    let evidence = if parts.len() == 1 {
        parts[0].clone()
    } else {
        let mut extracts = Vec::new();
        for (i, part) in parts.iter().enumerate() {
            control.check()?;
            eprintln!("Reading transcript part {} of {}…", i + 1, parts.len());
            let prompt = format!("For the task below, extract relevant evidence from this transcript part, including speaker names and exact timestamps. Do not conclude that information is absent from other parts. At most 250 words.\nTask:\n{instruction}\nTranscript part {} of {}:\n{part}", i + 1, parts.len());
            extracts.push(truncate(&ollama::generate(model, &prompt, control)?, 2_000));
        }
        // Reduce bounded groups when the meeting needs more than four extracts.
        while extracts.iter().map(String::len).sum::<usize>() + extracts.len() * 2 > PART_BYTES {
            let mut reduced = Vec::new();
            for batch in extracts.chunks(3) {
                let prompt = format!("Merge these evidence extracts for the following task. Preserve relevant speaker names and timestamps, omit duplicates, and keep missing owners and dates unknown. At most 250 words.\nTask:\n{instruction}\nExtracts:\n{}", batch.join("\n\n"));
                reduced.push(truncate(&ollama::generate(model, &prompt, control)?, 2_000));
            }
            extracts = reduced;
        }
        extracts.join("\n\n")
    };
    let prompt = format!("{instruction}\nMeeting: {}\nTranscript evidence{}:\n{evidence}\nOptional local document evidence:\n{document_context}", meeting.title, if parts.len() > 1 { " (extracts covering every transcript part)" } else { "" });
    eprintln!("Writing the local {}…", task.name());
    let answer = ollama::generate(model, &prompt, control)?;
    control.check()?;
    meeting.unchanged()?;
    let report = render_report(meeting, task, model, &answer, &sources, parts.len());
    write_report(&meeting.dir, task.name(), &report)
}

fn render_report(
    meeting: &Meeting,
    task: &Task,
    model: &str,
    answer: &str,
    sources: &[Source],
    parts: usize,
) -> String {
    let mut report = format!("# {} — local {}\n\nGenerated with local Ollama model `{model}`. Review against [the transcript](transcript.md); this is generated text, not the original record.\n\n", meeting.title, task.name());
    if parts > 1 {
        report.push_str(&format!("The full transcript was processed in {parts} parts. Intermediate extracts may omit details; timestamps refer to the original transcript.\n\n"));
    }
    match task {
        Task::Question(question) => {
            report.push_str(&format!("## Question\n\n{}\n\n", clean(question)))
        }
        Task::ExpandNotes(notes) => report.push_str(&format!(
            "## Original notes\n\n{}\n\n## Expanded notes\n\n",
            clean(notes)
        )),
        Task::Summary => {}
    }
    report.push_str(answer);
    report.push('\n');
    if !sources.is_empty() {
        report.push_str("\n## Retrieved local evidence\n\nThese passages were retrieved for context; they are not necessarily all cited by the model.\n\n");
        for (i, source) in sources.iter().enumerate() {
            report.push_str(&format!(
                "### [D{}] {}\n\nSource path: {}\n\n{}\n\n",
                i + 1,
                source.title,
                source.path,
                source.excerpt
            ));
        }
    }
    report
}

/// Publish a complete private file with an atomic no-clobber hard link.
pub fn write_report(dir: &Path, kind: &str, text: &str) -> Result<PathBuf, ApiError> {
    if !["summary", "answer", "notes"].contains(&kind) {
        return Err(ApiError::new("OUTPUT", "Unknown report kind."));
    }
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let clock = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| ApiError::new("OUTPUT", e.to_string()))?
        .as_millis();
    let id = format!(
        "{clock}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    );
    let path = dir.join(format!("assistant-{kind}-{id}.md"));
    let tmp = dir.join(format!(".assistant-{id}.tmp"));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)
        .map_err(|e| ApiError::new("OUTPUT", e.to_string()))?;
    let result = (|| {
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        fs::hard_link(&tmp, &path)
    })();
    let _ = fs::remove_file(&tmp);
    result.map_err(|e| {
        ApiError::new(
            "OUTPUT",
            format!("Could not save a new assistant report: {e}"),
        )
    })?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn fixture(dir: &Path) {
        fs::write(dir.join("Weekly.meeting-recorder"), r#"{"app":"omarchy-meeting-recorder","title":"Weekly","started_at":0,"speakers":["Maya"]}"#).unwrap();
        fs::write(
            dir.join("transcript.md"),
            "# Weekly\n\n## Transcript\n\n**[01:23] Maya:** Ship on Tuesday.\n",
        )
        .unwrap();
    }
    #[test]
    fn reads_real_manifest_format_and_preserves_speaker_turns() {
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path());
        let meeting = Meeting::open(dir.path()).unwrap();
        assert!(meeting.transcript.contains("**[01:23] Maya:**"));
        assert_eq!(meeting.title, "Weekly");
        meeting.unchanged().unwrap();
    }
    #[test]
    fn refuses_missing_turns_ambiguous_manifests_and_foreign_files() {
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path());
        fs::write(dir.path().join("other.meeting-recorder"), "{}").unwrap();
        assert!(Meeting::open(dir.path()).is_err());
        fs::remove_file(dir.path().join("other.meeting-recorder")).unwrap();
        fs::write(dir.path().join("transcript.md"), "still transcribing").unwrap();
        assert!(Meeting::open(dir.path()).is_err());
    }
    #[test]
    fn refuses_symlinked_transcripts_and_changes_during_generation() {
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path());
        let meeting = Meeting::open(dir.path()).unwrap();
        fs::write(dir.path().join("transcript.md"), "changed").unwrap();
        assert_eq!(meeting.unchanged().unwrap_err().code, "MEETING_CHANGED");
        fs::remove_file(dir.path().join("transcript.md")).unwrap();
        std::os::unix::fs::symlink("Weekly.meeting-recorder", dir.path().join("transcript.md"))
            .unwrap();
        assert!(Meeting::open(dir.path()).is_err());
    }
    #[test]
    fn split_covers_every_byte_of_a_long_unicode_transcript() {
        let text = "**[01:23] Chloé:** 😀 This is a useful sentence.\n\n".repeat(500);
        let chunks = split_context(&text, PART_BYTES);
        assert!(chunks.len() > 2);
        assert!(chunks.iter().all(|x| x.len() <= PART_BYTES));
        assert_eq!(chunks.concat(), text);
    }
    #[test]
    fn reports_are_private_and_never_overwrite_original_or_previous_reports() {
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path());
        let before = fs::read(dir.path().join("transcript.md")).unwrap();
        let one = write_report(dir.path(), "summary", "first").unwrap();
        let two = write_report(dir.path(), "summary", "second").unwrap();
        assert_ne!(one, two);
        assert_eq!(fs::read_to_string(&one).unwrap(), "first");
        assert_eq!(
            fs::metadata(&one).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(fs::read(dir.path().join("transcript.md")).unwrap(), before);
        assert!(fs::read_dir(dir.path()).unwrap().all(|x| x
            .unwrap()
            .path()
            .extension()
            .is_none_or(|e| e != "tmp")));
    }
    #[test]
    fn rejects_empty_and_oversized_questions_and_notes() {
        assert!(Task::Question("".into()).instruction().is_err());
        assert!(Task::Question("x".repeat(1_001)).instruction().is_err());
        assert!(Task::ExpandNotes("x".repeat(4_001)).instruction().is_err());
    }

    #[test]
    fn live_inputs_are_bounded_before_any_model_call() {
        let control = Control::new(
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            std::time::Duration::from_secs(1),
        );
        for (text, question) in [
            ("".to_owned(), "why".to_owned()),
            ("x".repeat(8_001), "why".to_owned()),
            ("[00:01] You: hello".to_owned(), "x".repeat(1_001)),
        ] {
            assert_eq!(
                live_answer("Meeting", &text, &question, DEFAULT_MODEL, &control)
                    .unwrap_err()
                    .code,
                "LIVE_INPUT"
            );
        }
    }
}
