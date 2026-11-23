use nvim_rpc::mp::encode::primitive::Map;
use nvim_rpc::mp::encode::{Primitive, WriterSyncExt as _};
use nvim_rpc::rpc::Message;

use nvim_rpc_ui::{
    Checkpoint, CheckpointError, Event, Frame, GridId, GridPlacementKind, HighlightId, Limits,
    Model, ProtocolError, Value,
};

use std::sync::Arc;

fn notify(
    model: &mut Model,
    method: &str,
    params: impl Primitive,
) -> Result<Vec<Frame>, ProtocolError> {
    let mut bytes = Vec::new();
    bytes.write_primitive((2u8, method, params)).consume();
    let Message::Notification(notification) = nvim_rpc::rpc::decode(&mut bytes.as_slice()).unwrap()
    else {
        panic!("expected a notification");
    };

    let mut frames = Vec::new();
    assert!(model.apply_notification(
        notification.method.as_slice(),
        notification.params,
        |frame| frames.push(frame),
    )?);
    Ok(frames)
}

fn redraw(model: &mut Model, params: impl Primitive) -> Result<Vec<Frame>, ProtocolError> {
    notify(model, "redraw", params)
}

fn flush(model: &mut Model) -> Frame {
    let [frame]: [Frame; 1] = redraw(model, (("flush", ()),)).unwrap().try_into().unwrap();
    frame
}

#[test]
fn exports_only_flushed_state_and_excludes_saved_events() {
    let limits = Limits::default();
    let mut model = Model::new(limits);
    assert!(model.checkpoint().is_none());

    let error = notify(&mut model, "nvim_error_event", (0, "before flush")).unwrap();
    assert!(matches!(
        error[0].events.as_slice(),
        [Event::RpcError { .. }]
    ));
    assert!(model.checkpoint().is_none());

    let frames = redraw(
        &mut model,
        (
            ("set_title", ("saved title",)),
            ("bell", ()),
            ("connect", ("saved address",)),
            ("flush", ()),
            ("set_title", ("pending title",)),
            ("screenshot", ("pending path",)),
        ),
    )
    .unwrap();
    assert!(matches!(
        frames[0].events.as_slice(),
        [Event::SetTitle(_), Event::Bell, Event::Connect(_)]
    ));
    let checkpoint = model.checkpoint().unwrap();
    assert_eq!(checkpoint.state().title.as_ref(), "saved title");

    let error = notify(&mut model, "nvim_error_event", (1, "after flush")).unwrap();
    assert!(matches!(
        error[0].events.as_slice(),
        [
            Event::SetTitle(_),
            Event::Screenshot(_),
            Event::RpcError { .. }
        ]
    ));
    assert_eq!(
        model.checkpoint().unwrap().state().title.as_ref(),
        "saved title"
    );
    assert_eq!(flush(&mut model).state.title.as_ref(), "pending title");
    assert_eq!(checkpoint.state().title.as_ref(), "saved title");

    let bytes = checkpoint.encode().unwrap();
    let checkpoint = Checkpoint::decode(&bytes, limits, bytes.len()).unwrap();
    let mut restored = Model::from_checkpoint(checkpoint, limits).unwrap();
    let first = flush(&mut restored);
    assert_eq!(first.sequence, 1);
    assert!(first.damage.full);
    assert_eq!(first.state.title.as_ref(), "saved title");
    assert!(first.events.is_empty());
    assert!(!flush(&mut restored).damage.full);
}

