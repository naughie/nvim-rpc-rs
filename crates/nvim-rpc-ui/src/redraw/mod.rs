use crate::model::*;
use crate::{Checkpoint, CheckpointError, ProtocolError, RedrawError};

mod decode;
use decode::*;
mod grid;
mod reference;
use reference::Reference;
mod text;
mod widgets;
mod windows;

use nvim_rpc::api_types::{Decode, DecodeError};
use nvim_rpc::mp::decode::{Array, Object};
use nvim_rpc::rpc::Notification;

use std::collections::BTreeMap;
use std::convert::Infallible;
use std::sync::Arc;

type Obj<'a> = Object<&'a [u8]>;
type Args<'a> = Array<&'a [u8]>;
type Result<T> = std::result::Result<T, ProtocolError>;

/// Applies Neovim redraw notifications and publishes a frame at each flush.
///
/// Use [`Ui`](crate::Ui) to present redraws through a [`Backend`](crate::Backend).
/// Use this model directly when the application supplies its own frame delivery.
///
/// Pass notifications to [`Self::apply_redraw()`] in receive order. The model
/// retains updates between notifications until Neovim sends `flush`. Published
/// frames remain valid while later notifications are processed.
///
/// If decoding or resource-limit checking fails, subsequent calls return
/// [`ProtocolError::Failed`]. Create a fresh model and UI attachment to resume.
pub struct Model {
    state: Arc<State>,
    committed: Arc<State>,
    checkpoint: Option<Checkpoint>,
    reference: Option<Reference>,
    limits: Limits,
    cell_text: text::Cache,
    damage: Damage,
    events: Vec<Event>,
    sequence: u64,
    failed: bool,
}

impl Default for Model {
    fn default() -> Self {
        Self::new(Limits::default())
    }
}

impl Model {
    /// Creates an empty model with explicit resource limits.
    pub fn new(limits: Limits) -> Self {
        Self::with_committed(limits, None)
    }

    /// Creates a model from a flushed checkpoint after checking `limits`.
    ///
    /// Use a checkpoint from the same running Neovim process. The first
    /// published frame requests a full repaint, and frame sequences start at 1.
    pub fn from_checkpoint(
        checkpoint: Checkpoint,
        limits: Limits,
    ) -> std::result::Result<Self, CheckpointError> {
        let state = checkpoint.into_state(limits)?;
        Ok(Self::with_committed(limits, Some(state)))
    }

    /// Creates a fresh model with a checkpoint as a redraw reference.
    ///
    /// Supply a checkpoint from the same running Neovim process and UI
    /// configuration. The model uses saved grid contents and highlight
    /// definitions when new events refer to them. A viewport for the same grid
    /// and window can also recover its saved placement and hidden state.
    /// Windows absent from the new stream remain outside frames. Command lines
    /// and external widgets are created by the new stream.
    ///
    /// The first redraw flush requests a full repaint.
    pub fn with_checkpoint_reference(
        checkpoint: Checkpoint,
        limits: Limits,
    ) -> std::result::Result<Self, CheckpointError> {
        let state = checkpoint.into_state(limits)?;
        let mut model = Self::new(limits);
        model.reference = Some(Reference::new(&state));
        model.damage.full = true;
        Ok(model)
    }

    /// Exports the last flushed or restored state.
    ///
    /// Returns `None` before the first `flush` of a fresh or reference model.
    /// In reference mode the checkpoint can include saved grids and placements
    /// that were absent from the frame. They remain available for future
    /// attachments within the model's limits.
    /// Pending updates and ordered events are excluded. A redraw error leaves
    /// the last flushed checkpoint available.
    pub fn checkpoint(&self) -> Option<Checkpoint> {
        self.checkpoint.clone()
    }

    pub(crate) fn grids(&self) -> &Arc<BTreeMap<GridId, Arc<Grid>>> {
        &self.state.grids
    }

    fn with_committed(limits: Limits, committed: Option<Arc<State>>) -> Self {
        let checkpoint = committed
            .as_ref()
            .map(|state| Checkpoint::from_committed(Arc::clone(state)));
        let state = committed.unwrap_or_default();
        Self {
            committed: Arc::clone(&state),
            state,
            checkpoint,
            reference: None,
            limits,
            cell_text: text::Cache::default(),
            damage: Damage::default(),
            events: Vec::new(),
            sequence: 0,
            failed: false,
        }
    }

    /// Reports whether an earlier error stopped UI updates.
    pub fn is_failed(&self) -> bool {
        self.failed
    }

