use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    Bbo,
    Ticker,
    L2Book,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataQuality {
    #[serde(default)]
    pub gap: bool,
    #[serde(default)]
    pub stale: bool,
    #[serde(default)]
    pub inconsistent: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl DataQuality {
    pub fn add_note(&mut self, note: impl Into<String>) {
        let note = note.into();
        self.note = match self.note.take() {
            Some(existing) => Some(format!("{existing}; {note}")),
            None => Some(note),
        };
    }
}
