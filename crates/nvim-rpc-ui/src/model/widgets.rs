use super::{GridId, HighlightId, Value};

use nvim_rpc::{Buffer, Tabpage};

use serde::{Deserialize, Serialize};

use std::sync::Arc;

/// A highlighted UTF-8 text chunk in a command line or message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Chunk {
    pub highlight: HighlightId,
    pub text: Arc<str>,
    /// Optional `hl_id` metadata supplied with the chunk.
    pub hl_id: Option<i64>,
}

/// An ordered sequence of highlighted text chunks.
pub type Content = Vec<Chunk>;

/// A command line at one nesting level.
///
/// `pos` is a UTF-8 byte offset into the concatenated content. Multiple command
/// lines can be active when Neovim prompts for input during command evaluation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cmdline {
    pub content: Content,
    pub pos: usize,
    pub firstc: Arc<str>,
    pub prompt: Arc<str>,
    pub indent: usize,
    pub level: u64,
    pub hl_id: Option<i64>,
    pub special: Option<(Arc<str>, bool)>,
}

/// The completion popup's position in a grid or external command line.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum PopupAnchor {
    Grid { grid: GridId, row: i64, col: i64 },
    Cmdline { byte_offset: usize },
}
/// A completion popup entry with its display word and descriptive text.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionItem {
    pub word: Arc<str>,
    pub kind: Arc<str>,
    pub menu: Arc<str>,
    pub info: Arc<str>,
}
/// The completion popup's entries, selection, and position.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Popupmenu {
    pub items: Vec<CompletionItem>,
    /// The selected entry, or `None` when no entry is selected.
    pub selected: Option<usize>,
    pub anchor: PopupAnchor,
}
/// A tabpage and its display name in the tabline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tab {
    #[serde(with = "super::checkpoint::wire::handle")]
    pub tab: Tabpage,
    pub name: Arc<str>,
}
/// A buffer and its display name in the tabline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BufferItem {
    #[serde(with = "super::checkpoint::wire::handle")]
    pub buffer: Buffer,
    pub name: Arc<str>,
}
/// Tabline entries and the current tabpage and buffer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tabline {
    #[serde(with = "super::checkpoint::wire::handle")]
    pub current: Tabpage,
    pub tabs: Vec<Tab>,
    #[serde(with = "super::checkpoint::wire::optional_handle")]
    pub current_buffer: Option<Buffer>,
    pub buffers: Vec<BufferItem>,
}
/// Completion entries and selection from the legacy `ext_wildmenu` extension.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Wildmenu {
    pub items: Vec<Arc<str>>,
    pub selected: Option<usize>,
}
/// A message displayed by Neovim's external message UI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub kind: Arc<str>,
    pub content: Content,
    pub history: bool,
    pub id: Value,
    pub trigger: Arc<str>,
}
/// An entry displayed by the `:messages` command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub kind: Arc<str>,
    pub content: Content,
    pub append: bool,
}
/// Visible messages, mode and command displays, and message history.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Messages {
    pub visible: Vec<Message>,
    pub showmode: Content,
    /// Command-key display from `msg_showcmd`.
    ///
    /// Valid UTF-8 is preserved. Each invalid byte is displayed as `\xNN` with
    /// lowercase hexadecimal digits. The expanded text in each chunk must fit
    /// [`Limits::max_string_bytes`](super::Limits::max_string_bytes).
    pub showcmd: Content,
    pub ruler: Content,
    pub history: Option<Vec<HistoryEntry>>,
    pub history_prev_cmd: bool,
}
