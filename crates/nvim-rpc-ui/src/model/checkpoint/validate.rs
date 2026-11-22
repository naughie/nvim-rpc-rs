use super::CheckpointError;

use crate::model::{
    Content, GridId, GridPlacementKind, HighlightAttributes, HighlightId, Limits, ModeInfo,
    PopupAnchor, State, Value,
};

type Result = std::result::Result<(), CheckpointError>;

pub(super) fn state(state: &State, limits: Limits) -> Result {
    let validate = Validate(limits);

    validate.grids(state)?;
    validate.layout(state)?;
    validate.styles(state)?;
    validate.widgets(state)?;
    validate.dictionary(&state.options)?;
    validate.collection(state.modes.len())?;
    for mode in state.modes.iter() {
        validate.dictionary(mode)?;
    }
    index(state.mode.index)?;
    for text in [&state.mode.name, &state.title, &state.icon, &state.cwd] {
        validate.bytes(text.len())?;
    }
    Ok(())
}

struct Validate(Limits);

impl Validate {
    fn grids(&self, state: &State) -> Result {
        if state.grids.len() > self.0.max_grids {
            return Err(CheckpointError::Limit("grids"));
        }

        let mut total = 0usize;
        for (&id, grid) in state.grids.iter() {
            grid_id(id)?;
            size(grid.width)?;
            size(grid.height)?;
            self.collection(grid.width)?;
            self.collection(grid.height)?;

            total = grid
                .width
                .checked_mul(grid.height)
                .and_then(|count| total.checked_add(count))
                .filter(|&count| count <= self.0.max_cells)
                .ok_or(CheckpointError::Limit("grid cells"))?;

            // Grid scrolling indexes both vectors using the stored dimensions.
            if grid.rows.len() != grid.height {
                return Err(CheckpointError::Invalid("grid row count"));
            }
            for row in &grid.rows {
                if row.cells.len() != grid.width {
                    return Err(CheckpointError::Invalid("grid column count"));
                }
                for cell in &row.cells {
                    self.bytes(cell.text.len())?;
                    highlight_id(cell.highlight)?;
                }
            }
        }

        if let Some(cursor) = state.cursor {
            // A resize can leave the cursor outside its grid. A destroy removes it.
            if !state.grids.contains_key(&cursor.grid) {
                return Err(CheckpointError::Invalid("cursor grid"));
            }
            index(cursor.row)?;
            index(cursor.col)?;
        }
        Ok(())
    }

    fn layout(&self, state: &State) -> Result {
        // Placement and viewport notifications can arrive before grid_resize.
        self.collection(state.placements.len())?;
        for (&id, placement) in state.placements.iter() {
            grid_id(id)?;
            match &placement.kind {
                GridPlacementKind::Normal { width, height, .. } => {
                    size(*width)?;
                    size(*height)?;
                }
                GridPlacementKind::Floating {
                    anchor,
                    anchor_grid,
                    anchor_row,
                    anchor_col,
                    ..
                } => {
                    grid_id(*anchor_grid)?;
                    self.bytes(anchor.len())?;
                    if !anchor_row.is_finite() || !anchor_col.is_finite() {
                        return Err(CheckpointError::Invalid("floating coordinates"));
                    }
                }
                GridPlacementKind::External { .. } => {}
                GridPlacementKind::Message { separator, .. } => self.bytes(separator.len())?,
            }
        }

        self.collection(state.viewports.len())?;
        for (&id, viewport) in state.viewports.iter() {
            grid_id(id)?;
            for margin in viewport.margins {
                index(margin)?;
            }
        }
        Ok(())
    }

    fn styles(&self, state: &State) -> Result {
        let colors = state.default_colors;
        rgb([colors.foreground, colors.background, colors.special])?;

        self.collection(state.highlights.len())?;
        for (&id, highlight) in state.highlights.iter() {
            highlight_id(id)?;
            self.attributes(&highlight.rgb)?;
            self.attributes(&highlight.cterm)?;

            self.collection(highlight.info.len())?;
            for value in &highlight.info {
                self.value(value, 0)?;
            }
        }

        self.collection(state.highlight_groups.len())?;
        for (name, &id) in state.highlight_groups.iter() {
            self.bytes(name.len())?;
            highlight_id(id)?;
        }
        Ok(())
    }

    fn attributes(&self, attr: &HighlightAttributes) -> Result {
        rgb([attr.foreground, attr.background, attr.special])?;
        if attr.blend.is_some_and(|blend| blend > 100) {
            return Err(CheckpointError::Invalid("highlight blend"));
        }
        if let Some(url) = &attr.url {
            self.bytes(url.len())?;
        }
        self.dictionary(&attr.extra)
    }

