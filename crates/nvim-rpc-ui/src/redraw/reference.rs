use super::Result;
use super::decode::bounded_insert;

use crate::model::{
    Grid, GridId, GridPlacement, GridPlacementKind, Highlight, HighlightId, Limits, State,
    WindowViewport,
};

use nvim_rpc::Window;

use std::collections::BTreeMap;
use std::sync::Arc;

/// Retains definitions that the new redraw stream may still need.
pub(super) struct Reference {
    grids: Arc<BTreeMap<GridId, Arc<Grid>>>,
    placements: Arc<BTreeMap<GridId, GridPlacement>>,
    pub(super) highlights: Arc<BTreeMap<HighlightId, Highlight>>,
    viewports: Arc<BTreeMap<GridId, WindowViewport>>,
}

impl Reference {
    pub(super) fn new(state: &State) -> Self {
        Self {
            grids: Arc::clone(&state.grids),
            placements: Arc::clone(&state.placements),
            highlights: Arc::clone(&state.highlights),
            viewports: Arc::clone(&state.viewports),
        }
    }

    pub(super) fn take_grid(&mut self, id: GridId) -> Option<Arc<Grid>> {
        self.grids.get(&id)?;
        Arc::make_mut(&mut self.grids).remove(&id)
    }

    pub(super) fn take_placement(&mut self, id: GridId) -> Option<GridPlacement> {
        self.placements.get(&id)?;
        Arc::make_mut(&mut self.placements).remove(&id)
    }

    pub(super) fn take_viewport(&mut self, id: GridId) -> Option<WindowViewport> {
        self.viewports.get(&id)?;
        Arc::make_mut(&mut self.viewports).remove(&id)
    }

    pub(super) fn hide_placement(&mut self, id: GridId) {
        if self.placements.get(&id).is_some_and(|p| !p.hidden)
            && let Some(placement) = Arc::make_mut(&mut self.placements).get_mut(&id)
        {
            placement.hidden = true;
        }
    }

    pub(super) fn forget_highlight(&mut self, id: HighlightId) {
        if self.highlights.contains_key(&id) {
            Arc::make_mut(&mut self.highlights).remove(&id);
        }
    }

    pub(super) fn restore_highlight(
        &self,
        id: HighlightId,
        highlights: &mut Arc<BTreeMap<HighlightId, Highlight>>,
        limits: Limits,
    ) -> Result<bool> {
        if !highlights.contains_key(&id)
            && let Some(highlight) = self.highlights.get(&id)
        {
            bounded_insert(Arc::make_mut(highlights), id, highlight.clone(), limits)?;
            return Ok(true);
        }
        Ok(false)
    }

    pub(super) fn capture(&self, live: &Arc<State>, limits: Limits) -> Arc<State> {
        let mut stored = Arc::clone(live);
        let mut cells = if self.grids.is_empty() {
            0
        } else {
            live.grids.values().fold(0usize, |n, grid| {
                n.saturating_add(grid.width.saturating_mul(grid.height))
            })
        };
        for (&id, grid) in self.grids.iter() {
            if stored.grids.len() >= limits.max_grids {
                break;
            }
            let size = grid.width.saturating_mul(grid.height);
            if size <= limits.max_cells.saturating_sub(cells) && !stored.grids.contains_key(&id) {
                Arc::make_mut(&mut Arc::make_mut(&mut stored).grids).insert(id, Arc::clone(grid));
                cells = cells.saturating_add(size);
            }
        }

        for (&id, placement) in self.placements.iter() {
            if stored.placements.len() >= limits.max_collection_len {
                break;
            }
            if placement_window(placement).is_some() && !stored.placements.contains_key(&id) {
                Arc::make_mut(&mut Arc::make_mut(&mut stored).placements)
                    .insert(id, placement.clone());
            }
        }

        for (&id, viewport) in self.viewports.iter() {
            if stored.viewports.len() >= limits.max_collection_len {
                break;
            }
            if !stored.viewports.contains_key(&id) {
                Arc::make_mut(&mut Arc::make_mut(&mut stored).viewports)
                    .insert(id, viewport.clone());
            }
        }

        for (&id, highlight) in self.highlights.iter() {
            if stored.highlights.len() >= limits.max_collection_len {
                break;
            }
            if !stored.highlights.contains_key(&id) {
                Arc::make_mut(&mut Arc::make_mut(&mut stored).highlights)
                    .insert(id, highlight.clone());
            }
        }

        stored
    }
}

pub(super) fn placement_window(placement: &GridPlacement) -> Option<Window> {
    match placement.kind {
        GridPlacementKind::Normal { window, .. }
        | GridPlacementKind::Floating { window, .. }
        | GridPlacementKind::External { window } => Some(window),
        GridPlacementKind::Message { .. } => None,
    }
}
