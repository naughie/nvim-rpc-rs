use super::HighlightId;

use nvim_rpc::Window;

use serde::{Deserialize, Serialize};

use std::sync::Arc;

/// A screen grid identifier, distinct from an editor window handle.
#[derive(
    Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
pub struct GridId(pub i64);
/// A screen cell's text and highlight.
///
/// An empty string denotes the continuation cell of a double-width character.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cell {
    pub text: Arc<str>,
    pub highlight: HighlightId,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            text: Arc::from(" "),
            highlight: HighlightId(0),
        }
    }
}

/// A row of screen cells and its line-wrap flag.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Row {
    pub cells: Vec<Cell>,
    /// The line continues into the next row.
    pub wrap: bool,
}

/// A rectangular screen grid, indexed by row and column in cells.
///
/// Its position and visibility are stored separately in [`super::State::placements`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Grid {
    pub width: usize,
    pub height: usize,
    pub rows: Vec<Arc<Row>>,
}

impl Grid {
    /// Borrows a cell, returning `None` outside the grid.
    pub fn cell(&self, row: usize, col: usize) -> Option<&Cell> {
        self.rows.get(row)?.cells.get(col)
    }
}

/// Neovim's cursor position in grid cell coordinates.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Cursor {
    pub grid: GridId,
    pub row: usize,
    pub col: usize,
}

/// A grid's position in the UI or in an external application window.
///
/// Coordinates and dimensions are measured in cells. Floating offsets can
/// include fractions of a cell relative to the anchor grid.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum GridPlacementKind {
    Normal {
        #[serde(with = "super::checkpoint::wire::handle")]
        window: Window,
        row: i64,
        col: i64,
        width: usize,
        height: usize,
    },
    Floating {
        #[serde(with = "super::checkpoint::wire::handle")]
        window: Window,
        anchor: Arc<str>,
        anchor_grid: GridId,
        anchor_row: f64,
        anchor_col: f64,
        mouse_enabled: bool,
        zindex: i64,
        compindex: Option<i64>,
        screen_row: Option<i64>,
        screen_col: Option<i64>,
    },
    External {
        #[serde(with = "super::checkpoint::wire::handle")]
        window: Window,
    },
    Message {
        row: i64,
        scrolled: bool,
        separator: Arc<str>,
        zindex: i64,
        compindex: Option<i64>,
    },
}

/// The buffer lines and cursor position visible in an editor window.
///
/// Buffer positions are zero-based; `curcol` counts UTF-8 bytes. Margins count
/// screen cells in top, bottom, left, right order.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WindowViewport {
    #[serde(with = "super::checkpoint::wire::handle")]
    pub window: Window,
    pub topline: i64,
    /// May be `line_count + 1` when filler lines extend past the buffer's end.
    pub botline: i64,
    pub curline: i64,
    pub curcol: i64,
    pub line_count: i64,
    /// Viewport movement in displayed lines since the preceding update.
    ///
    /// A fold counts as one displayed line. Movement exceeding a screenful
    /// can be approximate.
    pub scroll_delta: i64,
    pub margins: [usize; 4],
}
/// A grid's position and visibility.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GridPlacement {
    pub kind: GridPlacementKind,
    /// Whether the grid is hidden; its cell contents are retained.
    pub hidden: bool,
}