#[test]
fn restores_hidden_wide_cells_and_accepts_subsequent_updates() {
    let limits = Limits::default();
    let mut model = Model::new(limits);
    let frames = redraw(
        &mut model,
        (
            ("grid_resize", (1, 1, 1), (4, 4, 2)),
            (
                "grid_line",
                (4, 0, 0, (("界", 7), ("",), ("e\u{301}", 7), ("!", 0)), true),
            ),
            (
                "hl_attr_define",
                (
                    7,
                    Map([("foreground", 0x8800ff)]),
                    Map([("foreground", 5)]),
                    (),
                ),
            ),
            ("hl_group_set", ("Float", 7)),
            ("default_colors_set", (0xdddddd, 0x111111, -1, 7, 0)),
            (
                "win_float_pos",
                (4, 1000, "NW", 1, 1.5, 3.25, true, 50, 1, 2, 3),
            ),
            ("win_viewport", (4, 1000, 2, 8, 3, 4, 20, -1)),
            ("win_viewport_margins", (4, 1000, 1, 0, 2, 0)),
            ("win_hide", (4,)),
            ("flush", ()),
        ),
    )
    .unwrap();
    assert!(frames[0].state.placements[&GridId(4)].hidden);

    let bytes = model.checkpoint().unwrap().encode().unwrap();
    let checkpoint = Checkpoint::decode(&bytes, limits, bytes.len()).unwrap();
    let state = checkpoint.state();
    let grid = &state.grids[&GridId(4)];
    assert_eq!((grid.width, grid.height), (4, 2));
    assert!(grid.rows[0].wrap);
    let cells: Vec<_> = grid.rows[0]
        .cells
        .iter()
        .map(|cell| (cell.text.as_ref(), cell.highlight))
        .collect();
    assert_eq!(
        cells,
        [
            ("界", HighlightId(7)),
            ("", HighlightId(7)),
            ("e\u{301}", HighlightId(7)),
            ("!", HighlightId(0))
        ]
    );
    assert!(state.placements[&GridId(4)].hidden);
    assert!(matches!(
        &state.placements[&GridId(4)].kind,
        GridPlacementKind::Floating {
            window,
            anchor,
            anchor_grid: GridId(1),
            anchor_row: 1.5,
            anchor_col: 3.25,
            mouse_enabled: true,
            zindex: 50,
            compindex: Some(1),
            screen_row: Some(2),
            screen_col: Some(3),
        } if window.id() == 1000 && anchor.as_ref() == "NW"
    ));
    let viewport = &state.viewports[&GridId(4)];
    assert_eq!(viewport.window.id(), 1000);
    assert_eq!(
        (
            viewport.topline,
            viewport.botline,
            viewport.curline,
            viewport.curcol
        ),
        (2, 8, 3, 4)
    );
    assert_eq!(
        (viewport.line_count, viewport.scroll_delta, viewport.margins),
        (20, -1, [1, 0, 2, 0])
    );
    assert_eq!(
        state.highlights[&HighlightId(7)].rgb.foreground,
        Some(0x8800ff)
    );
    assert_eq!(state.highlights[&HighlightId(7)].cterm.foreground, Some(5));
    assert_eq!(state.highlight_groups["Float"], HighlightId(7));
    assert_eq!(state.default_colors.foreground, Some(0xdddddd));
    assert_eq!(state.default_colors.background, Some(0x111111));
    assert_eq!(state.default_colors.special, None);
    assert_eq!(state.default_colors.cterm_foreground, 7);

    let mut restored = Model::from_checkpoint(checkpoint, limits).unwrap();
    let updates = (
        ("win_float_pos", (4, 1000, "NW", 1, 2.5, 4.0, true, 50)),
        ("grid_cursor_goto", (4, 1, 2)),
        ("grid_line", (4, 1, 2, (("新", 7), ("",)), false)),
        ("flush", ()),
    );
    assert!(matches!(
        redraw(&mut Model::default(), updates),
        Err(ProtocolError::Invalid { event, reason: "unknown grid" }) if event == "grid_cursor_goto"
    ));

    let frames = redraw(&mut restored, updates).unwrap();
    let first = &frames[0];
    let cursor = first.state.cursor.unwrap();
    assert_eq!((cursor.grid, cursor.row, cursor.col), (GridId(4), 1, 2));
    assert!(!first.state.placements[&GridId(4)].hidden);
    assert_eq!(
        first.state.grids[&GridId(4)]
            .cell(0, 0)
            .unwrap()
            .text
            .as_ref(),
        "界"
    );
    assert_eq!(
        first.state.grids[&GridId(4)]
            .cell(1, 2)
            .unwrap()
            .text
            .as_ref(),
        "新"
    );
    assert!(first.damage.full);
    assert!(first.events.is_empty());

    let frames = redraw(
        &mut restored,
        (
            ("grid_resize", (4, 3, 2)),
            ("grid_clear", (4,)),
            (
                "hl_attr_define",
                (
                    7,
                    Map([("foreground", 0x445566)]),
                    Map([("foreground", 6)]),
                    (),
                ),
            ),
            ("flush", ()),
            ("grid_destroy", (4,)),
            ("flush", ()),
        ),
    )
    .unwrap();
    let cleared = &frames[0].state;
    let grid = &cleared.grids[&GridId(4)];
    assert_eq!((grid.width, grid.height), (3, 2));
    assert!(!grid.rows[0].wrap);
    assert_eq!(grid.cell(0, 0).unwrap().text.as_ref(), " ");
    assert_eq!(
        cleared.highlights[&HighlightId(7)].rgb.foreground,
        Some(0x445566)
    );

    let destroyed = &frames[1];
    assert_eq!(destroyed.damage.removed_grids, [GridId(4)]);
    assert!(!destroyed.state.grids.contains_key(&GridId(4)));
    assert!(!destroyed.state.placements.contains_key(&GridId(4)));
    assert!(!destroyed.state.viewports.contains_key(&GridId(4)));
    assert!(destroyed.state.cursor.is_none());
}

