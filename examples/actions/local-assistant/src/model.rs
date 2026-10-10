#[derive(Clone, Debug)]
pub struct Config {
    pub embedding_model: String,
}

#[derive(Clone, Debug)]
pub struct Chunk {
    pub title: String,
    pub path: String,
    pub text: String,
    pub embedding: Vec<f32>,
}

#[derive(Clone, Debug)]
pub struct Source {
    pub title: String,
    pub path: String,
    pub excerpt: String,
}

#[derive(Debug)]
pub struct ApiError {
    pub code: String,
    pub message: String,
}

impl ApiError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: truncate(&message.into(), 2000),
        }
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for ApiError {}

pub fn validate_model_name(name: &str) -> Result<(), ApiError> {
    if name.is_empty()
        || name.len() > 120
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-:/".contains(&b))
    {
        return Err(ApiError::new(
            "INVALID_MODEL",
            "Use an installed local Ollama model name.",
        ));
    }
    if name.to_ascii_lowercase().contains("cloud") || name.contains("://") {
        return Err(ApiError::new(
            "REMOTE_MODEL",
            "Cloud models are disabled. Choose installed local weights.",
        ));
    }
    Ok(())
}

pub fn truncate(text: &str, limit: usize) -> String {
    let mut end = text.len().min(limit);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

pub fn clean(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\r' | '\t'))
        .collect::<String>()
        .trim()
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_cloud_endpoints_and_control_characters() {
        for name in ["x:cloud", "https://example.com/model", "x\nfoo", ""] {
            assert!(validate_model_name(name).is_err());
        }
        assert!(validate_model_name("gemma4:e2b").is_ok());
    }
    #[test]
    fn truncation_preserves_unicode() {
        assert_eq!(truncate("a😀bc", 4), "a");
        assert_eq!(truncate("a😀bc", 5), "a😀");
    }
}
