# Live assistant development preview

On `feature/local-meeting-assistant` in Tom Ballard's fork. Upstream and the
fork's `main` remain separate from this experiment.

Build the recorder normally (`cargo build --release --locked`) or use the
native tarball from the **Live assistant preview** workflow. The tarball's
`INSTALL.md` describes the existing native installation and removal process.
This is a development build, not a stable release or marketplace submission.

In the recorder, download its configured Whisper model using its existing
Download button if necessary. Enable **Live meeting assistant** before Start
recording. The recorder reads its own staging tracks in six-second steps with
a one-second overlap, using the same whisper.cpp library, model selection,
model file lookup, downmix and speech/echo gates as the final pass. Live mode
never downloads an additional speech model, starts a second audio capture, or
changes the audio files. It holds a separate Whisper context while recording;
its memory/GPU cost and latency must be measured on the target machine.

The provisional transcript keeps only `You` and `Remote` track labels. It may
duplicate boundary words, clip words or lag if inference is slower than audio.
It is not a verbatim or complete live meeting record. The language is the one
selected when recording starts. There is no cloud fallback. Six seconds is
the chunk size, not a guaranteed six-second response latency.

For answers, run local Ollama and choose an already installed local model in
the panel (`gemma4:e2b` is the initial default; another local model is allowed).
**Ask** and **Recap recent discussion** operate on a fixed snapshot of the
recent provisional transcript, bounded to 8,000 bytes at complete lines.
Earlier discussion may have fallen out of that window. Answers are generated
text, cite approximate preview timestamps, and need review. Live document
retrieval and a whole-meeting rolling summary are not implemented here. The
existing completed-meeting Action still supports full-transcript processing
and optional document retrieval.

The native panel uses the existing helper library's loopback-only transport,
cloud-alias checks, bounded output and cancellation. It never calls the
configured Omarchy coding agent. Live text and answers are kept in memory,
cleared on Stop and not saved as another transcript/history. The ordinary
final transcript is created by the existing full pass; optional upstream
chapters still use the configured coding agent's service as before.

Pause pauses live processing and cancels an outstanding answer. Stop cancels
the preview, discards late replies and joins the Whisper worker off the GTK
thread before final transcription loads a model. An unavailable model or
local assistant affects only the preview; audio recording continues.

To revert, disable the toggle before recording or reinstall the stable
upstream package. Meetings stay in the existing Documents/Meetings folders.

## Verification

Portable tests cover bounded overlapping windows, short reads, Unicode
context eviction, live prompt limits and the production local HTTP transport.
The preview workflow builds the complete GTK application and Vulkan binary,
runs its unit tests and an Xvfb GTK lifecycle test, and packages a native
tarball. A passing workflow is build/fixture evidence, not an Omarchy desktop
or XPS acceptance test.

Still needed on the XPS: real microphone/computer audio, existing Whisper
weights, pause/resume/Stop during inference, GPU memory/latency, local model
answer quality, and opening the final transcript after the live session.

The design was informed by Jankees' existing recorder and the unmerged
[live captions draft #11](https://github.com/jankeesvw/omarchy-meeting-recorder/pull/11).
This implementation reads the existing staging files rather than adding a
second capture buffer. Existing MIT attribution is preserved; helper notices
are included in the native package. Implementation was assisted by Codex.