#[test]
fn round_trips_retained_widgets_and_values() {
    let limits = Limits::default();
    let mut model = Model::new(limits);
    let bits = 0x7ff8_0123_4567_89ab;
    let frames = redraw(
        &mut model,
        (
            ("mode_info_set", (true, (Map([("cursor_shape", "block")]),))),
            ("mode_change", ("insert", 0)),
            ("cmdline_show", (((1, "日本語"),), 3, ":", "prompt", 0, 1)),
            ("cmdline_special_char", ("^", true, 1)),
            ("cmdline_block_show", ((((0, "block"),),),)),
            (
                "popupmenu_show",
                ((("word", "kind", "menu", "info"),), 0, 0, 3, -1),
            ),
            ("wildmenu_show", (("one", "two"),)),
            ("wildmenu_select", (1,)),
            (
                "tabline_update",
                (1, (Map([("tab", 1)]),), 2, (Map([("buffer", 2)]),)),
            ),
            (
                "msg_show",
                (
                    "echo",
                    ((1, "visible"),),
                    false,
                    true,
                    false,
                    1,
                    "typed_cmd",
                ),
            ),
            (
                "msg_history_show",
                ((("echo", ((0, "old"),), false),), true),
            ),
            (
                "option_set",
                (
                    "future",
                    (
                        Map([("same", 1), ("same", 2)]),
                        "日本語",
                        "日本語".as_bytes(),
                        u64::MAX,
                        f64::from_bits(bits),
                    ),
                ),
            ),
            ("mouse_on", ()),
            ("busy_start", ()),
            ("flush", ()),
        ),
    )
    .unwrap();
    assert_eq!(frames.len(), 1);

    let bytes = model.checkpoint().unwrap().encode().unwrap();
    let checkpoint = Checkpoint::decode(&bytes, limits, bytes.len()).unwrap();
    let state = checkpoint.state();
    assert!(state.mode.cursor_style_enabled);
    assert_eq!((state.mode.name.as_ref(), state.mode.index), ("insert", 0));
    assert_eq!(
        state.modes[0]["cursor_shape"],
        Value::String(Arc::from(b"block".as_slice()))
    );
    assert!(state.mouse_enabled && state.busy);
    assert_eq!(state.cmdlines[&1].content[0].text.as_ref(), "日本語");
    assert_eq!(state.cmdlines[&1].pos, 3);
    assert_eq!(state.cmdlines[&1].special, Some((Arc::from("^"), true)));
    assert_eq!(state.cmdline_block[0][0].text.as_ref(), "block");
    assert_eq!(
        state.popupmenu.as_ref().unwrap().items[0].word.as_ref(),
        "word"
    );
    assert_eq!(state.popupmenu.as_ref().unwrap().selected, Some(0));
    assert_eq!(state.wildmenu.as_ref().unwrap().selected, Some(1));
    assert_eq!(state.tabline.as_ref().unwrap().current.id(), 1);
    assert_eq!(state.tabline.as_ref().unwrap().buffers[0].buffer.id(), 2);
    assert_eq!(
        state.messages.visible[0].content[0].text.as_ref(),
        "visible"
    );
    assert_eq!(
        state.messages.history.as_ref().unwrap()[0].content[0]
            .text
            .as_ref(),
        "old"
    );
    assert!(state.messages.history_prev_cmd);

    let Value::Array(values) = &state.options["future"] else {
        panic!("expected option array");
    };
    let [Value::Map(map), text, binary, uint, Value::Float(float)] = values.as_ref() else {
        panic!("unexpected option values: {values:?}");
    };
    assert_eq!(
        map.as_ref(),
        [
            (Value::String(Arc::from(b"same".as_slice())), Value::Int(1)),
            (Value::String(Arc::from(b"same".as_slice())), Value::Int(2)),
        ]
    );
    assert_eq!(*text, Value::String(Arc::from("日本語".as_bytes())));
    assert_eq!(*binary, Value::Binary(Arc::from("日本語".as_bytes())));
    assert_eq!(*uint, Value::Uint(u64::MAX));
    assert_eq!(float.to_bits(), bits);

    let shallow = Limits {
        max_depth: 1,
        ..limits
    };
    assert!(matches!(
        Checkpoint::decode(&bytes, shallow, bytes.len()),
        Err(CheckpointError::Limit("value depth"))
    ));
}

