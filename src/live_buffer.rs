//! Bounded live windows on the recording's own timeline; no extra capture.
use std::collections::VecDeque;
use std::io::{Read, Seek, SeekFrom};

pub const BYTES_PER_SECOND: u64 = 48_000 * 2 * 2;
const HOP: u64 = 6 * BYTES_PER_SECOND;
const OVERLAP: u64 = BYTES_PER_SECOND;
pub const CONTEXT_BYTES: usize = 8_000;

#[derive(Default)]
pub struct Cursor {
    pub end: u64,
}

impl Cursor {
    pub fn next(&self, available: u64) -> Option<(u64, u64)> {
        (available.saturating_sub(self.end) >= HOP)
            .then(|| (self.end.saturating_sub(OVERLAP), self.end + HOP))
    }
}

pub fn read_window(
    reader: &mut (impl Read + Seek),
    start: u64,
    end: u64,
) -> std::io::Result<Vec<u8>> {
    reader.seek(SeekFrom::Start(start))?;
    let mut bytes = vec![0; (end - start) as usize];
    reader.read_exact(&mut bytes)?;
    Ok(bytes)
}

#[derive(Default)]
pub struct Context {
    lines: VecDeque<String>,
    bytes: usize,
}

impl Context {
    pub fn push(&mut self, line: String) {
        self.bytes += line.len() + 1;
        self.lines.push_back(line);
        while self.bytes > CONTEXT_BYTES {
            if let Some(line) = self.lines.pop_front() {
                self.bytes -= line.len() + 1;
            }
        }
    }

    pub fn text(&self) -> String {
        self.lines.iter().cloned().collect::<Vec<_>>().join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_overlap_without_skipping_slow_processing() {
        let mut cursor = Cursor::default();
        assert_eq!(cursor.next(HOP - 4), None);
        assert_eq!(cursor.next(HOP), Some((0, HOP)));
        cursor.end = HOP;
        assert_eq!(cursor.next(HOP * 20), Some((HOP - OVERLAP, HOP * 2)));
    }

    #[test]
    fn partial_frames_wait_and_reads_never_include_future_audio() {
        let mut reader = std::io::Cursor::new(vec![1, 2, 3, 4, 5, 6]);
        assert_eq!(read_window(&mut reader, 2, 4).unwrap(), [3, 4]);
        assert!(read_window(&mut reader, 2, 8).is_err());
    }

    #[test]
    fn context_is_bounded_on_complete_unicode_lines() {
        let mut context = Context::default();
        context.push("old".into());
        for _ in 0..400 {
            context.push("日本語".repeat(20));
        }
        let text = context.text();
        assert!(text.len() <= CONTEXT_BYTES);
        assert!(!text.contains("old"));
        assert!(text.contains("日本語"));
    }
}
