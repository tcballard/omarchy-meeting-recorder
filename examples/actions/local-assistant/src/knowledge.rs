use crate::model::{clean, truncate, ApiError, Chunk, Config, Source};
use crate::ollama;
use crate::process::{run, Control};
use crate::storage::read_limited;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

const MAX_FILES: usize = 128;
const MAX_CHUNKS: usize = 256;

pub fn index(paths: &[String], config: &Config, control: &Control) -> Result<Vec<Chunk>, ApiError> {
    if paths.is_empty() || paths.len() > 16 {
        return Err(ApiError::new(
            "KNOWLEDGE_PATHS",
            "Choose between one and 16 local text, Markdown, PDF, or folder paths.",
        ));
    }
    ollama::verify_local(&config.embedding_model, control)?;
    let mut files = Vec::new();
    let mut visited = 0;
    for path in paths {
        if path.len() > 4096 || path.chars().any(char::is_control) || !Path::new(path).is_absolute()
        {
            return Err(ApiError::new(
                "KNOWLEDGE_PATHS",
                "Knowledge paths must be absolute local paths without control characters.",
            ));
        }
        let path = fs::canonicalize(path)
            .map_err(|e| ApiError::new("KNOWLEDGE_PATHS", format!("Could not open {path}: {e}")))?;
        collect_files(&path, 0, &mut visited, &mut files, control)?;
    }
    files.sort();
    files.dedup();
    if files.is_empty() {
        return Err(ApiError::new(
            "KNOWLEDGE_EMPTY",
            "No readable .txt, .md, .markdown, or .pdf documents were found.",
        ));
    }
    let mut chunks = Vec::new();
    let mut total = 0;
    for path in files {
        control.check()?;
        let text = read_document(&path, control)?;
        total += text.len();
        if total > 4_000_000 {
            return Err(ApiError::new(
                "KNOWLEDGE_LIMIT",
                "The selected documents exceed 4 MiB of extracted text. Index a smaller folder.",
            ));
        }
        let title = truncate(
            path.file_stem().and_then(|p| p.to_str()).unwrap_or("none"),
            256,
        );
        for text in chunk_text(&text) {
            chunks.push(Chunk {
                title: title.clone(),
                path: path.to_string_lossy().into(),
                text,
                embedding: Vec::new(),
            });
            if chunks.len() > MAX_CHUNKS {
                return Err(ApiError::new("KNOWLEDGE_LIMIT", "The knowledge base exceeds 256 passages. Choose fewer documents or a smaller folder."));
            }
        }
    }
    if chunks.is_empty() {
        return Err(ApiError::new("KNOWLEDGE_EMPTY", "The selected documents contain no extractable text. Scanned PDFs need OCR before indexing."));
    }
    for batch in chunks.chunks_mut(16) {
        control.check()?;
        // EmbeddingGemma document prefix: no task prefix for documents.
        let input: Vec<_> = batch
            .iter()
            .map(|c| format!("title: {} | text: {}", c.title, c.text))
            .collect();
        let embeddings = ollama::embed(&config.embedding_model, &input, control)?;
        for (chunk, embedding) in batch.iter_mut().zip(embeddings) {
            chunk.embedding = embedding;
        }
    }
    Ok(chunks)
}

fn collect_files(
    path: &Path,
    depth: usize,
    visited: &mut usize,
    out: &mut Vec<PathBuf>,
    control: &Control,
) -> Result<(), ApiError> {
    control.check()?;
    if path.to_string_lossy().len() > 4096 || path.to_string_lossy().chars().any(char::is_control) {
        return Err(ApiError::new(
            "KNOWLEDGE_PATHS",
            "A document path is too long or contains control characters.",
        ));
    }
    *visited += 1;
    if *visited > 4096 || depth > 8 {
        return Err(ApiError::new(
            "KNOWLEDGE_LIMIT",
            "The folder is too large or too deeply nested. Select a smaller subfolder.",
        ));
    }
    let metadata = fs::symlink_metadata(path).map_err(|e| {
        ApiError::new(
            "KNOWLEDGE_PATHS",
            format!("Could not read {}: {e}", path.display()),
        )
    })?;
    if metadata.file_type().is_symlink() {
        return Ok(());
    }
    if metadata.is_dir() {
        let mut entries: Vec<_> = fs::read_dir(path)
            .map_err(|e| ApiError::new("KNOWLEDGE_PATHS", e.to_string()))?
            .collect::<Result<_, _>>()
            .map_err(|e| ApiError::new("KNOWLEDGE_PATHS", e.to_string()))?;
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            if entry.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            collect_files(&entry.path(), depth + 1, visited, out, control)?;
        }
    } else if metadata.is_file() && supported(path) {
        out.push(path.to_owned());
        if out.len() > MAX_FILES {
            return Err(ApiError::new(
                "KNOWLEDGE_LIMIT",
                "Choose at most 128 knowledge files.",
            ));
        }
    } else if depth == 0 {
        return Err(ApiError::new(
            "KNOWLEDGE_FORMAT",
            "Knowledge documents must be .txt, .md, .markdown, or .pdf files.",
        ));
    }
    Ok(())
}

pub fn supported(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|s| s.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("txt" | "md" | "markdown" | "pdf")
    )
}