    fn widgets(&self, state: &State) -> Result {
        self.collection(state.cmdlines.len())?;
        for (&level, cmdline) in state.cmdlines.iter() {
            if level != cmdline.level
                || usize::try_from(level).is_err()
                || i64::try_from(level).is_err()
            {
                return Err(CheckpointError::Invalid("command-line level"));
            }
            self.content(&cmdline.content)?;
            command_position(&cmdline.content, cmdline.pos)?;
            index(cmdline.indent)?;
            self.bytes(cmdline.firstc.len())?;
            self.bytes(cmdline.prompt.len())?;
            if let Some((text, _)) = &cmdline.special {
                self.bytes(text.len())?;
            }
        }
        self.collection(state.cmdline_block.len())?;
        for content in state.cmdline_block.iter() {
            self.content(content)?;
        }

        if let Some(popup) = &state.popupmenu {
            self.collection(popup.items.len())?;
            selection(popup.selected, popup.items.len())?;
            match popup.anchor {
                PopupAnchor::Grid {
                    grid: GridId(-1), ..
                } => {
                    return Err(CheckpointError::Invalid("popup menu anchor"));
                }
                PopupAnchor::Grid { .. } => {}
                PopupAnchor::Cmdline { byte_offset } => index(byte_offset)?,
            }
            for item in &popup.items {
                for text in [&item.word, &item.kind, &item.menu, &item.info] {
                    self.bytes(text.len())?;
                }
            }
        }
        if let Some(menu) = &state.wildmenu {
            self.collection(menu.items.len())?;
            selection(menu.selected, menu.items.len())?;
            for item in &menu.items {
                self.bytes(item.len())?;
            }
        }

        if let Some(tabline) = &state.tabline {
            self.collection(tabline.tabs.len())?;
            self.collection(tabline.buffers.len())?;
            for tab in &tabline.tabs {
                self.bytes(tab.name.len())?;
            }
            for buffer in &tabline.buffers {
                self.bytes(buffer.name.len())?;
            }
        }

        let messages = &state.messages;
        if messages.visible.len() > self.0.max_messages {
            return Err(CheckpointError::Limit("messages"));
        }
        for message in &messages.visible {
            self.content(&message.content)?;
            self.bytes(message.kind.len())?;
            self.bytes(message.trigger.len())?;
            self.value(&message.id, 0)?;
        }
        for content in [&messages.showmode, &messages.showcmd, &messages.ruler] {
            self.content(content)?;
        }
        if let Some(history) = &messages.history {
            self.collection(history.len())?;
            for entry in history {
                self.content(&entry.content)?;
                self.bytes(entry.kind.len())?;
            }
        }

        Ok(())
    }

    fn content(&self, content: &Content) -> Result {
        self.collection(content.len())?;
        for chunk in content {
            highlight_id(chunk.highlight)?;
            self.bytes(chunk.text.len())?;
        }
        Ok(())
    }

    fn dictionary(&self, dict: &ModeInfo) -> Result {
        self.collection(dict.len())?;
        for (name, value) in dict {
            self.bytes(name.len())?;
            self.value(value, 0)?;
        }
        Ok(())
    }

    fn value(&self, value: &Value, depth: usize) -> Result {
        if depth > self.0.max_depth {
            return Err(CheckpointError::Limit("value depth"));
        }
        match value {
            Value::String(v) | Value::Binary(v) | Value::Extension(_, v) => self.bytes(v.len()),
            Value::Array(v) => {
                self.collection(v.len())?;
                let depth = depth
                    .checked_add(1)
                    .ok_or(CheckpointError::Limit("value depth"))?;
                for item in v.iter() {
                    self.value(item, depth)?;
                }
                Ok(())
            }
            Value::Map(v) => {
                self.collection(v.len())?;
                let depth = depth
                    .checked_add(1)
                    .ok_or(CheckpointError::Limit("value depth"))?;
                for (key, value) in v.iter() {
                    self.value(key, depth)?;
                    self.value(value, depth)?;
                }
                Ok(())
            }
            Value::Nil | Value::Bool(_) | Value::Int(_) | Value::Uint(_) | Value::Float(_) => {
                Ok(())
            }
        }
    }

    fn collection(&self, len: usize) -> Result {
        if len > self.0.max_collection_len {
            return Err(CheckpointError::Limit("collection length"));
        }
        Ok(())
    }

    fn bytes(&self, len: usize) -> Result {
        if len > self.0.max_string_bytes {
            return Err(CheckpointError::Limit("string bytes"));
        }
        Ok(())
    }
}

