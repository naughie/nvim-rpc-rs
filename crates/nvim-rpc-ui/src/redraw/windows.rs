use super::decode::*;
use super::reference::placement_window;
use super::{Args, Model, Result};

use crate::model::*;

use std::sync::Arc;

impl Model {
    pub(super) fn window_call(&mut self, name: &str, mut a: Args<'_>) -> Result<()> {
        let limits = self.limits;
        match name {
            "win_pos" => {
                let id = grid_id(next(&mut a)?)?;
                let window = window(next(&mut a)?)?;
                let row = integer(next(&mut a)?)?;
                let col = integer(next(&mut a)?)?;
                let width = size(next(&mut a)?)?;
                let height = size(next(&mut a)?)?;
                self.placement(
                    id,
                    GridPlacementKind::Normal {
                        window,
                        row,
                        col,
                        width,
                        height,
                    },
                )?;
            }
            "win_float_pos" => {
                let id = grid_id(next(&mut a)?)?;
                let window = window(next(&mut a)?)?;
                let anchor = text(next(&mut a)?, limits)?;
                let anchor_grid = grid_id(next(&mut a)?)?;
                let anchor_row = float(next(&mut a)?)?;
                let anchor_col = float(next(&mut a)?)?;
                let mouse_enabled = boolean(next(&mut a)?)?;
                let zindex = integer(next(&mut a)?)?;
                let compindex = a.next().map(integer).transpose()?;
                let screen_row = a.next().map(integer).transpose()?;
                let screen_col = a.next().map(integer).transpose()?;
                self.placement(
                    id,
                    GridPlacementKind::Floating {
                        window,
                        anchor,
                        anchor_grid,
                        anchor_row,
                        anchor_col,
                        mouse_enabled,
                        zindex,
                        compindex,
                        screen_row,
                        screen_col,
                    },
                )?;
            }
            "win_external_pos" => {
                let id = grid_id(next(&mut a)?)?;
                let window = window(next(&mut a)?)?;
                self.placement(id, GridPlacementKind::External { window })?;
            }
            "win_hide" => {
                let id = grid_id(next(&mut a)?)?;
                if let Some(reference) = &mut self.reference {
                    reference.hide_placement(id);
                }
                if let Some(p) =
                    Arc::make_mut(&mut Arc::make_mut(&mut self.state).placements).get_mut(&id)
                {
                    p.hidden = true;
                }
                self.damage.layout = true;
            }
            "win_close" => {
                let id = grid_id(next(&mut a)?)?;
                if let Some(reference) = &mut self.reference {
                    reference.take_placement(id);
                    reference.take_viewport(id);
                }
                Arc::make_mut(&mut Arc::make_mut(&mut self.state).placements).remove(&id);
                Arc::make_mut(&mut Arc::make_mut(&mut self.state).viewports).remove(&id);
                self.damage.layout = true;
            }
            "msg_set_pos" => {
                let id = grid_id(next(&mut a)?)?;
                let row = integer(next(&mut a)?)?;
                let scrolled = boolean(next(&mut a)?)?;
                let separator = text(next(&mut a)?, limits)?;
                let zindex = a.next().map(integer).transpose()?.unwrap_or(200);
                let compindex = a.next().map(integer).transpose()?;
                self.placement(
                    id,
                    GridPlacementKind::Message {
                        row,
                        scrolled,
                        separator,
                        zindex,
                        compindex,
                    },
                )?;
            }
            "win_viewport" => {
                let id = grid_id(next(&mut a)?)?;
                let window = window(next(&mut a)?)?;
                let saved = self
                    .reference
                    .as_mut()
                    .and_then(|reference| reference.take_viewport(id));
                let margins = self
                    .state
                    .viewports
                    .get(&id)
                    .or_else(|| saved.as_ref().filter(|viewport| viewport.window == window))
                    .map_or([0; 4], |viewport| viewport.margins);
                let viewport = WindowViewport {
                    window,
                    topline: integer(next(&mut a)?)?,
                    botline: integer(next(&mut a)?)?,
                    curline: integer(next(&mut a)?)?,
                    curcol: integer(next(&mut a)?)?,
                    line_count: integer(next(&mut a)?)?,
                    scroll_delta: a.next().map(integer).transpose()?.unwrap_or(0),
                    margins,
                };
                bounded_insert(
                    Arc::make_mut(&mut Arc::make_mut(&mut self.state).viewports),
                    id,
                    viewport,
                    limits,
                )?;

                // A viewport identifies the window, including when it is hidden.
                // Recover the old placement only while it still matches this window.
                if let Some(reference) = &mut self.reference
                    && let Some(placement) = reference.take_placement(id)
                    && placement_window(&placement) == Some(window)
                    && !self.state.placements.contains_key(&id)
                {
                    self.restore_grid(id)?;
                    self.set_placement(id, placement)?;
                }
                self.damage.layout = true;
            }
            "win_viewport_margins" => {
                let id = grid_id(next(&mut a)?)?;
                let window = window(next(&mut a)?)?;
                let margins = [
                    index(next(&mut a)?)?,
                    index(next(&mut a)?)?,
                    index(next(&mut a)?)?,
                    index(next(&mut a)?)?,
                ];
                if let Some(reference) = &mut self.reference {
                    reference.take_viewport(id);
                }
                if !self.state.viewports.contains_key(&id) {
                    bounded_insert(
                        Arc::make_mut(&mut Arc::make_mut(&mut self.state).viewports),
                        id,
                        WindowViewport {
                            window,
                            ..WindowViewport::default()
                        },
                        limits,
                    )?;
                }
                Arc::make_mut(&mut Arc::make_mut(&mut self.state).viewports)
                    .get_mut(&id)
                    .ok_or_else(|| bad(name, "missing viewport"))?
                    .margins = margins;
                self.damage.layout = true;
            }
            "win_extmark" => {
                let event = Event::Extmark {
                    grid: grid_id(next(&mut a)?)?,
                    window: window(next(&mut a)?)?,
                    namespace: integer(next(&mut a)?)?,
                    mark: integer(next(&mut a)?)?,
                    row: integer(next(&mut a)?)?,
                    col: integer(next(&mut a)?)?,
                };
                self.push_event(event)?;
            }
            _ => return invalid(name, "unsupported window event"),
        }
        Ok(())
    }

    fn placement(&mut self, id: GridId, kind: GridPlacementKind) -> Result<()> {
        if let Some(reference) = &mut self.reference {
            reference.take_placement(id);
        }
        self.restore_grid(id)?;
        self.set_placement(
            id,
            GridPlacement {
                kind,
                hidden: false,
            },
        )
    }

    fn set_placement(&mut self, id: GridId, placement: GridPlacement) -> Result<()> {
        bounded_insert(
            Arc::make_mut(&mut Arc::make_mut(&mut self.state).placements),
            id,
            placement,
            self.limits,
        )?;
        self.damage.layout = true;
        Ok(())
    }
}