#[test]
fn round_trips_layout_entries_and_cursor_independent_of_current_grid_shape() {
    let limits = Limits::default();
    let mut model = Model::new(limits);
    redraw(
        &mut model,
        (
            ("grid_resize", (1, 2, 2)),
            ("grid_cursor_goto", (1, 1, 1)),
            ("grid_resize", (1, 1, 1)),
            ("grid_line", (1, 0, 0, (("x", 17),), false)),
            ("win_float_pos", (4, 1000, "NW", 1, 1.0, 1.0, true, 50)),
            ("win_viewport", (4, 1000, 0, 2, 0, 0, 2, 0)),
            ("flush", ()),
        ),
    )
    .unwrap();

    let bytes = model.checkpoint().unwrap().encode().unwrap();
    let checkpoint = Checkpoint::decode(&bytes, limits, bytes.len()).unwrap();
    let state = checkpoint.state();
    let cursor = state.cursor.unwrap();
    assert_eq!((cursor.grid, cursor.row, cursor.col), (GridId(1), 1, 1));
    assert!(
        state.grids[&GridId(1)]
            .cell(cursor.row, cursor.col)
            .is_none()
    );
    assert_eq!(
        state.grids[&GridId(1)].cell(0, 0).unwrap().highlight,
        HighlightId(17)
    );
    assert!(!state.highlights.contains_key(&HighlightId(17)));
    assert!(!state.grids.contains_key(&GridId(4)));
    assert!(state.placements.contains_key(&GridId(4)));
    assert!(state.viewports.contains_key(&GridId(4)));
}

