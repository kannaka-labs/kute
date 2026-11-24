use crate::{KuteError, Result};
use serde::{Deserialize, Serialize};
use std::fmt;

/// A vocabulary identifier such as `kute:industrial:events`.
///
/// Rules (KVF 0.1): two to eight segments separated by `:`; each segment is
/// 1 to 64 characters from `a-z 0-9 . _ -` and starts with a letter or digit;
/// the whole identifier is at most 255 bytes. Token ids only mean something
/// together with one of these and a version (FR-005).
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct VocabularyId(String);

impl VocabularyId {
    pub const MAX_LEN: usize = 255;

    pub fn parse(s: &str) -> Result<Self> {
        let bad = |why: &str| KuteError::InvalidIdentifier(format!("{s:?}: {why}"));
        if s.is_empty() {
            return Err(bad("empty"));
        }
        if s.len() > Self::MAX_LEN {
            return Err(bad("longer than 255 bytes"));
        }
        let segments: Vec<&str> = s.split(':').collect();
        if segments.len() < 2 || segments.len() > 8 {
            return Err(bad("needs 2 to 8 ':'-separated segments, e.g. kute:industrial:events"));
        }
        for seg in segments {
            if seg.is_empty() || seg.len() > 64 {
                return Err(bad("each segment must be 1 to 64 characters"));
            }
            let mut chars = seg.chars();
            let first = chars.next().unwrap_or(' ');
            if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
                return Err(bad("each segment must start with a-z or 0-9"));
            }
            if !chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-')) {
                return Err(bad("segments may only contain a-z 0-9 . _ -"));
            }
        }
        Ok(Self(s.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The first segment, e.g. `kute`.
    pub fn namespace(&self) -> &str {
        self.0.split(':').next().unwrap_or("")
    }
}

impl fmt::Display for VocabularyId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for VocabularyId {
    type Error = KuteError;
    fn try_from(s: String) -> Result<Self> {
        Self::parse(&s)
    }
}

impl From<VocabularyId> for String {
    fn from(v: VocabularyId) -> String {
        v.0
    }
}

impl std::str::FromStr for VocabularyId {
    type Err = KuteError;
    fn from_str(s: &str) -> Result<Self> {
        Self::parse(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_prd_examples() {
        for ok in ["kute:industrial:events", "kannaka:hrm", "language:english", "a:b:c:d:e:f:g:h", "kute:v1.2_x-y"] {
            assert!(VocabularyId::parse(ok).is_ok(), "{ok}");
        }
    }

    #[test]
    fn rejects_malformed() {
        let long = format!("kute:{}", "a".repeat(65));
        for bad in ["", "kute", "kute:", ":kute", "Kute:x", "kute:Événements", "kute:a b", "kute:-x", "a:b:c:d:e:f:g:h:i", "kute:../x", long.as_str()] {
            assert!(VocabularyId::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn serde_validates() {
        let ok: VocabularyId = serde_json::from_str("\"kute:test\"").unwrap();
        assert_eq!(ok.as_str(), "kute:test");
        assert!(serde_json::from_str::<VocabularyId>("\"NOPE\"").is_err());
    }
}
