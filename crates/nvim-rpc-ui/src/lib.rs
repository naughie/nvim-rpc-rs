#![doc = include_str!("lib.md")]

pub use nvim_rpc::{Buffer, Tabpage, UiOptions, Window, api};

mod error;
pub use error::{Error, PresentationError, ProtocolError, RedrawError};
mod keys;
pub use keys::{AsciiKey, EncodedKey, KeyInput, Modifiers, SpecialKey};
mod model;
pub use model::{
    BufferItem, Cell, Checkpoint, CheckpointError, Chunk, Cmdline, CompletionItem, Content, Cursor,
    Damage, DefaultColors, Event, Frame, Grid, GridDamage, GridId, GridPlacement,
    GridPlacementKind, Highlight, HighlightAttributes, HighlightId, HistoryEntry, Limits, Message,
    Messages, Mode, ModeInfo, PopupAnchor, Popupmenu, Row, State, Tab, Tabline, Value, Wildmenu,
    WindowViewport,
};
mod redraw;
pub use redraw::Model;
mod ui;
pub use ui::Ui;

mod delivery;
pub use delivery::{Coalescing, Error as FrameError, Frames, UiHandler};
mod input;
pub use input::{Client, ClientError};
mod connection;
pub use connection::{
    ConnectCheckpointError, Connection, ConnectionOptions, connect, connect_with_checkpoint,
    connect_with_checkpoint_reference,
};
mod embedded;
pub use embedded::Embedded;
pub use nvim_rpc_plugin::NoHandler;

/// Presents Neovim UI frames using the application's toolkit or renderer.
///
/// Implement this trait for a toolkit adapter and pass it to [`Ui::new()`] to
/// display redraw notifications. Pass the backend to
/// [`Connection::present_next()`] or [`Embedded::present_next()`] for frame delivery
/// from an existing or embedded Neovim connection.
///
/// Call [`Self::present()`] on the thread that owns the toolkit objects. An
/// implementation may retain a frame and schedule a later paint. Process its
/// events in order, including when painting is deferred or coalesced.
///
/// See [incremental painting](crate#incremental-painting) for repaint baselines
/// and deferred drawing. Handle presentation errors according to the driver's
/// recovery contract.
pub trait Backend {
    type Error;

    /// Accepts a stable, owned frame for presentation.
    fn present(&mut self, frame: Frame) -> Result<(), Self::Error>;
}

impl<B: Backend + ?Sized> Backend for &mut B {
    type Error = B::Error;

    fn present(&mut self, frame: Frame) -> Result<(), Self::Error> {
        <B as Backend>::present(self, frame)
    }
}