#[test]
fn rejects_invalid_truncated_incompatible_and_over_limit_checkpoints() {
    let limits = Limits::default();
    let mut model = Model::new(limits);
    redraw(
        &mut model,
        (
            ("grid_resize", (1, 2, 2), (4, 2, 1)),
            ("set_title", ("retained title",)),
            ("flush", ()),
        ),
    )
    .unwrap();
    let checkpoint = model.checkpoint().unwrap();
    let bytes = checkpoint.encode().unwrap();
    assert!(Checkpoint::decode(&bytes, limits, bytes.len()).is_ok());

    assert!(matches!(
        Checkpoint::decode(&bytes, limits, bytes.len() - 1),
        Err(CheckpointError::Limit(_))
    ));
    for n in 0..bytes.len() {
        assert!(
            Checkpoint::decode(&bytes[..n], limits, bytes.len()).is_err(),
            "prefix of length {n}"
        );
    }

    let mut malformed = bytes.clone();
    malformed.push(0);
    assert!(Checkpoint::decode(&malformed, limits, malformed.len()).is_err());
    malformed.pop();
    malformed[0] = 0;
    assert!(Checkpoint::decode(&malformed, limits, malformed.len()).is_err());

    let mut incompatible = bytes.clone();
    incompatible[4] = 255;
    assert!(matches!(
        Checkpoint::decode(&incompatible, limits, incompatible.len()),
        Err(CheckpointError::IncompatibleVersion(255))
    ));

    for limits in [
        Limits {
            max_grids: 1,
            ..limits
        },
        Limits {
            max_cells: 5,
            ..limits
        },
        Limits {
            max_collection_len: 1,
            ..limits
        },
        Limits {
            max_string_bytes: 4,
            ..limits
        },
    ] {
        assert!(matches!(
            Checkpoint::decode(&bytes, limits, bytes.len()),
            Err(CheckpointError::Limit(_))
        ));
        assert!(matches!(
            Model::from_checkpoint(checkpoint.clone(), limits),
            Err(CheckpointError::Limit(_))
        ));
    }
}

#[test]
fn reads_postcard_checkpoint() {
    // Version 2 output for a 1×1 grid 4 containing "A", the cursor on that
    // cell, and the title "fixture".
    const V2: &[u8] = b"NRUI\x02\x01\x08\x01\x01\x01\x01\x01A\x00\x00\
                       \x00\x00\x00\x00\x00\x00\x00\x00\x00\x01\x08\x00\x00\
                       \x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\
                       \x00\x00\x00\x00\x00\x07fixture\x00\x00\x00\x00";

    let limits = Limits::default();
    let checkpoint = Checkpoint::decode(V2, limits, V2.len()).unwrap();
    let state = checkpoint.state();
    assert_eq!(state.title.as_ref(), "fixture");
    assert_eq!(
        state.grids[&GridId(4)].cell(0, 0).unwrap().text.as_ref(),
        "A"
    );
    let cursor = state.cursor.unwrap();
    assert_eq!((cursor.grid, cursor.row, cursor.col), (GridId(4), 0, 0));
    assert_eq!(checkpoint.encode().unwrap(), V2);

    let mut malformed = V2.to_vec();
    let grid = malformed
        .windows(8)
        .position(|bytes| bytes == b"\x01\x08\x01\x01\x01\x01\x01A")
        .unwrap();
    malformed[grid + 2] = 2;
    assert!(matches!(
        Checkpoint::decode(&malformed, limits, malformed.len()),
        Err(CheckpointError::Invalid("grid column count"))
    ));

    let mut noncanonical = V2.to_vec();
    noncanonical.splice(grid..grid + 1, [0x81, 0x00]);
    assert!(matches!(
        Checkpoint::decode(&noncanonical, limits, noncanonical.len()),
        Err(CheckpointError::Invalid("payload representation"))
    ));

    // A complete row count with no rows must fail without reserving that count.
    let oversized = b"NRUI\x02\x01\x08\x01\x01\xff\xff\xff\xff\x0f";
    assert!(matches!(
        Checkpoint::decode(oversized, limits, oversized.len()),
        Err(CheckpointError::Decode(
            postcard::Error::DeserializeUnexpectedEnd
        ))
    ));

    let v1 = b"NRUI\x01\xdc\x00\x15";
    assert!(matches!(
        Checkpoint::decode(v1, limits, v1.len()),
        Err(CheckpointError::IncompatibleVersion(1))
    ));
}