    /// Applies all redraw events and calls `emit` once for each `flush` event.
    ///
    /// A notification can contain several flushes or none. Updates preceding
    /// the next flush are retained between calls. The callback receives an owned
    /// frame and can keep it after this method returns.
    ///
    /// Keep the callback short when it runs on the RPC receive task so that
    /// responses and later notifications can continue to arrive.
    pub fn apply_redraw(
        &mut self,
        notification: Notification<&[u8]>,
        mut emit: impl FnMut(Frame),
    ) -> Result<()> {
        match self.apply_redraw_with(notification, |frame, _| {
            emit(frame);
            Ok::<(), Infallible>(())
        }) {
            Ok(()) => Ok(()),
            Err(RedrawError::Protocol(e)) => Err(e),
            Err(RedrawError::Backend(e)) => match e {},
        }
    }

    pub(crate) fn apply_redraw_with<E>(
        &mut self,
        notification: Notification<&[u8]>,
        emit: impl FnMut(Frame, Option<&Checkpoint>) -> std::result::Result<(), E>,
    ) -> std::result::Result<(), RedrawError<E>> {
        if self.failed {
            return Err(ProtocolError::Failed.into());
        }
        if notification.method.as_slice() != b"redraw" {
            self.failed = true;
            return Err(DecodeError::UnexpectedType("redraw notification").into());
        }
        self.apply_notification_with(b"redraw", notification.params, emit)
            .map(|_| ())
    }

    /// Applies a UI notification and returns whether its method was handled.
    ///
    /// Handles `redraw` and `nvim_error_event`; other methods return `false`
    /// without changing the model. Pass notifications in receive order. Borrow
    /// shared parameter storage with [`Array::with_borrowed_buffer()`] when the
    /// dispatcher needs to retain it or route an unhandled notification elsewhere.
    ///
    /// Redraw frames are published at each `flush`. An API error publishes the
    /// last flushed state with all earlier pending events followed by the
    /// error event. Unflushed grid and widget changes remain pending until
    /// `flush`. Frames own all retained data. A protocol error stops UI updates.
    pub fn apply_notification(
        &mut self,
        method: &[u8],
        params: Array<&[u8]>,
        mut emit: impl FnMut(Frame),
    ) -> Result<bool> {
        match self.apply_notification_with(method, params, |frame, _| {
            emit(frame);
            Ok::<(), Infallible>(())
        }) {
            Ok(handled) => Ok(handled),
            Err(RedrawError::Protocol(e)) => Err(e),
            Err(RedrawError::Backend(e)) => match e {},
        }
    }

    pub(crate) fn apply_notification_with<E>(
        &mut self,
        method: &[u8],
        params: Args<'_>,
        mut emit: impl FnMut(Frame, Option<&Checkpoint>) -> std::result::Result<(), E>,
    ) -> std::result::Result<bool, RedrawError<E>> {
        if !matches!(method, b"redraw" | b"nvim_error_event") {
            return Ok(false);
        }
        if self.failed {
            return Err(ProtocolError::Failed.into());
        }
        let ret = if method == b"redraw" {
            self.apply(params, &mut emit)
        } else {
            self.api_error(params, &mut emit)
        };
        if ret.is_err() {
            self.failed = true;
        }
        ret.map(|()| true)
    }

    fn apply<E>(
        &mut self,
        params: Args<'_>,
        emit: &mut impl FnMut(Frame, Option<&Checkpoint>) -> std::result::Result<(), E>,
    ) -> std::result::Result<(), RedrawError<E>> {
        for group in nvim_rpc::ui::Redraw::from_params(params) {
            let mut group = group?;
            let name =
                std::str::from_utf8(group.name().get_ref()).map_err(DecodeError::InvalidUtf8)?;
            if name.len() > self.limits.max_string_bytes {
                return Err(ProtocolError::Limit("event name bytes").into());
            }
            for args in &mut group {
                let args = args?;
                if name == "flush" {
                    self.publish(true, emit)?;
                } else {
                    self.call(name, args).map_err(|e| match e {
                        ProtocolError::Invalid { reason, .. } => bad(name, reason),
                        e => e,
                    })?;
                }
            }
        }
        Ok(())
    }

