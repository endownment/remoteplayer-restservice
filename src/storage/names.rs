use std::fmt;

const MAX_NAME_BYTES: usize = 255;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NameError {
    #[error("name must not be empty")]
    Empty,
    #[error("name is longer than {MAX_NAME_BYTES} bytes")]
    TooLong,
    #[error("name must not start with '.'")]
    Hidden,
    #[error("name contains a forbidden character {0:?}")]
    ForbiddenCharacter(char),
}

/// A single path component that is safe to join under the upload root:
/// no separators, no `..`, no hidden or control characters.
///
/// The "newtype" pattern: once you hold an `EntryName`, it has been validated —
/// the type system remembers that, so functions taking `&EntryName` never
/// re-check (similar to a value object with a private constructor).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EntryName(String);

impl EntryName {
    pub fn parse(raw: &str) -> Result<EntryName, NameError> {
        if raw.is_empty() {
            return Err(NameError::Empty);
        }
        if raw.len() > MAX_NAME_BYTES {
            return Err(NameError::TooLong);
        }
        if raw.starts_with('.') {
            return Err(NameError::Hidden);
        }
        let forbidden: Option<char> =
            raw.chars().find(|character: &char| *character == '/' || *character == '\\' || character.is_control());
        if let Some(character) = forbidden {
            return Err(NameError::ForbiddenCharacter(character));
        }
        Ok(EntryName(raw.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for EntryName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_names_with_spaces_and_unicode_are_accepted() {
        assert!(EntryName::parse("Été à Zürich (2024) 1080p.mkv").is_ok());
    }

    #[test]
    fn traversal_and_hidden_names_are_rejected() {
        assert_eq!(EntryName::parse(".."), Err(NameError::Hidden));
        assert_eq!(EntryName::parse(".part"), Err(NameError::Hidden));
        assert_eq!(EntryName::parse("a/b"), Err(NameError::ForbiddenCharacter('/')));
        assert_eq!(EntryName::parse("a\u{0}b"), Err(NameError::ForbiddenCharacter('\u{0}')));
        assert_eq!(EntryName::parse(""), Err(NameError::Empty));
    }
}