#[test]
fn uses_saved_cells_after_a_fresh_reference_and_never_replays_transient_widgets() {
    let limits = Limits::default();
    let mut model = Model::new(limits);
    redraw(
        &mut model,
        (
            ("grid_resize", (2, 3, 1), (5, 1, 1)),
            ("grid_line", (2, 0, 0, (("a", 1), ("b",), ("c",)), false)),
            ("grid_line", (5, 0, 0, (("complete", 1),), false)),
            (
                "hl_attr_define",
                (
                    1,
                    Map([("foreground", 0x123456)]),
                    Map([("foreground", 3)]),
                    (),
                ),
            ),
            ("win_float_pos", (2, 1000, "NW", 1, 2.0, 3.0, true, 50)),
            ("win_float_pos", (5, 1001, "NW", 1, 4.0, 5.0, true, 50)),
            ("cmdline_show", (((0, "detach"),), 6, ":", "", 0, 1)),
            ("popupmenu_show", ((("detach", "", "", ""),), 0, 0, 0, -1)),
            ("flush", ()),
        ),
    )
    .unwrap();

    let mut attached =
        Model::with_checkpoint_reference(model.checkpoint().unwrap(), limits).unwrap();
    assert!(attached.checkpoint().is_none());
    let frame = flush(&mut attached);
    assert!(frame.damage.full);
    assert!(frame.state.grids.is_empty());
    assert!(frame.state.placements.is_empty());
    assert!(frame.state.cmdlines.is_empty());
    assert!(frame.state.popupmenu.is_none());

    // An unreferenced grid can still surface in a subsequent attachment.
    let bytes = attached.checkpoint().unwrap().encode().unwrap();
    let checkpoint = Checkpoint::decode(&bytes, limits, bytes.len()).unwrap();
    let mut attached = Model::with_checkpoint_reference(checkpoint, limits).unwrap();

    let frames = redraw(
        &mut attached,
        (
            ("grid_line", (2, 0, 1, (("B", 1),), false)),
            ("flush", ()),
            ("win_float_pos", (2, 1000, "NW", 1, 6.0, 7.0, true, 50)),
            ("grid_cursor_goto", (2, 0, 2)),
            ("flush", ()),
        ),
    )
    .unwrap();
    let [hidden, shown]: [Frame; 2] = frames.try_into().unwrap();

    assert!(!hidden.state.placements.contains_key(&GridId(2)));
    let grid = &shown.state.grids[&GridId(2)];
    let text: Vec<_> = grid.rows[0]
        .cells
        .iter()
        .map(|cell| cell.text.as_ref())
        .collect();
    assert_eq!(text, ["a", "B", "c"]);
    assert_eq!(
        shown.state.highlights[&HighlightId(1)].rgb.foreground,
        Some(0x123456)
    );
    assert!(matches!(
        shown.state.placements[&GridId(2)].kind,
        GridPlacementKind::Floating {
            anchor_row: 6.0,
            anchor_col: 7.0,
            ..
        }
    ));
    assert_eq!(shown.state.cursor.unwrap().grid, GridId(2));
    assert!(!shown.state.grids.contains_key(&GridId(5)));
    assert!(!shown.state.placements.contains_key(&GridId(5)));
}