    fn api_error<E>(
        &mut self,
        mut params: Args<'_>,
        emit: &mut impl FnMut(Frame, Option<&Checkpoint>) -> std::result::Result<(), E>,
    ) -> std::result::Result<(), RedrawError<E>> {
        let kind = integer(next(&mut params)?)?;
        let message = text(next(&mut params)?, self.limits)?;
        self.push_event(Event::RpcError { kind, message })?;
        self.publish(false, emit)
    }

    fn publish<E>(
        &mut self,
        flushed: bool,
        emit: &mut impl FnMut(Frame, Option<&Checkpoint>) -> std::result::Result<(), E>,
    ) -> std::result::Result<(), RedrawError<E>> {
        let first_restored = self.sequence == 0 && self.checkpoint.is_some();
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or(ProtocolError::Limit("frame sequence"))?;
        let mut damage = if flushed {
            // Reference changes also mutate live state. Reuse the combined
            // checkpoint when a flush carries only ordered events.
            if self.checkpoint.is_none() || !Arc::ptr_eq(&self.committed, &self.state) {
                self.committed = Arc::clone(&self.state);
                let state = self.reference.as_ref().map_or_else(
                    || Arc::clone(&self.committed),
                    |reference| reference.capture(&self.committed, self.limits),
                );
                self.checkpoint = Some(Checkpoint::from_committed(state));
            }
            std::mem::take(&mut self.damage)
        } else {
            Damage::default()
        };
        damage.full |= first_restored;
        emit(
            Frame {
                sequence: self.sequence,
                state: Arc::clone(&self.committed),
                damage,
                events: std::mem::take(&mut self.events),
            },
            self.checkpoint.as_ref().filter(|_| flushed),
        )
        .map_err(RedrawError::Backend)
    }

