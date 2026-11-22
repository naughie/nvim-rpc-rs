use super::decode::*;
use super::{Args, Model, Result};

use crate::ProtocolError;
use crate::model::*;

use std::sync::Arc;

impl Model {
    pub(super) fn grid_call(&mut self, name: &str, mut a: Args<'_>) -> Result<()> {
        let limits = self.limits;
        match name {
            "grid_resize" => {
                let id = grid_id(next(&mut a)?)?;
                let width = size(next(&mut a)?)?;
                let height = size(next(&mut a)?)?;
                bounded_len(width, limits)?;
                bounded_len(height, limits)?;
                let count = width
                    .checked_mul(height)
                    .ok_or(ProtocolError::Limit("grid cells"))?;
                let total = self
                    .state
                    .grids
                    .iter()
                    .filter(|(key, _)| **key != id)
                    .try_fold(count, |n, (_, g)| {
                        g.width
                            .checked_mul(g.height)
                            .and_then(|cells| n.checked_add(cells))
                    })
                    .ok_or(ProtocolError::Limit("grid cells"))?;
                if total > limits.max_cells {
                    return Err(ProtocolError::Limit("grid cells"));
                }
                if !self.state.grids.contains_key(&id) && self.state.grids.len() >= limits.max_grids
                {
                    return Err(ProtocolError::Limit("grids"));
                }
                let cell = Cell::default();
                let old = self.state.grids.get(&id);
                let mut rows = Vec::with_capacity(height);
                for row in 0..height {
                    if let Some(src) = old
                        .filter(|g| g.width == width)
                        .and_then(|g| g.rows.get(row))
                    {
                        rows.push(Arc::clone(src));
                        continue;
                    }
                    let mut cells = vec![cell.clone(); width];
                    if let Some(src) = old.and_then(|g| g.rows.get(row)) {
                        for (dst, src) in cells.iter_mut().zip(&src.cells) {
                            dst.clone_from(src);
                        }
                    }
                    rows.push(Arc::new(Row { cells, wrap: false }));
                }
                Arc::make_mut(&mut Arc::make_mut(&mut self.state).grids).insert(
                    id,
                    Arc::new(Grid {
                        width,
                        height,
                        rows,
                    }),
                );
                self.dirty(id, 0, height, 0, width);
                self.damage.layout = true;
            }
            "grid_line" => {
                let id = grid_id(next(&mut a)?)?;
                let row = index(next(&mut a)?)?;
                let col = index(next(&mut a)?)?;
                let cells = array(next(&mut a)?)?;
                bounded_len(cells.len(), limits)?;
                let wrap = boolean(next(&mut a)?)?;
                let grid = grid_mut(&mut self.state, id)?;
                let cell_text = &mut self.cell_text;
                if row >= grid.height || col > grid.width {
                    return invalid(name, "cell coordinates outside grid");
                }
                let width = grid.width;
                let dst = grid
                    .rows
                    .get_mut(row)
                    .ok_or_else(|| bad(name, "missing grid row"))?;
                let dst = Arc::make_mut(dst);
                let mut end = col;
                let mut highlight = None;
                for cell in cells {
                    let mut cell = array(cell)?;
                    let text = cell_text.intern(string(next(&mut cell)?, limits)?);
                    if let Some(value) = cell.next() {
                        highlight = Some(hl_id(value)?);
                    }
                    let highlight =
                        highlight.ok_or_else(|| bad(name, "first cell omits highlight"))?;
                    let repeat = cell.next().map(index).transpose()?.unwrap_or(1);
                    let next = end
                        .checked_add(repeat)
                        .filter(|&n| n <= width)
                        .ok_or_else(|| bad(name, "cell run outside grid"))?;
                    dst.cells
                        .get_mut(end..next)
                        .ok_or_else(|| bad(name, "cell run outside grid"))?
                        .fill(Cell { text, highlight });
                    end = next;
                }
                let changed_wrap = dst.wrap != wrap;
                dst.wrap = wrap;
                // row < grid.height was checked above, so row + 1 <= height
                // fits usize, including when height is usize::MAX.
                let bottom = row + 1;
                if changed_wrap {
                    self.dirty(id, row, bottom, 0, width);
                } else {
                    self.dirty(id, row, bottom, col, end);
                }
            }
            "grid_clear" => {
                let id = grid_id(next(&mut a)?)?;
                let grid = grid_mut(&mut self.state, id)?;
                let (width, height) = (grid.width, grid.height);
                let row = Arc::new(Row {
                    cells: vec![Cell::default(); width],
                    wrap: false,
                });
                grid.rows.fill(row);
                self.dirty(id, 0, height, 0, width);
            }
            "grid_destroy" => {
                let id = grid_id(next(&mut a)?)?;

                // A floating window can close before its grid's first resize.
                Arc::make_mut(&mut Arc::make_mut(&mut self.state).grids).remove(&id);
                Arc::make_mut(&mut Arc::make_mut(&mut self.state).placements).remove(&id);
                Arc::make_mut(&mut Arc::make_mut(&mut self.state).viewports).remove(&id);

                if self.state.cursor.is_some_and(|c| c.grid == id) {
                    Arc::make_mut(&mut self.state).cursor = None;
                    self.damage.cursor = true;
                }

                self.damage.record_removal(id, limits.max_grids);
            }
            "grid_cursor_goto" => {
                let id = grid_id(next(&mut a)?)?;
                let row = index(next(&mut a)?)?;
                let col = index(next(&mut a)?)?;
                let grid = self
                    .state
                    .grids
                    .get(&id)
                    .ok_or_else(|| bad(name, "unknown grid"))?;
                if row >= grid.height || col >= grid.width {
                    return invalid(name, "cursor outside grid");
                }
                Arc::make_mut(&mut self.state).cursor = Some(Cursor { grid: id, row, col });
                self.damage.cursor = true;
            }
            "grid_scroll" => {
                let id = grid_id(next(&mut a)?)?;
                let top = index(next(&mut a)?)?;
                let bottom = index(next(&mut a)?)?;
                let left = index(next(&mut a)?)?;
                let right = index(next(&mut a)?)?;
                let rows = integer(next(&mut a)?)?;
                let cols = integer(next(&mut a)?)?;
                let grid = grid_mut(&mut self.state, id)?;
                if top > bottom || bottom > grid.height || left > right || right > grid.width {
                    return invalid(name, "scroll region outside grid");
                }
                scroll(grid, top, bottom, left, right, rows, cols);
                self.dirty(id, top, bottom, left, right);
            }
            _ => return invalid(name, "unsupported grid event"),
        }
        Ok(())
    }
}