#[test]
fn restores_matching_viewports_and_preserves_new_hides_closes_and_highlights() {
    let limits = Limits::default();
    let mut model = Model::new(limits);
    redraw(
        &mut model,
        (
            (
                "grid_resize",
                (2, 3, 1),
                (3, 1, 1),
                (4, 1, 1),
                (5, 1, 1),
                (6, 1, 1),
            ),
            ("grid_line", (2, 0, 0, (("a", 1), ("b",), ("c",)), false)),
            (
                "hl_attr_define",
                (
                    1,
                    Map([("foreground", 0x123456)]),
                    Map([("foreground", 3)]),
                    (),
                ),
            ),
            (
                "win_pos",
                (2, 1000, 1, 3, 3, 1),
                (3, 1001, 4, 5, 1, 1),
                (4, 1002, 6, 7, 1, 1),
                (5, 1003, 8, 9, 1, 1),
                (6, 1004, 10, 11, 1, 1),
            ),
            (
                "win_viewport_margins",
                (2, 1000, 1, 2, 3, 4),
                (3, 1001, 4, 3, 2, 1),
            ),
            ("win_hide", (3,)),
            ("flush", ()),
        ),
    )
    .unwrap();

    let mut attached =
        Model::with_checkpoint_reference(model.checkpoint().unwrap(), limits).unwrap();
    let frames = redraw(
        &mut attached,
        (
            ("grid_resize", (2, 2, 1)),
            (
                "hl_attr_define",
                (
                    1,
                    Map([("foreground", 0xaabbcc)]),
                    Map([("foreground", 4)]),
                    (),
                ),
            ),
            ("win_viewport_margins", (3, 1001, 1, 2, 3, 4)),
            ("win_hide", (4,)),
            ("win_close", (5,)),
            (
                "win_viewport",
                (2, 1000, 0, 1, 0, 0, 1, 0),
                (3, 1001, 0, 1, 0, 0, 1, 0),
                (4, 1002, 0, 1, 0, 0, 1, 0),
                (5, 1003, 0, 1, 0, 0, 1, 0),
                (6, 1234, 0, 1, 0, 0, 1, 0),
            ),
            ("flush", ()),
        ),
    )
    .unwrap();
    let state = &frames[0].state;

    let grid = &state.grids[&GridId(2)];
    assert_eq!((grid.width, grid.height), (2, 1));
    assert_eq!(grid.cell(0, 1).unwrap().text.as_ref(), "b");
    assert_eq!(
        state.highlights[&HighlightId(1)].rgb.foreground,
        Some(0xaabbcc)
    );

    assert!(matches!(
        state.placements[&GridId(2)].kind,
        GridPlacementKind::Normal { row: 1, col: 3, .. }
    ));
    assert!(!state.placements[&GridId(2)].hidden);
    assert!(state.placements[&GridId(3)].hidden);
    assert_eq!(state.viewports[&GridId(2)].margins, [1, 2, 3, 4]);
    assert_eq!(state.viewports[&GridId(3)].margins, [1, 2, 3, 4]);
    assert!(state.placements[&GridId(4)].hidden);
    for id in [GridId(5), GridId(6)] {
        assert!(!state.placements.contains_key(&id));
        assert!(!state.grids.contains_key(&id));
    }
    let saved = attached.checkpoint().unwrap();
    assert!(!saved.state().placements.contains_key(&GridId(5)));
    assert!(!saved.state().placements.contains_key(&GridId(6)));
}

#[test]
fn caps_combined_checkpoints_and_invalidates_destroyed_reference_grids() {
    let limits = Limits {
        max_grids: 2,
        max_cells: 3,
        ..Limits::default()
    };
    let mut model = Model::new(limits);
    redraw(
        &mut model,
        (
            ("grid_resize", (2, 1, 1), (3, 2, 1)),
            ("win_pos", (2, 1000, 0, 0, 1, 1)),
            ("flush", ()),
        ),
    )
    .unwrap();
    let mut attached =
        Model::with_checkpoint_reference(model.checkpoint().unwrap(), limits).unwrap();

    redraw(&mut attached, (("grid_resize", (4, 2, 1)),)).unwrap();
    let frame = flush(&mut attached);
    let active: Vec<_> = frame.state.grids.keys().copied().collect();
    assert_eq!(active, [GridId(4)]);
    let bytes = attached.checkpoint().unwrap().encode().unwrap();
    let saved = Checkpoint::decode(&bytes, limits, bytes.len()).unwrap();
    let stored: Vec<_> = saved.state().grids.keys().copied().collect();
    assert_eq!(stored, [GridId(2), GridId(4)]);

    redraw(&mut attached, (("grid_destroy", (2,)),)).unwrap();
    assert!(
        attached
            .checkpoint()
            .unwrap()
            .state()
            .grids
            .contains_key(&GridId(2))
    );
    flush(&mut attached);
    let checkpoint = attached.checkpoint().unwrap();
    assert!(!checkpoint.state().grids.contains_key(&GridId(2)));
    assert!(!checkpoint.state().placements.contains_key(&GridId(2)));

    assert!(matches!(
        redraw(&mut attached, (("grid_cursor_goto", (2, 0, 0)),)),
        Err(ProtocolError::Invalid { event, reason: "unknown grid" }) if event == "grid_cursor_goto"
    ));
}
