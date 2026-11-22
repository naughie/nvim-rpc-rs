use super::Value;

use serde::{Deserialize, Serialize};

use std::collections::BTreeMap;
use std::sync::Arc;

/// An index into the UI highlight table.
#[derive(
    Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
pub struct HighlightId(pub i64);
/// Highlight colors and text styles. Absent colors use the current defaults.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HighlightAttributes {
    pub foreground: Option<u32>,
    pub background: Option<u32>,
    pub special: Option<u32>,
    pub reverse: bool,
    pub italic: bool,
    pub bold: bool,
    pub strikethrough: bool,
    pub underline: bool,
    pub undercurl: bool,
    pub underdouble: bool,
    pub underdotted: bool,
    pub underdashed: bool,
    pub altfont: bool,
    pub dim: bool,
    pub blink: bool,
    pub conceal: bool,
    pub overline: bool,
    pub blend: Option<u8>,
    pub url: Option<Arc<str>>,
    pub extra: BTreeMap<Arc<str>, Value>,
}

/// RGB and terminal styles, plus optional semantic highlight information.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Highlight {
    pub rgb: HighlightAttributes,
    pub cterm: HighlightAttributes,
    pub info: Vec<Value>,
}

/// Default colors; `None` delegates a color to the host's defaults.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct DefaultColors {
    pub foreground: Option<u32>,
    pub background: Option<u32>,
    pub special: Option<u32>,
    pub cterm_foreground: i64,
    pub cterm_background: i64,
}
