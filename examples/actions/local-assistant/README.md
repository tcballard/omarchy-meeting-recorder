# Local meeting assistant (experimental)

An optional Rust Action for finished meetings: a recap with decisions and agreed actions, answers to questions, and expansion of rough notes. It can retrieve context from local Markdown, text and PDF documents you explicitly select. Recording, transcription, speaker names, chapters and playback stay in Meeting Recorder.

This is a Foresight-inspired prototype for testing the Actions integration. It is not affiliated with Google. It does not supply live captions or listen during a meeting.

## Try it on Omarchy

Keep your installed Meeting Recorder. With Rust 1.88 or newer, `curl`, `rg` and local Ollama available, run the installer from this checkout:

```bash
./examples/actions/local-assistant/install
```

The installer builds a native executable, puts it in `~/.local/bin`, backs up your existing recorder configuration, and adds **Local meeting recap (Ollama)** to the Actions menu. It preserves existing settings and actions. It does not replace the recorder, enable automatic actions, or download models. It refuses to overwrite an existing assistant executable or a symlinked configuration file.

Alternatively, download the `meeting-local-assistant-x86_64-linux` artifact from a successful **Local assistant** workflow run on this branch. Extract the inner `.tar.gz` and run `./install` from that folder. That package contains the same native executable and needs `curl` and `rg`, without Rust. It includes the source commit and a SHA-256 digest.

Start Ollama with the installed local `gemma4:e2b` model before the first run. If you have not installed it yet, `ollama pull gemma4:e2b` downloads the weights. The optional document retrieval also needs `embeddinggemma-2:270m`; install that with `ollama pull embeddinggemma-2:270m`. Choose other installed local tags with `--model` and `--embedding-model` in the Action command.

Open a finished meeting, choose **Actions → Local meeting recap (Ollama)**, then **Open folder** to read the new `assistant-summary-*.md` file in your editor. Every run creates a new report, so handwritten changes and previous results remain intact. Review the output against `transcript.md`, particularly owners, dates and commitments.

You can also test the registered Action through the app's CLI:

```bash
omarchy-meeting-recorder action "Local meeting recap (Ollama)" "$HOME/Documents/Meetings/<meeting folder>"
```

If you select **Transcribe again**, wait until the new transcript is finished before running the assistant. If the transcript or manifest changes while the assistant is working, it fails without saving a stale report. Reports are snapshots; later renaming or editing a meeting does not update them.

## Questions, notes and local documents

The helper also works on a meeting folder or its `.meeting-recorder` file:

```bash
meeting-local-assistant ask "$HOME/Documents/Meetings/<meeting folder>" \
  --question "Who agreed to review, and when?"

meeting-local-assistant expand-notes "$HOME/Documents/Meetings/<meeting folder>" \
  --notes "$HOME/Documents/notes.md"

meeting-local-assistant ask "$HOME/Documents/Meetings/<meeting folder>" \
  --question "How does the agreed date fit the release plan?" \
  --knowledge "$HOME/Documents/Project/release-plan.md"
```

`--knowledge` is opt-in and repeatable. No Documents-folder scan happens by default. Retrieval builds a bounded in-memory embedding index for each invocation, selects up to six passages, and records their exact paths and excerpts in the report. Document labels `[D1]` etc. are supplied to the model; the model may still cite incorrectly or omit citations. PDF extraction uses `pdftotext` from `poppler`; scanned PDFs need OCR first.

To make note expansion an Action, add this manually:

```toml
[[action]]
name = "Expand meeting notes locally (Ollama)"
command = '~/.local/bin/meeting-local-assistant expand-notes --notes "$MEETING_DIR/notes.md"'
```

Create `notes.md` in the meeting folder with your editor, then select that Action. Add `--knowledge` to an Action command to use a particular project folder. `auto = true` is available in the recorder, but this installer leaves the choice to you.

## Data and limits

- Model requests use only `http://127.0.0.1:11434`, bypass proxies and personal curl configuration, and do not follow redirects. Model tags and metadata are checked for remote aliases before inference. No cloud fallback, model downloads, external coding-agent calls, or audio access occur in this helper. Ollama itself and its installed models are part of the local trust boundary.
- The recorder's existing default-agent chapter generation is separate and may send transcript text to that agent's service. This Action does not change that behavior.
- The helper reads the canonical transcript and manifest without rewriting either. Outputs are new Markdown files with mode `0600`, published atomically without overwriting existing reports. They include the model name and distinguish generated text from the source transcript.
- A transcript may be up to 512,000 UTF-8 bytes. Longer transcripts are processed in parts covering every byte, followed by bounded evidence reduction. Intermediate extracts can omit details; the full original transcript remains available for review.
- Questions are limited to 1,000 bytes and rough notes to 4,000 bytes. Knowledge is limited to 16 selected paths, 128 files, 256 passages and 4 MiB of extracted text. Individual requests have a 90-second inference limit; the whole operation has an eight-minute deadline and handles SIGINT/SIGTERM. The app's existing Action executor does not provide a dedicated Cancel button.
- No persistent assistant index or second meeting-history database is created. Delete an assistant report from its meeting folder to remove that output. Original notes/documents and the recorder's files remain owned by their existing applications.

## Development and validation

```bash
cargo fmt --manifest-path examples/actions/local-assistant/Cargo.toml -- --check
cargo clippy --locked --all-targets --manifest-path examples/actions/local-assistant/Cargo.toml -- -D warnings
cargo test --locked --manifest-path examples/actions/local-assistant/Cargo.toml
cargo build --locked --release --manifest-path examples/actions/local-assistant/Cargo.toml
```

The executable integration test uses real `curl` against an HTTP fixture on port 11434, so stop any real Ollama instance while running tests. The fixture tests the Actions environment, recap/question/note commands, retrieval prefixes, complete long-transcript coverage, private output, repeat runs, model refusal, failures and concurrent transcript edits. It does not measure model quality or performance.

The `Local assistant` workflow validates and builds this independent crate on a feature-branch push; it also produces a native executable artifact. GTK, whisper.cpp and the recorder are not build dependencies of the helper. The recorder's full Arch bench remains a separate check.

## Provenance and next steps

The process supervision, local Ollama transport and document retrieval modules were adapted from the independently written MIT-licensed Omarchy Foresight prototype (source commit `fb8591639ed5e7b0fcfc8355e265739c24ab84ce`). The included [MIT license](LICENSE) retains that notice; the recorder's root license and attribution are unchanged. [Runtime dependency notices](THIRD_PARTY_NOTICES.md) accompany the native package. Google binaries, source, icons, prompts and model weights are not redistributed. Code and this documentation were developed with OpenAI Codex assistance.

This first experiment uses the existing completed-transcript Action boundary. A native assistant view and in-app setup can follow if the integration is useful. Live meeting assistance would need an incremental transcript feed and should be designed separately, considering the existing live-captions proposal.