fn grid_mut(state: &mut Arc<State>, id: GridId) -> Result<&mut Grid> {
    Arc::make_mut(&mut Arc::make_mut(state).grids)
        .get_mut(&id)
        .map(Arc::make_mut)
        .ok_or_else(|| bad("grid", "unknown grid"))
}

fn scroll(
    grid: &mut Grid,
    top: usize,
    bottom: usize,
    left: usize,
    right: usize,
    rows: i64,
    cols: i64,
) {
    // grid_call checks top <= bottom <= height and left <= right <= width.
    // The model constructs grids with exactly height rows of exactly width
    // cells and validates that shape before installing restored grids. Only
    // its private grid methods mutate these vectors. Thus all region bounds
    // below are within their vectors. Traversing toward the source preserves
    // overlap without allocating a scratch buffer or deep-copying unchanged rows.
    let vertical = usize::try_from(rows.unsigned_abs()).unwrap_or(usize::MAX);
    let horizontal = usize::try_from(cols.unsigned_abs()).unwrap_or(usize::MAX);
    if vertical >= bottom - top || horizontal >= right - left {
        return;
    }
    // Now vertical < bottom-top and horizontal < right-left: each subtraction
    // is nonnegative; top+vertical < bottom and left+horizontal < right, so
    // every addition fits usize. For row in start..end, row +/- vertical lies
    // in top..bottom. Source/destination column ranges have equal length
    // right-left-horizontal, and lie in left..right. These facts prove every
    // row/cell index and slice below, including reversed traversal.
    let (start, end) = if rows >= 0 {
        (top, bottom - vertical)
    } else {
        (top + vertical, bottom)
    };
    let (dst_left, dst_right, src_left, src_right) = if cols >= 0 {
        (left, right - horizontal, left + horizontal, right)
    } else {
        (left + horizontal, right, left, right - horizontal)
    };
    let copy_row = |grid: &mut Grid, row: usize| {
        let src = if rows >= 0 {
            row + vertical
        } else {
            row - vertical
        };
        if left == 0 && right == grid.width && cols == 0 {
            grid.rows[row] = Arc::clone(&grid.rows[src]);
        } else if src == row {
            let cells = &mut Arc::make_mut(&mut grid.rows[row]).cells;
            if cols >= 0 {
                for (dst, src) in (dst_left..dst_right).zip(src_left..src_right) {
                    cells[dst] = cells[src].clone();
                }
            } else {
                for (dst, src) in (dst_left..dst_right).zip(src_left..src_right).rev() {
                    cells[dst] = cells[src].clone();
                }
            }
        } else {
            let src = Arc::clone(&grid.rows[src]);
            let dst = Arc::make_mut(&mut grid.rows[row]);
            dst.cells[dst_left..dst_right].clone_from_slice(&src.cells[src_left..src_right]);
        }
    };
    if rows >= 0 {
        for row in start..end {
            copy_row(grid, row);
        }
    } else {
        for row in (start..end).rev() {
            copy_row(grid, row);
        }
    }
}
