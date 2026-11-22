mod checkpoint;
pub use checkpoint::{Checkpoint, CheckpointError};
mod grid;
pub use grid::{Cell, Cursor, Grid, GridId, GridPlacement, GridPlacementKind, Row, WindowViewport};
mod style;
pub use style::{DefaultColors, Highlight, HighlightAttributes, HighlightId};
mod widgets;
pub use widgets::{
    BufferItem, Chunk, Cmdline, CompletionItem, Content, HistoryEntry, Message, Messages,
    PopupAnchor, Popupmenu, Tab, Tabline, Wildmenu,
};
mod damage;
mod diff;
pub use damage::{Damage, GridDamage};
mod frame;

use nvim_rpc::Window;

use serde::{Deserialize, Serialize};

use std::collections::BTreeMap;
use std::sync::Arc;

/// A decoded value retained in UI options, highlight metadata, or events.
///
/// String values retain their original bytes, independently of UTF-8 validity.
/// Arrays and maps retain their element order; maps also retain duplicate keys.
/// Cloning a string, binary value, array, map, or extension shares its storage.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Value {
    Nil,
    Bool(bool),
    Int(i64),
    Uint(u64),
    Float(f64),
    String(#[serde(with = "checkpoint::wire::bytes")] Arc<[u8]>),
    Binary(#[serde(with = "checkpoint::wire::bytes")] Arc<[u8]>),
    Array(Arc<[Value]>),
    Map(Arc<[(Value, Value)]>),
    Extension(i8, #[serde(with = "checkpoint::wire::bytes")] Arc<[u8]>),
}

/// Resource limits used by [`Model`](crate::Model).
///
/// Exceeding a limit returns [`ProtocolError::Limit`](crate::ProtocolError::Limit).
/// These limits do not bound the combined memory of frames retained by the
/// application.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Maximum number of grids present at once.
    ///
    /// Also bounds each damage list when connection delivery uses
    /// [`crate::Coalescing::Incremental`]. Exceeding a damage-list bound requests
    /// a full repaint rather than returning a protocol error.
    pub max_grids: usize,
    /// Maximum total number of cells across all current grids.
    pub max_cells: usize,
    /// Maximum byte length of an individual string, binary value, or extension.
    pub max_string_bytes: usize,
    /// Maximum number of elements in an individual collection or metadata table.
    pub max_collection_len: usize,
    /// Maximum nesting depth of retained values.
    pub max_depth: usize,
    /// Maximum number of ordered events before publication or in a coalesced frame.
    pub max_pending_events: usize,
    /// Maximum number of visible messages retained; the oldest are evicted.
    pub max_messages: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_grids: 512,
            max_cells: 16 * 1024 * 1024,
            max_string_bytes: 16 * 1024 * 1024,
            max_collection_len: 1024 * 1024,
            max_depth: 64,
            max_pending_events: 65536,
            max_messages: 4096,
        }
    }
}

/// Cursor style properties from Neovim's `mode_info_set` event.
pub type ModeInfo = BTreeMap<Arc<str>, Value>;

/// The current editor mode and its cursor-style selection.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Mode {
    pub name: Arc<str>,
    /// Index of the current cursor style in [`State::modes`].
    pub index: usize,
    pub cursor_style_enabled: bool,
}

/// A snapshot of Neovim's grids, windows, cursor, and external UI components.
///
/// A frame's state remains unchanged when the model applies later redraws.
/// Grid coordinates count screen cells. Command-line positions count UTF-8
/// bytes. Grid contents and placements have separate lifetimes: hiding or
/// closing a window does not destroy its grid.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct State {
    pub grids: Arc<BTreeMap<GridId, Arc<Grid>>>,
    pub highlights: Arc<BTreeMap<HighlightId, Highlight>>,
    pub highlight_groups: Arc<BTreeMap<Arc<str>, HighlightId>>,
    pub default_colors: DefaultColors,
    pub placements: Arc<BTreeMap<GridId, GridPlacement>>,
    pub viewports: Arc<BTreeMap<GridId, WindowViewport>>,
    pub cursor: Option<Cursor>,
    pub cmdlines: Arc<BTreeMap<u64, Arc<Cmdline>>>,
    pub cmdline_block: Arc<Vec<Content>>,
    pub popupmenu: Option<Arc<Popupmenu>>,
    pub tabline: Option<Arc<Tabline>>,
    pub wildmenu: Option<Arc<Wildmenu>>,
    pub messages: Arc<Messages>,
    pub options: Arc<BTreeMap<Arc<str>, Value>>,
    pub modes: Arc<Vec<ModeInfo>>,
    pub mode: Mode,
    pub title: Arc<str>,
    pub icon: Arc<str>,
    pub cwd: Arc<str>,
    pub mouse_enabled: bool,
    pub busy: bool,
}

/// An application event received from Neovim.
///
/// Grid and widget changes are available through [`Frame::state`]. These
/// events include bells, window titles, and connection requests. Frames carry
/// them in receive order; handle every event even when several frames are
/// painted together. Unknown redraw events retain their name and arguments.
#[derive(Debug, Clone)]
pub enum Event {
    /// Rings the audible bell.
    Bell,
    /// Requests a visual bell.
    VisualBell,
    /// Requests suspension of the UI, where supported by the application.
    Suspend,
    /// Neovim's menu mappings changed.
    UpdateMenu,
    /// Reports an asynchronous API error from `nvim_error_event`.
    RpcError { kind: i64, message: Arc<str> },
    /// Sets the application window's title.
    SetTitle(Arc<str>),
    /// Sets the window's icon title.
    SetIcon(Arc<str>),
    /// Neovim's working directory changed.
    Chdir(Arc<str>),
    /// Requests attachment to another Neovim server at this address.
    Connect(Arc<str>),
    /// Requests attachment to a replacement server after this connection closes.
    Restart {
        address: Arc<str>,
        channel_config: Option<Value>,
    },
    /// Requests a screenshot at the supplied path.
    Screenshot(Arc<str>),
    /// Supplies uninterpreted bytes for the connected terminal from `ui_send`.
    UiSend(Arc<[u8]>),
    /// Reports Neovim's error exit status.
    ErrorExit(i64),
    /// Reports an extmark's screen position.
    Extmark {
        grid: GridId,
        window: Window,
        namespace: i64,
        mark: i64,
        row: i64,
        col: i64,
    },
    /// A redraw event without a dedicated model operation.
    Unknown { name: Arc<str>, args: Vec<Value> },
}

/// A UI state snapshot, repaint information, and ordered application events.
///
/// The frame owns its data and remains valid after the input buffer, model, or
/// connection is dropped. Uncoalesced damage is relative to the previous
/// published frame; coalesced damage covers all combined frames. Process events
/// in order even when drawing is deferred.
#[derive(Debug, Clone)]
pub struct Frame {
    pub sequence: u64,
    pub state: Arc<State>,
    pub damage: Damage,
    pub events: Vec<Event>,
}
