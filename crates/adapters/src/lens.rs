//! Lens markdown parsing.

use news_lens_domain::Lens;
use std::path::Path;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum LensError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("missing frontmatter field: {0}")]
    MissingField(&'static str),
}

pub fn load_lens(path: impl AsRef<Path>) -> Result<Lens, LensError> {
    let path = path.as_ref();
    let content = std::fs::read_to_string(path)?;
    let frontmatter = parse_frontmatter(&content);

    let id = frontmatter
        .id
        .ok_or(LensError::MissingField("id"))?
        .to_string();

    Ok(Lens {
        id,
        voice: frontmatter.voice.map(str::to_string),
        register: frontmatter.register.map(str::to_string),
        path: path.to_path_buf(),
        content,
    })
}

#[derive(Default)]
struct Frontmatter<'a> {
    id: Option<&'a str>,
    voice: Option<&'a str>,
    register: Option<&'a str>,
}

fn parse_frontmatter(content: &str) -> Frontmatter<'_> {
    let Some(rest) = content.strip_prefix("---") else {
        return Frontmatter::default();
    };

    let rest = rest
        .strip_prefix("\r\n")
        .or_else(|| rest.strip_prefix('\n'))
        .unwrap_or(rest);

    let mut end = None;
    let mut offset = 0;
    for line in rest.split_inclusive('\n') {
        if line.trim() == "---" {
            end = Some(offset);
            break;
        }
        offset += line.len();
    }

    let Some(end) = end else {
        return Frontmatter::default();
    };
    let frontmatter = &rest[..end];

    let mut parsed = Frontmatter::default();
    for line in frontmatter.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim().trim_matches('"').trim_matches('\'');
        match key.trim() {
            "id" => parsed.id = Some(value),
            "voice" => parsed.voice = Some(value),
            "register" => parsed.register = Some(value),
            _ => {}
        }
    }

    parsed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_lens_extracts_frontmatter_and_preserves_content() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let path = dir.path().join("lens.md");
        let content = "---\nid: test\nvoice: terse\nregister: written\n---\n\n# Policy\n";
        std::fs::write(&path, content).expect("write lens");

        let lens = load_lens(&path).expect("lens");

        assert_eq!(lens.id, "test");
        assert_eq!(lens.voice.as_deref(), Some("terse"));
        assert_eq!(lens.register.as_deref(), Some("written"));
        assert_eq!(lens.content, content);
    }

    #[test]
    fn frontmatter_values_can_contain_dashes() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let path = dir.path().join("lens.md");
        let content = "---\nid: test\nvoice: terse --- dry\nregister: written\n---\n\n# Policy\n";
        std::fs::write(&path, content).expect("write lens");

        let lens = load_lens(&path).expect("lens");

        assert_eq!(lens.id, "test");
        assert_eq!(lens.voice.as_deref(), Some("terse --- dry"));
    }
}
