//! Preview from the recorder's existing staging tracks and configured model.
use std::fs::File;
use std::path::Path;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::live_buffer::{BYTES_PER_SECOND, Cursor, read_window};
use crate::transcribe::{self, Abort};

pub enum Update {
    Ready,
    Lines(Vec<String>),
    Error(String),
}

pub struct Session {
    cancel: Abort,
    paused: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl Session {
    pub fn start(
        dir: &Path,
        language: &'static str,
        tx: async_channel::Sender<Update>,
    ) -> Result<Self, String> {
        let model = crate::models::find()
            .ok_or("Download the existing speech model above before enabling live assistance.")?;
        let mut mic = File::open(dir.join("mic.raw")).map_err(|e| e.to_string())?;
        let mut computer = File::open(dir.join("system.raw")).map_err(|e| e.to_string())?;
        let cancel = Abort::default();
        let paused = Arc::new(AtomicBool::new(false));
        let (abort, pause) = (cancel.clone(), paused.clone());
        let worker = thread::spawn(move || {
            // Progress is not a live UI stream; a closed sink discards it.
            let (events, rx) = async_channel::unbounded();
            rx.close();
            let result = (|| {
                let context = transcribe::load_whisper_path(&model, &events, &abort)?;
                if abort.load(Ordering::Relaxed) {
                    return Ok(());
                }
                if tx.send_blocking(Update::Ready).is_err() {
                    return Ok(());
                }
                let mut cursor = Cursor::default();
                while !abort.load(Ordering::Relaxed) {
                    if pause.load(Ordering::Relaxed) {
                        thread::sleep(Duration::from_millis(100));
                        continue;
                    }
                    let available = mic
                        .metadata()
                        .map_err(|e| e.to_string())?
                        .len()
                        .min(computer.metadata().map_err(|e| e.to_string())?.len());
                    let Some((start, end)) = cursor.next(available) else {
                        thread::sleep(Duration::from_millis(100));
                        continue;
                    };
                    let own = transcribe::raw_to_whisper_rate(
                        &read_window(&mut mic, start, end).map_err(|e| e.to_string())?,
                    );
                    let remote = transcribe::raw_to_whisper_rate(
                        &read_window(&mut computer, start, end).map_err(|e| e.to_string())?,
                    );
                    let lines = transcribe::live_pass(&context, &own, &remote, language, &abort)?;
                    let offset_ms = (start * 1000 / BYTES_PER_SECOND) as i64;
                    let committed_ms = (cursor.end * 1000 / BYTES_PER_SECOND) as i64;
                    let lines = lines
                        .into_iter()
                        .filter(|s| s.end_ms + offset_ms > committed_ms)
                        .map(|s| {
                            let secs = (s.start_ms + offset_ms).max(0) / 1000;
                            format!(
                                "[{:02}:{:02}] {}: {}",
                                secs / 60,
                                secs % 60,
                                s.speaker,
                                s.text
                            )
                        })
                        .collect();
                    cursor.end = end;
                    if !abort.load(Ordering::Relaxed)
                        && !pause.load(Ordering::Relaxed)
                        && tx.send_blocking(Update::Lines(lines)).is_err()
                    {
                        break;
                    }
                }
                Ok::<(), String>(())
            })();
            if let Err(error) = result
                && !abort.load(Ordering::Relaxed)
            {
                let _ = tx.send_blocking(Update::Error(error));
            }
            drop(events);
        });
        Ok(Self {
            cancel,
            paused,
            worker: Some(worker),
        })
    }

    pub fn set_paused(&self, paused: bool) {
        self.paused.store(paused, Ordering::Relaxed);
    }
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
    /// Join off the GTK thread before loading the final transcription context.
    pub fn finish(mut self) {
        self.cancel();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "needs existing Whisper weights and raw speech fixtures; preview CI supplies them"]
    fn real_whisper_live_session_pauses_and_preserves_audio() {
        let dir = std::path::PathBuf::from(std::env::var_os("LIVE_TEST_DIR").unwrap());
        let model = std::env::var("LIVE_TEST_MODEL").unwrap();
        crate::models::set_override(&model);
        let before = [
            std::fs::read(dir.join("mic.raw")).unwrap(),
            std::fs::read(dir.join("system.raw")).unwrap(),
        ];
        let (tx, rx) = async_channel::bounded(1);
        let session = Session::start(&dir, "en", tx).unwrap();
        session.set_paused(true);
        let deadline = std::time::Instant::now() + Duration::from_secs(120);
        loop {
            match rx.try_recv() {
                Ok(Update::Ready) => break,
                Ok(Update::Error(error)) => panic!("{error}"),
                _ => {}
            }
            assert!(
                std::time::Instant::now() < deadline,
                "model did not become ready"
            );
            thread::sleep(Duration::from_millis(20));
        }
        thread::sleep(Duration::from_millis(250));
        assert!(
            rx.try_recv().is_err(),
            "no captions should be published while paused"
        );
        session.set_paused(false);
        let mut heard = false;
        while std::time::Instant::now() < deadline {
            match rx.try_recv() {
                Ok(Update::Lines(lines)) if !lines.is_empty() => {
                    assert!(
                        lines
                            .iter()
                            .all(|line| line.contains("You:") || line.contains("Remote:"))
                    );
                    heard = true;
                    break;
                }
                Ok(Update::Error(error)) => panic!("{error}"),
                _ => {}
            }
            thread::sleep(Duration::from_millis(20));
        }
        // Close the bounded receiver before joining: a queued publisher cannot
        // delay shutdown when there is no UI left to consume it.
        rx.close();
        session.finish();
        assert!(heard, "real Whisper produced no speech preview");
        assert_eq!(std::fs::read(dir.join("mic.raw")).unwrap(), before[0]);
        assert_eq!(std::fs::read(dir.join("system.raw")).unwrap(), before[1]);
        assert!(!dir.join("transcript.md").exists());
    }
}