    fn call(&mut self, name: &str, mut a: Args<'_>) -> Result<()> {
        let limits = self.limits;
        match name {
            "grid_resize" | "grid_line" | "grid_clear" | "grid_destroy" | "grid_cursor_goto"
            | "grid_scroll" => self.grid_call(name, a)?,
            "default_colors_set" => {
                Arc::make_mut(&mut self.state).default_colors = DefaultColors {
                    foreground: color(next(&mut a)?)?,
                    background: color(next(&mut a)?)?,
                    special: color(next(&mut a)?)?,
                    cterm_foreground: integer(next(&mut a)?)?,
                    cterm_background: integer(next(&mut a)?)?,
                };
                self.damage.full = true;
            }
            "hl_attr_define" => {
                let id = hl_id(next(&mut a)?)?;
                let rgb = attributes(next(&mut a)?, limits)?;
                let cterm = attributes(next(&mut a)?, limits)?;
                let info = values(next(&mut a)?, limits)?;
                if let Some(reference) = &mut self.reference {
                    reference.forget_highlight(id);
                }
                if !self.state.highlights.contains_key(&id)
                    && self.state.highlights.len() >= limits.max_collection_len
                {
                    return Err(ProtocolError::Limit("highlights"));
                }
                Arc::make_mut(&mut Arc::make_mut(&mut self.state).highlights)
                    .insert(id, Highlight { rgb, cterm, info });
                self.damage.full = true;
            }
            "hl_group_set" => {
                let name = text(next(&mut a)?, limits)?;
                let id = hl_id(next(&mut a)?)?;
                if let Some(reference) = &self.reference {
                    self.damage.full |= reference.restore_highlight(
                        id,
                        &mut Arc::make_mut(&mut self.state).highlights,
                        limits,
                    )?;
                }
                let groups = Arc::make_mut(&mut Arc::make_mut(&mut self.state).highlight_groups);
                bounded_insert(groups, name, id, limits)?;
                self.damage.widgets = true;
            }
            "win_pos"
            | "win_float_pos"
            | "win_external_pos"
            | "win_hide"
            | "win_close"
            | "msg_set_pos"
            | "win_viewport"
            | "win_viewport_margins"
            | "win_extmark" => {
                self.window_call(name, a)?;
            }
            "cmdline_show"
            | "cmdline_pos"
            | "cmdline_special_char"
            | "cmdline_hide"
            | "cmdline_block_show"
            | "cmdline_block_append"
            | "cmdline_block_hide"
            | "popupmenu_show"
            | "popupmenu_select"
            | "popupmenu_hide"
            | "wildmenu_show"
            | "wildmenu_select"
            | "wildmenu_hide"
            | "tabline_update"
            | "msg_show"
            | "msg_clear"
            | "msg_showmode"
            | "msg_showcmd"
            | "msg_ruler"
            | "msg_history_show" => {
                self.widget_call(name, a)?;
            }
            "mode_info_set" => {
                let enabled = boolean(next(&mut a)?)?;
                let modes = array(next(&mut a)?)?;
                bounded_len(modes.len(), limits)?;
                let modes = modes
                    .map(|v| owned_dictionary(v, limits))
                    .collect::<Result<_>>()?;
                Arc::make_mut(&mut self.state).modes = Arc::new(modes);
                Arc::make_mut(&mut self.state).mode.cursor_style_enabled = enabled;
                self.damage.cursor = true;
            }
            "mode_change" => {
                Arc::make_mut(&mut self.state).mode.name = text(next(&mut a)?, limits)?;
                Arc::make_mut(&mut self.state).mode.index = index(next(&mut a)?)?;
                self.damage.cursor = true;
            }
            "option_set" => {
                let name = text(next(&mut a)?, limits)?;
                let value = value(next(&mut a)?, limits, 0)?;
                bounded_insert(
                    Arc::make_mut(&mut Arc::make_mut(&mut self.state).options),
                    name,
                    value,
                    limits,
                )?;
                self.damage.full = true;
            }
            "mouse_on" => Arc::make_mut(&mut self.state).mouse_enabled = true,
            "mouse_off" => Arc::make_mut(&mut self.state).mouse_enabled = false,
            "busy_start" => {
                Arc::make_mut(&mut self.state).busy = true;
                self.damage.cursor = true;
            }
            "busy_stop" => {
                Arc::make_mut(&mut self.state).busy = false;
                self.damage.cursor = true;
            }
            "set_title" | "set_icon" | "chdir" => {
                let text = text(next(&mut a)?, limits)?;
                let event = match name {
                    "set_title" => {
                        Arc::make_mut(&mut self.state).title = text.clone();
                        Event::SetTitle(text)
                    }
                    "set_icon" => {
                        Arc::make_mut(&mut self.state).icon = text.clone();
                        Event::SetIcon(text)
                    }
                    _ => {
                        Arc::make_mut(&mut self.state).cwd = text.clone();
                        Event::Chdir(text)
                    }
                };
                self.push_event(event)?;
            }
            "bell" => self.push_event(Event::Bell)?,
            "visual_bell" => self.push_event(Event::VisualBell)?,
            "suspend" => self.push_event(Event::Suspend)?,
            "update_menu" => self.push_event(Event::UpdateMenu)?,
            "connect" => {
                let text = text(next(&mut a)?, limits)?;
                self.push_event(Event::Connect(text))?;
            }
            "restart" => {
                let address = text(next(&mut a)?, limits)?;
                let channel_config = a.next().map(|v| value(v, limits, 0)).transpose()?;
                self.push_event(Event::Restart {
                    address,
                    channel_config,
                })?;
            }
            "screenshot" => {
                let text = text(next(&mut a)?, limits)?;
                self.push_event(Event::Screenshot(text))?;
            }
            "ui_send" => {
                let bytes =
                    <&[u8] as Decode<nvim_rpc::sys::types::String, _>>::decode(next(&mut a)?)?;
                if bytes.len() > limits.max_string_bytes {
                    return Err(ProtocolError::Limit("string bytes"));
                }
                self.push_event(Event::UiSend(Arc::from(bytes)))?;
            }
            "error_exit" => {
                let status = integer(next(&mut a)?)?;
                self.push_event(Event::ErrorExit(status))?;
            }
            "resize" | "clear" | "eol_clear" | "cursor_goto" | "highlight_set" | "put"
            | "set_scroll_region" | "scroll" | "update_fg" | "update_bg" | "update_sp" => {
                return Err(ProtocolError::LegacyGrid);
            }
            _ => {
                bounded_len(a.len(), limits)?;
                let args = a.map(|v| value(v, limits, 0)).collect::<Result<_>>()?;
                self.push_event(Event::Unknown {
                    name: Arc::from(name),
                    args,
                })?;
            }
        }
        Ok(())
    }

    fn push_event(&mut self, event: Event) -> Result<()> {
        if self.events.len() >= self.limits.max_pending_events {
            return Err(ProtocolError::Limit("pending events"));
        }
        self.events.push(event);
        Ok(())
    }

    fn dirty(&mut self, grid: GridId, top: usize, bottom: usize, left: usize, right: usize) {
        self.damage.record_grid(
            GridDamage {
                grid,
                top,
                bottom,
                left,
                right,
            },
            self.limits.max_grids,
        );
    }
}