fn index(index: usize) -> Result {
    if i64::try_from(index).is_err() {
        return Err(CheckpointError::Invalid("index out of range"));
    }
    Ok(())
}

fn size(size: usize) -> Result {
    if size == 0 {
        return Err(CheckpointError::Invalid("zero size"));
    }
    index(size)
}

fn grid_id(id: GridId) -> Result {
    if id.0 <= 0 {
        return Err(CheckpointError::Invalid("grid id"));
    }
    Ok(())
}

fn highlight_id(id: HighlightId) -> Result {
    if id.0 < 0 {
        return Err(CheckpointError::Invalid("highlight id"));
    }
    Ok(())
}

fn rgb(colors: [Option<u32>; 3]) -> Result {
    if colors.into_iter().flatten().any(|color| color > 0xff_ffff) {
        return Err(CheckpointError::Invalid("color"));
    }
    Ok(())
}

fn selection(selected: Option<usize>, count: usize) -> Result {
    if let Some(selected) = selected {
        index(selected)?;
        if selected >= count {
            return Err(CheckpointError::Invalid("selection outside items"));
        }
    }
    Ok(())
}

fn command_position(content: &Content, pos: usize) -> Result {
    index(pos)?;
    let mut remaining = pos;
    for chunk in content {
        if let Some(n) = remaining.checked_sub(chunk.text.len()) {
            remaining = n;
        } else if chunk.text.is_char_boundary(remaining) {
            return Ok(());
        } else {
            return Err(CheckpointError::Invalid("command-line position"));
        }
    }
    if remaining != 0 {
        return Err(CheckpointError::Invalid("command-line position"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::{Checkpoint, MAX_VALUE_DEPTH};
    use super::*;

    use crate::model::{Cell, Cursor, Grid, Row};

    use std::collections::BTreeMap;
    use std::sync::Arc;

    #[test]
    fn rejects_inconsistent_grid_shape() {
        let limits = Limits::default();
        for (rows, expected) in [
            (vec![], "grid row count"),
            (
                vec![Arc::new(Row {
                    cells: vec![Cell::default()],
                    wrap: false,
                })],
                "grid column count",
            ),
        ] {
            let snapshot = State {
                grids: Arc::new(BTreeMap::from([(
                    GridId(1),
                    Arc::new(Grid {
                        width: 2,
                        height: 1,
                        rows,
                    }),
                )])),
                ..State::default()
            };
            assert!(matches!(
                state(&snapshot, limits),
                Err(CheckpointError::Invalid(reason)) if reason == expected
            ));
        }
    }

    #[test]
    fn rejects_cursor_on_absent_grid() {
        let snapshot = State {
            cursor: Some(Cursor {
                grid: GridId(1),
                row: 0,
                col: 0,
            }),
            ..State::default()
        };
        assert!(matches!(
            state(&snapshot, Limits::default()),
            Err(CheckpointError::Invalid("cursor grid"))
        ));
    }

    #[test]
    fn deeply_nested_maps_round_trip_at_the_format_depth_limit() {
        let make = |depth| {
            let nested = (0..depth).fold(Value::Nil, |value, _| {
                Value::Map(Arc::new([(Value::Int(1), value)]))
            });
            Checkpoint::from_committed(Arc::new(State {
                options: Arc::new(BTreeMap::from([(Arc::from("nested"), nested)])),
                ..State::default()
            }))
        };

        let checkpoint = make(MAX_VALUE_DEPTH);
        let bytes = checkpoint.encode().unwrap();
        let limits = Limits {
            max_depth: MAX_VALUE_DEPTH,
            ..Limits::default()
        };
        let restored = Checkpoint::decode(&bytes, limits, bytes.len()).unwrap();
        assert_eq!(restored.state().options, checkpoint.state().options);
        let too_deep = make(MAX_VALUE_DEPTH + 1);
        assert!(matches!(
            too_deep.encode(),
            Err(CheckpointError::Limit("value depth"))
        ));

        use crate::model::checkpoint::{MAGIC, VERSION};

        // Bypass the encoder's limit to exercise rejection during decoding.
        let mut bytes = MAGIC.to_vec();
        bytes.push(VERSION);
        let bytes = postcard::to_extend(too_deep.state(), bytes).unwrap();
        assert!(matches!(
            Checkpoint::decode(
                &bytes,
                Limits {
                    max_depth: usize::MAX,
                    ..limits
                },
                bytes.len()
            ),
            Err(CheckpointError::Decode(_))
        ));
    }
}