pub fn read_document(path: &Path, control: &Control) -> Result<String, ApiError> {
    if !supported(path) {
        return Err(ApiError::new(
            "KNOWLEDGE_FORMAT",
            "Choose a local text, Markdown, or PDF document.",
        ));
    }
    let bytes = if path
        .extension()
        .is_some_and(|s| s.to_string_lossy().eq_ignore_ascii_case("pdf"))
    {
        // Preflight before handing a potentially large PDF to pdftotext.
        read_limited(path, 16_000_000)?;
        run(
            "pdftotext",
            &[
                "-layout",
                path.to_str().ok_or_else(|| {
                    ApiError::new("FILE_PATH", "The document path must be valid UTF-8.")
                })?,
                "-",
            ],
            None,
            256_000,
            Duration::from_secs(30),
            control,
        )?
        .stdout
    } else {
        read_limited(path, 256_000)?
    };
    if bytes.contains(&0) {
        return Err(ApiError::new(
            "KNOWLEDGE_FORMAT",
            "A selected document contains binary data rather than plain text.",
        ));
    }
    let text = String::from_utf8(bytes).map_err(|_| {
        ApiError::new(
            "KNOWLEDGE_ENCODING",
            "Text documents must use UTF-8 encoding.",
        )
    })?;
    Ok(clean(&text))
}

pub fn chunk_text(text: &str) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < text.len() {
        let mut end = (start + 1200).min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        if end < text.len() {
            if let Some(space) = text[start..end].rfind(char::is_whitespace) {
                if space > 800 {
                    end = start + space;
                }
            }
        }
        let part = text[start..end].trim();
        if !part.is_empty() {
            chunks.push(part.to_owned());
        }
        if end >= text.len() {
            break;
        }
        let mut next = end.saturating_sub(180).max(start + 1);
        while !text.is_char_boundary(next) {
            next += 1;
        }
        start = next;
    }
    chunks
}

pub fn retrieve(
    index: &[Chunk],
    index_model: &str,
    question: &str,
    config: &Config,
    control: &Control,
) -> Result<Vec<Source>, ApiError> {
    if index.is_empty() {
        return Ok(Vec::new());
    }
    if index_model != config.embedding_model {
        return Err(ApiError::new(
            "INDEX_MODEL_CHANGED",
            "Re-index local knowledge after changing the embedding model.",
        ));
    }
    ollama::verify_local(&config.embedding_model, control)?;
    // Google recommends this task prefix for retrieval used to answer a question.
    let input = vec![format!("task: question answering | query: {question}")];
    let embedding = ollama::embed(&config.embedding_model, &input, control)?.remove(0);
    let mut scored = Vec::new();
    for chunk in index {
        control.check()?;
        let score = cosine(&embedding, &chunk.embedding).ok_or_else(|| {
            ApiError::new(
                "INDEX_DIMENSION",
                "Knowledge embeddings do not match this model. Re-index the documents.",
            )
        })?;
        scored.push((score, chunk));
    }
    scored.sort_by(|a, b| b.0.total_cmp(&a.0));
    Ok(scored
        .into_iter()
        .take(6)
        .map(|(_, c)| Source {
            title: c.title.clone(),
            path: c.path.clone(),
            excerpt: c.text.clone(),
        })
        .collect())
}

pub fn cosine(a: &[f32], b: &[f32]) -> Option<f64> {
    if a.len() != b.len() || a.is_empty() {
        return None;
    }
    let (mut dot, mut aa, mut bb) = (0.0, 0.0, 0.0);
    for (&a, &b) in a.iter().zip(b) {
        if !a.is_finite() || !b.is_finite() {
            return None;
        }
        let a = a as f64;
        let b = b as f64;
        dot += a * b;
        aa += a * a;
        bb += b * b;
    }
    if aa == 0.0 || bb == 0.0 {
        return Some(0.0);
    }
    Some(dot / (aa.sqrt() * bb.sqrt()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn chunks_preserve_unicode_with_overlap() {
        let text = "😀 useful sentence. ".repeat(150);
        let chunks = chunk_text(&text);
        assert!(chunks.len() > 2);
        assert!(chunks.iter().all(|c| c.len() <= 1200 && !c.is_empty()));
        assert!(chunks.iter().all(|c| c.is_char_boundary(c.len())));
    }
    #[test]
    fn semantic_cosine_orders_matches_and_rejects_dimension_errors() {
        assert!((cosine(&[1., 0.], &[1., 0.]).unwrap() - 1.).abs() < 1e-9);
        assert_eq!(cosine(&[1., 0.], &[0., 1.]), Some(0.));
        assert!(cosine(&[1.], &[1., 0.]).is_none());
        assert!(cosine(&[f32::NAN], &[1.]).is_none());
    }
    #[test]
    fn recursive_scan_never_follows_directory_links() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.md"), "hello").unwrap();
        std::os::unix::fs::symlink(dir.path(), dir.path().join("loop")).unwrap();
        let c = Control::new(
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            Duration::from_secs(2),
        );
        let mut files = Vec::new();
        collect_files(dir.path(), 0, &mut 0, &mut files, &c).unwrap();
        assert_eq!(files.len(), 1);
    }
}
