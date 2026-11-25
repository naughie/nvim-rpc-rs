use nvim_rpc::mp::encode::{Primitive, WriterSyncExt as _};
use nvim_rpc::rpc::Message;
use nvim_rpc_ui::*;
use std::sync::Arc;

fn replay<P: Primitive>(model: &mut Model, params: P) -> Result<Vec<Frame>, ProtocolError> {
    let mut bytes = Vec::new();
    bytes.write_primitive((2u8, "redraw", params)).consume();
    let Message::Notification(n) = nvim_rpc::rpc::decode(&mut &bytes[..]).unwrap() else {
        panic!()
    };
    let mut frames = Vec::new();
    model.apply_redraw(n, |f| frames.push(f))?;
    Ok(frames)
}

fn notification(
    model: &mut Model,
    method: &str,
    params: impl Primitive,
) -> Result<(bool, Vec<Frame>), ProtocolError> {
    let mut bytes = Vec::new();
    bytes.write_primitive((2u8, method, params)).consume();
    let Message::Notification(n) = nvim_rpc::rpc::decode(&mut bytes.as_slice()).unwrap() else {
        panic!("expected notification");
    };
    let mut frames = Vec::new();
    let handled = model.apply_notification(n.method.as_slice(), n.params, |f| frames.push(f))?;
    Ok((handled, frames))
}

#[test]
fn api_errors_preserve_event_order_and_leave_unflushed_screen_changes_pending() {
    let mut model = Model::default();
    let (_, first) = notification(
        &mut model,
        "redraw",
        (
            ("grid_resize", (1, 1, 1)),
            ("grid_line", (1, 0, 0, (("a", 0),), false)),
            ("flush", ()),
        ),
    )
    .unwrap();
    let (handled, frames) = notification(&mut model, "custom", ("ignored",)).unwrap();
    assert!(!handled);
    assert!(frames.is_empty());
    let (handled, frames) = notification(
        &mut model,
        "redraw",
        (("bell", ()), ("grid_line", (1, 0, 0, (("b", 0),), false))),
    )
    .unwrap();
    assert!(handled);
    assert!(frames.is_empty());
    let (handled, errors) =
        notification(&mut model, "nvim_error_event", (1, "invalid call", 42)).unwrap();
    assert!(handled);
    let [error] = errors.as_slice() else {
        panic!("expected one error frame")
    };
    assert_eq!(error.sequence, 2);
    assert!(Arc::ptr_eq(&error.state, &first[0].state));
    assert!(error.damage.grids.is_empty());
    assert!(
        matches!(error.events.as_slice(), [Event::Bell, Event::RpcError { kind: 1, message }] if &**message == "invalid call")
    );

    let (_, final_frames) = notification(&mut model, "redraw", (("flush", ()),)).unwrap();
    let [last] = final_frames.as_slice() else {
        panic!("expected one final frame")
    };
    assert_eq!(last.sequence, 3);
    assert_eq!(&*last.state.grids[&GridId(1)].cell(0, 0).unwrap().text, "b");
    assert!(last.events.is_empty());
    assert_eq!(last.damage.grids.len(), 1);
}

#[test]
fn malformed_api_errors_stop_ui_updates_but_leave_other_methods_unhandled() {
    let mut model = Model::default();
    assert!(notification(&mut model, "nvim_error_event", (1,)).is_err());
    assert!(model.is_failed());
    let (handled, frames) = notification(&mut model, "custom", ()).unwrap();
    assert!(!handled);
    assert!(frames.is_empty());
    assert!(matches!(
        notification(&mut model, "redraw", (("flush", ()),)),
        Err(ProtocolError::Failed)
    ));

    let mut model = Model::new(nvim_rpc_ui::Limits {
        max_pending_events: 1,
        ..Default::default()
    });
    notification(&mut model, "redraw", (("bell", ()),)).unwrap();
    assert!(matches!(
        notification(&mut model, "nvim_error_event", (0, "error")),
        Err(ProtocolError::Limit("pending events"))
    ));
}

#[test]
fn notification_splits_repeated_calls_and_flushes_keep_snapshots_stable() {
    let mut model = Model::default();
    assert!(
        replay(
            &mut model,
            (
                ("grid_resize", (1, 4, 2)),
                (
                    "grid_line",
                    (1, 0, 0, (("a", 3, 2), ("界", 4), ("",)), false)
                )
            )
        )
        .unwrap()
        .is_empty()
    );
    let frames = replay(
        &mut model,
        (
            ("flush", ()),
            ("grid_line", (1, 0, 0, (("b", 5),), false)),
            ("flush", ()),
        ),
    )
    .unwrap();
    assert_eq!(frames.len(), 2);
    let before = &frames[0].state.grids[&GridId(1)];
    let after = &frames[1].state.grids[&GridId(1)];
    assert_eq!(&*before.cell(0, 0).unwrap().text, "a");
    assert_eq!(&*after.cell(0, 0).unwrap().text, "b");
    assert_eq!(&*after.cell(0, 2).unwrap().text, "界");
    assert_eq!(&*after.cell(0, 3).unwrap().text, "");
    assert_eq!(before.cell(0, 1).unwrap().highlight, HighlightId(3));
    assert!(Arc::ptr_eq(&before.rows[1], &after.rows[1]));
    assert!(!Arc::ptr_eq(&before.rows[0], &after.rows[0]));
    assert_eq!((frames[0].sequence, frames[1].sequence), (1, 2));
    assert_eq!(
        frames[1].damage.grids[0],
        GridDamage {
            grid: GridId(1),
            top: 0,
            bottom: 1,
            left: 0,
            right: 1
        }
    );
}

#[test]
fn scroll_regions_copy_overlapping_cells_and_leave_exposed_cells_for_following_lines() {
    let mut model = Model::default();
    replay(
        &mut model,
        (
            ("grid_resize", (1, 4, 3)),
            (
                "grid_line",
                (1, 0, 0, (("a", 0, 4),), false),
                (1, 1, 0, (("b", 0, 4),), false),
                (1, 2, 0, (("c", 0, 4),), false),
            ),
            ("flush", ()),
        ),
    )
    .unwrap();
    let frames = replay(
        &mut model,
        (("grid_scroll", (1, 0, 3, 1, 3, 1, 0)), ("flush", ())),
    )
    .unwrap();
    let grid = &frames[0].state.grids[&GridId(1)];
    assert_eq!(&*grid.cell(0, 0).unwrap().text, "a");
    assert_eq!(&*grid.cell(0, 1).unwrap().text, "b");
    assert_eq!(&*grid.cell(1, 1).unwrap().text, "c");
    assert_eq!(&*grid.cell(2, 1).unwrap().text, "c");
    let frames = replay(
        &mut model,
        (("grid_scroll", (1, 0, 3, 0, 4, -1, 0)), ("flush", ())),
    )
    .unwrap();
    let grid = &frames[0].state.grids[&GridId(1)];
    assert_eq!(&*grid.cell(1, 0).unwrap().text, "a");
    assert_eq!(&*grid.cell(2, 0).unwrap().text, "b");
}

#[test]
fn cmdline_nesting_popup_byte_anchor_and_wildmenu_are_retained() {
    let mut model = Model::default();
    let frames = replay(
        &mut model,
        (
            (
                "cmdline_show",
                (((1, "日本語"),), 3, ":", "prompt", 0, 1),
                (((2, "42"),), 2, "=", "", 0, 2),
            ),
            ("cmdline_pos", (6, 1)),
            ("cmdline_special_char", ("^", true, 1)),
            ("cmdline_hide", (2, false)),
            ("cmdline_block_show", ((((0, "line 1"),),),)),
            ("cmdline_block_append", (((0, "line 2"),),)),
            (
                "popupmenu_show",
                ((("word", "kind", "menu", "info"),), -1, 0, 3, -1),
            ),
            ("popupmenu_select", (0,)),
            ("wildmenu_show", (("one", "two"),)),
            ("wildmenu_select", (1,)),
            ("flush", ()),
        ),
    )
    .unwrap();
    let s = &frames[0].state;
    assert_eq!(s.cmdlines.len(), 1);
    assert_eq!(s.cmdlines[&1].pos, 6);
    assert_eq!(s.cmdlines[&1].special.as_ref().unwrap().0.as_ref(), "^");
    assert_eq!(s.cmdline_block.len(), 2);
    assert!(matches!(
        s.popupmenu.as_ref().unwrap().anchor,
        PopupAnchor::Cmdline { byte_offset: 3 }
    ));
    assert_eq!(s.popupmenu.as_ref().unwrap().selected, Some(0));
    assert_eq!(s.wildmenu.as_ref().unwrap().selected, Some(1));
    let frames = replay(
        &mut model,
        (
            ("cmdline_block_hide", ()),
            ("popupmenu_hide", ()),
            ("wildmenu_hide", ()),
            ("flush", ()),
        ),
    )
    .unwrap();
    assert!(frames[0].state.cmdline_block.is_empty());
    assert!(frames[0].state.popupmenu.is_none());
    assert!(frames[0].state.wildmenu.is_none());
    assert_eq!(s.cmdline_block.len(), 2);
}

#[test]
fn messages_preserve_channels_replace_append_and_history() {
    let mut model = Model::default();
    let frames = replay(
        &mut model,
        (
            (
                "msg_show",
                ("echo", ((1, "first"),), false, true, false, 1, "typed_cmd"),
                ("echo", ((1, "+"),), false, false, true, 2, ""),
                ("echo", ((1, "replacement"),), true, false, false, 3, ""),
                ("echo", ((1, "updated"),), false, false, false, 3, ""),
            ),
            ("msg_showmode", (((0, "INSERT"),),)),
            ("msg_showcmd", (((0, "g"),),)),
            ("msg_ruler", (((0, "1,1"),),)),
            (
                "msg_history_show",
                ((("echo", ((0, "old"),), false),), true),
            ),
            ("flush", ()),
        ),
    )
    .unwrap();
    let m = &frames[0].state.messages;
    assert_eq!(m.visible.len(), 1);
    assert_eq!(&*m.visible[0].content[0].text, "updated");
    assert_eq!(&*m.showmode[0].text, "INSERT");
    assert_eq!(&*m.showcmd[0].text, "g");
    assert_eq!(&*m.ruler[0].text, "1,1");
    assert_eq!(m.history.as_ref().unwrap().len(), 1);
    assert!(m.history_prev_cmd);
    let frames = replay(&mut model, (("msg_clear", ()), ("flush", ()))).unwrap();
    assert!(frames[0].state.messages.visible.is_empty());
    assert!(!frames[0].state.messages.showmode.is_empty());
}

fn replay_message_bytes(
    model: &mut Model,
    name: &str,
    text: &[u8],
) -> Result<Vec<Frame>, ProtocolError> {
    let placeholder = "X".repeat(text.len());
    let mut bytes = Vec::new();
    bytes
        .write_primitive((
            2u8,
            "redraw",
            (
                (name, (((7, placeholder.as_str(), 42), (3, "界")),)),
                ("set_title", ("after showcmd",)),
                ("flush", ()),
            ),
        ))
        .consume();

    if !text.is_empty() {
        let pos = bytes
            .windows(text.len())
            .position(|v| v == placeholder.as_bytes())
            .unwrap();
        bytes[pos..pos + text.len()].copy_from_slice(text);
    }

    let Message::Notification(notification) = nvim_rpc::rpc::decode(&mut bytes.as_slice()).unwrap()
    else {
        panic!("expected redraw notification");
    };

    let mut frames = Vec::new();
    model.apply_redraw(notification, |frame| frames.push(frame))?;

    Ok(frames)
}

#[test]
fn showcmd_escapes_invalid_bytes_and_preserves_utf8_and_chunk_metadata() {
    for (bytes, expected) in [
        (b"".as_slice(), ""),
        (b"g\\q\n\0", "g\\q\n\0"),
        ("界😀".as_bytes(), "界😀"),
        (b"~@\xfd", r"~@\xfd"),
        (b"\xffend", r"\xffend"),
        (
            b"\xe7\x95\x8c\xfd\xc3\xa9\x80\xf0\x9f\x98\x80",
            "界\\xfdé\\x80😀",
        ),
        (b"\xe2(\xa1", r"\xe2(\xa1"),
        (b"\xf0\x9f", r"\xf0\x9f"),
        (b"\xed\xa0\x80", r"\xed\xa0\x80"),
    ] {
        let mut model = Model::default();
        let frames = replay_message_bytes(&mut model, "msg_showcmd", bytes).unwrap();

        assert!(!model.is_failed(), "{bytes:?}");
        assert_eq!(frames.len(), 1);
        assert_eq!(
            frames[0].state.messages.showcmd,
            [
                Chunk {
                    highlight: HighlightId(7),
                    text: Arc::from(expected),
                    hl_id: Some(42),
                },
                Chunk {
                    highlight: HighlightId(3),
                    text: Arc::from("界"),
                    hl_id: None,
                },
            ],
            "{bytes:?}"
        );
        assert!(frames[0].damage.widgets);
        assert_eq!(frames[0].state.title.as_ref(), "after showcmd");
        assert!(matches!(
            frames[0].events.as_slice(),
            [Event::SetTitle(title)] if &**title == "after showcmd"
        ));
    }
}

#[test]
fn invalid_showcmd_does_not_stop_later_redraws_or_change_retained_frames() {
    let mut model = Model::default();
    let first = replay_message_bytes(&mut model, "msg_showcmd", b"~@\xfd").unwrap();
    let frames = replay(
        &mut model,
        (
            ("msg_showcmd", ((),)),
            ("set_title", ("next redraw",)),
            ("flush", ()),
        ),
    )
    .unwrap();

    assert!(!model.is_failed());
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0].sequence, 2);
    assert!(frames[0].state.messages.showcmd.is_empty());
    assert_eq!(frames[0].state.title.as_ref(), "next redraw");
    assert_eq!(first[0].state.messages.showcmd[0].text.as_ref(), r"~@\xfd");
}

#[test]
fn showcmd_checks_string_limits_after_escaping() {
    let limits = Limits {
        max_string_bytes: 16,
        ..Limits::default()
    };

    for (bytes, expected) in [
        (b"123456789012\xfd".as_slice(), r"123456789012\xfd"),
        (b"\xfd\xfd\xfd\xfd", r"\xfd\xfd\xfd\xfd"),
    ] {
        let mut model = Model::new(limits);
        let frames = replay_message_bytes(&mut model, "msg_showcmd", bytes).unwrap();

        assert_eq!(frames[0].state.messages.showcmd[0].text.as_ref(), expected);
    }

    for bytes in [
        b"1234567890123\xfd".as_slice(),
        b"\xfd\xfd\xfd\xfd\xfd",
        b"12345678901234567",
        b"1234567890123456\xfd",
    ] {
        let mut model = Model::new(limits);

        assert!(matches!(
            replay_message_bytes(&mut model, "msg_showcmd", bytes),
            Err(ProtocolError::Limit("string bytes"))
        ));
        assert!(model.is_failed());
    }
}

#[test]
fn other_message_displays_still_reject_invalid_utf8() {
    for name in ["msg_showmode", "msg_ruler"] {
        let mut model = Model::default();

        assert!(matches!(
            replay_message_bytes(&mut model, name, b"~@\xfd"),
            Err(ProtocolError::Decode(
                nvim_rpc::api_types::DecodeError::InvalidUtf8(_)
            ))
        ));
        assert!(model.is_failed());
    }
}

#[test]
fn unknown_events_and_appended_arguments_are_forward_compatible() {
    let mut model = Model::default();
    let frames = replay(
        &mut model,
        (
            ("grid_resize", (1, 2, 2, "extension")),
            ("future_event", ("hello", 42)),
            ("bell", ()),
            ("flush", ("future",)),
        ),
    )
    .unwrap();
    assert!(
        matches!(&frames[0].events[0], Event::Unknown {name,args} if &**name == "future_event" && args.len() == 2)
    );
    assert!(matches!(frames[0].events[1], Event::Bell));
}

#[test]
fn grid_and_collection_limits_fail_without_publishing_partial_frames() {
    let mut model = Model::new(Limits {
        max_cells: 6,
        ..Limits::default()
    });
    replay(&mut model, (("grid_resize", (1, 2, 2)),)).unwrap();
    assert!(matches!(
        replay(&mut model, (("grid_resize", (2, 2, 2)), ("flush", ()))),
        Err(ProtocolError::Limit("grid cells"))
    ));
    assert!(model.is_failed());
    assert!(matches!(
        replay(&mut model, (("flush", ()),)),
        Err(ProtocolError::Failed)
    ));
    let mut model = Model::new(Limits {
        max_pending_events: 1,
        ..Limits::default()
    });
    assert!(replay(&mut model, (("bell", (), ()), ("flush", ()))).is_err());
}

#[test]
fn malformed_known_calls_poison_model_without_panics() {
    let mut m = Model::default();
    assert!(replay(&mut m, (("grid_resize", (1,)),)).is_err());
    let mut m = Model::default();
    replay(&mut m, (("grid_resize", (1, 2, 2)),)).unwrap();
    assert!(replay(&mut m, (("grid_line", (1, 0, 1, (("x", 0, 2),), false)),)).is_err());
    let mut m = Model::default();
    assert!(
        replay(
            &mut m,
            (("cmdline_show", (((0, "日"),), 1, ":", "", 0, 1)),)
        )
        .is_err()
    );
    let mut m = Model::default();
    assert!(matches!(
        replay(&mut m, (("put", ("x",)),)),
        Err(ProtocolError::LegacyGrid)
    ));
}

#[test]
fn multigrid_layout_viewport_and_window_lifecycle_are_independent_of_cells() {
    let mut m = Model::default();
    let frames = replay(
        &mut m,
        (
            ("grid_resize", (1, 20, 10), (2, 10, 5)),
            ("win_pos", (2, 1000, 1, 2, 10, 5)),
            ("win_viewport", (2, 1000, 2, 20, 3, 4, 100, 2)),
            ("win_viewport_margins", (2, 1000, 1, 0, 2, 0)),
            ("grid_cursor_goto", (2, 1, 1)),
            ("win_hide", (2,)),
            ("flush", ()),
        ),
    )
    .unwrap();
    let s = &frames[0].state;
    assert!(s.placements[&GridId(2)].hidden);
    assert_eq!(s.viewports[&GridId(2)].margins, [1, 0, 2, 0]);
    assert_eq!(s.cursor.unwrap().grid, GridId(2));
    let f = replay(
        &mut m,
        (
            (
                "win_float_pos",
                (2, 1000, "NW", 1, 2.5, 3.0, true, 50, 1, 2, 3),
            ),
            ("flush", ()),
        ),
    )
    .unwrap();
    assert!(!f[0].state.placements[&GridId(2)].hidden);
    assert!(matches!(
        f[0].state.placements[&GridId(2)].kind,
        GridPlacementKind::Floating {
            compindex: Some(1),
            ..
        }
    ));
    let f = replay(&mut m, (("win_close", (2,)), ("flush", ()))).unwrap();
    assert!(f[0].state.grids.contains_key(&GridId(2)));
    assert!(!f[0].state.placements.contains_key(&GridId(2)));
    let f = replay(&mut m, (("grid_destroy", (2,)), ("flush", ()))).unwrap();
    assert!(!f[0].state.grids.contains_key(&GridId(2)));
    assert!(f[0].state.cursor.is_none());
    assert_eq!(f[0].damage.removed_grids, [GridId(2)]);
    assert!(s.grids.contains_key(&GridId(2)));
}

#[test]
fn unseen_grid_destruction_preserves_later_frames_and_events() {
    let mut model = Model::default();
    let frames = replay(
        &mut model,
        (
            ("grid_resize", (1, 80, 24)),
            ("flush", ()),
            ("win_close", (4,)),
            ("grid_destroy", (4,)),
            ("set_title", ("after destroy",)),
            ("flush", ()),
        ),
    )
    .unwrap();

    assert!(!model.is_failed());
    assert_eq!(frames.len(), 2);
    assert_eq!(frames[1].sequence, 2);
    assert!(!frames[1].state.grids.contains_key(&GridId(4)));
    assert!(Arc::ptr_eq(
        &frames[0].state.grids[&GridId(1)],
        &frames[1].state.grids[&GridId(1)],
    ));
    assert_eq!(frames[1].damage.removed_grids, [GridId(4)]);
    assert!(frames[1].damage.layout);
    assert!(matches!(
        frames[1].events.as_slice(),
        [Event::SetTitle(title)] if &**title == "after destroy"
    ));

    let later = replay(
        &mut model,
        (
            ("grid_line", (1, 0, 0, (("x", 0),), false)),
            ("bell", ()),
            ("flush", ()),
        ),
    )
    .unwrap();

    assert_eq!(later.len(), 1);
    assert_eq!(later[0].sequence, 3);
    assert_eq!(
        later[0].state.grids[&GridId(1)]
            .cell(0, 0)
            .unwrap()
            .text
            .as_ref(),
        "x"
    );
    assert!(matches!(later[0].events.as_slice(), [Event::Bell]));
    assert_eq!(
        frames[0].state.grids[&GridId(1)]
            .cell(0, 0)
            .unwrap()
            .text
            .as_ref(),
        " "
    );
}

#[test]
fn grid_destruction_clears_window_state_with_or_without_cells() {
    for resized in [false, true] {
        let mut model = Model::default();
        replay(
            &mut model,
            (("grid_resize", (1, 2, 2)), ("grid_cursor_goto", (1, 0, 0))),
        )
        .unwrap();

        if resized {
            replay(
                &mut model,
                (("grid_resize", (4, 2, 2)), ("grid_cursor_goto", (4, 0, 0))),
            )
            .unwrap();
        }

        let before = replay(
            &mut model,
            (
                ("win_float_pos", (4, 1000, "NW", 1, 1.0, 1.0, true, 50)),
                ("win_viewport", (4, 1000, 0, 2, 0, 0, 2, 0)),
                ("flush", ()),
            ),
        )
        .unwrap();
        let after = replay(&mut model, (("grid_destroy", (4,)), ("flush", ()))).unwrap();

        assert!(!model.is_failed());
        assert!(!after[0].state.grids.contains_key(&GridId(4)));
        assert!(!after[0].state.placements.contains_key(&GridId(4)));
        assert!(!after[0].state.viewports.contains_key(&GridId(4)));
        assert_eq!(
            after[0].state.cursor.map(|c| c.grid),
            if resized { None } else { Some(GridId(1)) }
        );
        assert_eq!(after[0].damage.cursor, resized);
        assert_eq!(after[0].damage.removed_grids, [GridId(4)]);
        assert!(after[0].damage.layout);
        assert_eq!(before[0].state.grids.contains_key(&GridId(4)), resized);
        assert!(before[0].state.placements.contains_key(&GridId(4)));
        assert!(before[0].state.viewports.contains_key(&GridId(4)));
    }
}

#[test]
fn shell_cursor_below_default_grid_preserves_redraw_progress() {
    let mut model = Model::default();
    replay(
        &mut model,
        (
            ("grid_resize", (1, 80, 24)),
            ("grid_cursor_goto", (1, 0, 0)),
            ("flush", ()),
        ),
    )
    .unwrap();

    let shell = replay(
        &mut model,
        (
            ("grid_cursor_goto", (1, 24, 0)),
            (
                "msg_show",
                ("shell_out", ((0, "ok"),), false, false, false, 1, ""),
            ),
            ("flush", ()),
        ),
    )
    .unwrap();
    let later = replay(
        &mut model,
        (("grid_cursor_goto", (1, 23, 7)), ("flush", ())),
    )
    .unwrap();

    assert!(!model.is_failed());
    let [shell] = shell.as_slice() else {
        panic!("expected one shell output frame");
    };
    let [later] = later.as_slice() else {
        panic!("expected one later frame");
    };
    assert_eq!((shell.sequence, later.sequence), (2, 3));
    let message = &shell.state.messages.visible[0];
    assert_eq!(&*message.content[0].text, "ok");
    assert!(!shell.damage.cursor);
    let cursor = shell.state.cursor.unwrap();
    assert_eq!((cursor.grid, cursor.row, cursor.col), (GridId(1), 0, 0));
    assert!(later.damage.cursor);
    let cursor = later.state.cursor.unwrap();
    assert_eq!((cursor.grid, cursor.row, cursor.col), (GridId(1), 23, 7));
}

#[test]
fn rejects_other_out_of_bounds_grid_cursor_moves() {
    for (id, row, col) in [(1, 25, 0), (1, 24, 1), (1, 23, 80), (2, 24, 0)] {
        let mut model = Model::default();
        let result = replay(
            &mut model,
            (
                ("grid_resize", (id, 80, 24)),
                ("grid_cursor_goto", (id, row, col)),
            ),
        );
        assert!(
            matches!(result, Err(ProtocolError::Invalid { event, reason: "cursor outside grid" }) if event == "grid_cursor_goto"),
            "unexpected result for ({id}, {row}, {col})"
        );
        assert!(model.is_failed());
    }
}

#[test]
fn highlights_modes_tabline_options_and_global_events_are_typed() {
    use nvim_rpc::mp::encode::primitive::Map;
    let mut m = Model::default();
    let f = replay(
        &mut m,
        (
            (
                "hl_attr_define",
                (
                    3,
                    Map([("foreground", 0xaabbcc), ("background", -1)]),
                    Map([("foreground", 2)]),
                    (Map([("kind", "syntax"), ("hi_name", "String")]),),
                ),
            ),
            ("hl_group_set", ("Pmenu", 3)),
            ("default_colors_set", (0xdddddd, 0x111111, -1, 7, 0)),
            ("mode_info_set", (true, (Map([("cursor_shape", "block")]),))),
            ("mode_change", ("insert", 0)),
            ("option_set", ("ext_multigrid", true)),
            (
                "tabline_update",
                (1, (Map([("tab", 1)]),), 2, (Map([("buffer", 2)]),)),
            ),
            ("set_title", ("editor",)),
            ("set_icon", ("icon",)),
            ("chdir", ("/tmp",)),
            ("mouse_on", ()),
            ("busy_start", ()),
            ("flush", ()),
        ),
    )
    .unwrap();
    let s = &f[0].state;
    assert_eq!(s.highlights[&HighlightId(3)].rgb.foreground, Some(0xaabbcc));
    assert_eq!(s.highlights[&HighlightId(3)].rgb.background, None);
    assert_eq!(s.highlights[&HighlightId(3)].info.len(), 1);
    assert_eq!(s.highlight_groups["Pmenu"], HighlightId(3));
    assert_eq!(s.options["ext_multigrid"], Value::Bool(true));
    assert_eq!(s.default_colors.foreground, Some(0xdddddd));
    assert!(s.mode.cursor_style_enabled);
    assert_eq!(&*s.mode.name, "insert");
    assert_eq!(s.tabline.as_ref().unwrap().current.id(), 1);
    assert_eq!(s.tabline.as_ref().unwrap().buffers[0].buffer.id(), 2);
    assert!(s.mouse_enabled && s.busy);
    assert_eq!(&*s.title, "editor");
    assert_eq!(f[0].events.len(), 3);
}

#[test]
fn repeated_dirty_calls_and_grid_lifecycle_keep_damage_bounded() {
    let mut m = Model::new(Limits {
        max_grids: 2,
        ..Limits::default()
    });
    for id in 1..100 {
        replay(
            &mut m,
            (
                ("grid_resize", (id, 1, 1)),
                ("grid_line", (id, 0, 0, (("x", 0),), false)),
                ("grid_destroy", (id,)),
            ),
        )
        .unwrap();
    }
    let f = replay(&mut m, (("flush", ()),)).unwrap();
    assert!(f[0].damage.full);
    assert!(f[0].damage.grids.len() <= 2);
    assert!(f[0].damage.removed_grids.len() <= 2);
}

#[test]
fn malformed_scalar_argument_mutations_never_panic() {
    // Exercise every known call using each scalar type as its first argument.
    let names = [
        "grid_resize",
        "grid_clear",
        "grid_destroy",
        "grid_cursor_goto",
        "grid_scroll",
        "grid_line",
        "win_pos",
        "win_float_pos",
        "win_external_pos",
        "win_hide",
        "win_close",
        "msg_set_pos",
        "win_viewport",
        "win_viewport_margins",
        "win_extmark",
        "cmdline_show",
        "cmdline_pos",
        "cmdline_special_char",
        "cmdline_hide",
        "cmdline_block_show",
        "cmdline_block_append",
        "popupmenu_show",
        "popupmenu_select",
        "wildmenu_show",
        "wildmenu_select",
        "tabline_update",
        "msg_show",
        "msg_showmode",
        "msg_showcmd",
        "msg_ruler",
        "msg_history_show",
        "mode_info_set",
        "mode_change",
        "option_set",
        "hl_attr_define",
        "hl_group_set",
        "default_colors_set",
        "set_title",
        "set_icon",
        "chdir",
        "connect",
        "restart",
        "screenshot",
        "ui_send",
        "error_exit",
    ];
    for name in names {
        let _ = replay(&mut Model::default(), ((name, (i64::MIN,)),));
        let _ = replay(&mut Model::default(), ((name, (i64::MAX,)),));
        let _ = replay(&mut Model::default(), ((name, (false,)),));
        let _ = replay(&mut Model::default(), ((name, ("not a number",)),));
        let _ = replay(&mut Model::default(), ((name, ((),)),));
    }
}

#[test]
fn zero_repeat_runs_from_live_neovim_do_not_advance_columns() {
    let mut m = Model::default();
    let f = replay(
        &mut m,
        (
            ("grid_resize", (1, 2, 1)),
            ("grid_line", (1, 0, 0, ((" ", 3, 0), ("a",), ("b",)), false)),
            ("flush", ()),
        ),
    )
    .unwrap();
    let g = &f[0].state.grids[&GridId(1)];
    assert_eq!(&*g.cell(0, 0).unwrap().text, "a");
    assert_eq!(&*g.cell(0, 1).unwrap().text, "b");
    assert_eq!(g.cell(0, 0).unwrap().highlight, HighlightId(3));
}

#[test]
fn same_width_resize_shares_unchanged_rows_and_ascii_cells_share_text() {
    let mut m = Model::default();
    let before = replay(
        &mut m,
        (
            ("grid_resize", (1, 2, 1)),
            ("grid_line", (1, 0, 0, (("a", 0), ("a",)), false)),
            ("flush", ()),
        ),
    )
    .unwrap();
    let after = replay(&mut m, (("grid_resize", (1, 2, 2)), ("flush", ()))).unwrap();
    let a = &before[0].state.grids[&GridId(1)];
    let b = &after[0].state.grids[&GridId(1)];
    assert!(Arc::ptr_eq(&a.rows[0], &b.rows[0]));
    assert!(Arc::ptr_eq(
        &a.cell(0, 0).unwrap().text,
        &a.cell(0, 1).unwrap().text
    ));
}

#[test]
fn unicode_cell_reuse_preserves_text_runs_and_retained_snapshots() {
    for text in ["界", "日本語", "é", "e\u{301}", "👩\u{200d}💻", "ab"] {
        let mut model = Model::default();
        let first = replay(
            &mut model,
            (
                ("grid_resize", (1, 4, 1)),
                (
                    "grid_line",
                    (1, 0, 0, ((text, 3, 2), (text,), ("", 4)), false),
                ),
                ("flush", ()),
            ),
        )
        .unwrap();
        let second = replay(
            &mut model,
            (
                ("grid_line", (1, 0, 0, ((text, 5, 3),), true)),
                ("flush", ()),
            ),
        )
        .unwrap();
        let before = &first[0].state.grids[&GridId(1)];
        let after = &second[0].state.grids[&GridId(1)];
        let a = before.cell(0, 0).unwrap();
        for col in 0..3 {
            let b = before.cell(0, col).unwrap();
            let c = after.cell(0, col).unwrap();
            assert_eq!(b.text.as_bytes(), text.as_bytes());
            assert_eq!(c.text.as_bytes(), text.as_bytes());
            assert_eq!(b.highlight, HighlightId(3));
            assert_eq!(c.highlight, HighlightId(5));
            assert!(Arc::ptr_eq(&a.text, &b.text));
            assert!(Arc::ptr_eq(&a.text, &c.text));
        }
        assert_eq!(before.cell(0, 3).unwrap().text.as_ref(), "");
        assert_eq!(before.cell(0, 3).unwrap().highlight, HighlightId(4));
        assert!(!before.rows[0].wrap);
        assert!(after.rows[0].wrap);

        let mut other = Model::default();
        let other_frame = replay(
            &mut other,
            (
                ("grid_resize", (1, 1, 1)),
                ("grid_line", (1, 0, 0, ((text, 0),), false)),
                ("flush", ()),
            ),
        )
        .unwrap();
        assert!(!Arc::ptr_eq(
            &a.text,
            &other_frame[0].state.grids[&GridId(1)]
                .cell(0, 0)
                .unwrap()
                .text
        ));

        for n in 0..8192 {
            let replacement = format!("字{n}");
            replay(
                &mut model,
                (("grid_line", (1, 0, 0, ((replacement.as_str(), 0),), false)),),
            )
            .unwrap();
        }
        drop(model);
        assert_eq!(before.cell(0, 0).unwrap().text.as_bytes(), text.as_bytes());
        assert_eq!(after.cell(0, 0).unwrap().text.as_bytes(), text.as_bytes());
    }
}

#[test]
fn cached_cells_still_validate_utf8_and_string_byte_limits() {
    let mut model = Model::new(Limits {
        max_string_bytes: 16,
        ..Limits::default()
    });
    let frames = replay(
        &mut model,
        (
            ("grid_resize", (1, 1, 1)),
            ("grid_line", (1, 0, 0, (("😀😀😀😀", 0),), false)),
            ("flush", ()),
        ),
    )
    .unwrap();
    assert_eq!(
        frames[0].state.grids[&GridId(1)]
            .cell(0, 0)
            .unwrap()
            .text
            .as_ref(),
        "😀😀😀😀"
    );
    assert!(matches!(
        replay(
            &mut model,
            (("grid_line", (1, 0, 0, (("😀😀😀😀x", 0),), false)),)
        ),
        Err(ProtocolError::Limit("string bytes"))
    ));
    assert!(model.is_failed());

    let mut model = Model::default();
    replay(
        &mut model,
        (
            ("grid_resize", (1, 1, 1)),
            ("grid_line", (1, 0, 0, (("界", 0),), false)),
        ),
    )
    .unwrap();
    let mut bytes = Vec::new();
    bytes
        .write_primitive((
            2u8,
            "redraw",
            (("grid_line", (1, 0, 0, (("界", 0),), false)),),
        ))
        .consume();
    let pos = bytes
        .windows("界".len())
        .position(|v| v == "界".as_bytes())
        .unwrap();
    bytes[pos] = 0xff;
    let Message::Notification(notification) = nvim_rpc::rpc::decode(&mut bytes.as_slice()).unwrap()
    else {
        panic!("expected redraw notification");
    };
    assert!(matches!(
        model.apply_redraw(notification, |_| panic!("unexpected flush")),
        Err(ProtocolError::Decode(
            nvim_rpc::api_types::DecodeError::InvalidUtf8(_)
        ))
    ));
    assert!(model.is_failed());
}

#[test]
fn overlapping_scrolls_match_snapshot_copies_for_subregions_and_extreme_deltas() {
    fn seeded() -> (Model, Frame) {
        let mut model = Model::default();
        let mut frames = replay(
            &mut model,
            (
                ("grid_resize", (1, 5, 5)),
                (
                    "grid_line",
                    (
                        1,
                        0,
                        0,
                        (("a", 0), ("b", 1), ("c", 2), ("d", 3), ("e", 4)),
                        false,
                    ),
                    (
                        1,
                        1,
                        0,
                        (("f", 5), ("g", 6), ("h", 7), ("i", 8), ("j", 9)),
                        true,
                    ),
                    (
                        1,
                        2,
                        0,
                        (("k", 10), ("l", 11), ("m", 12), ("n", 13), ("o", 14)),
                        false,
                    ),
                    (
                        1,
                        3,
                        0,
                        (("p", 15), ("q", 16), ("r", 17), ("s", 18), ("t", 19)),
                        true,
                    ),
                    (
                        1,
                        4,
                        0,
                        (("u", 20), ("v", 21), ("w", 22), ("x", 23), ("y", 24)),
                        false,
                    ),
                ),
                ("flush", ()),
            ),
        )
        .unwrap();
        (model, frames.pop().unwrap())
    }

    let deltas = [i64::MIN, -5, -2, -1, 0, 1, 2, 5, i64::MAX];
    for (top, bottom, left, right) in [(0, 5, 0, 5), (1, 4, 1, 4), (2, 2, 1, 4), (1, 4, 2, 2)] {
        for rows in deltas {
            for cols in deltas {
                let (mut model, initial) = seeded();
                let original = &initial.state.grids[&GridId(1)];
                let snapshot: Vec<_> = original.rows.iter().map(|r| r.cells.clone()).collect();
                let mut expected = snapshot.clone();
                let mut wraps: Vec<_> = original.rows.iter().map(|r| r.wrap).collect();
                for row in top..bottom {
                    let src_row = row as i128 + i128::from(rows);
                    if !(top as i128..bottom as i128).contains(&src_row) {
                        continue;
                    }
                    for (col, cell) in expected[row].iter_mut().enumerate().take(right).skip(left) {
                        let src_col = col as i128 + i128::from(cols);
                        if (left as i128..right as i128).contains(&src_col) {
                            *cell = original.rows[src_row as usize].cells[src_col as usize].clone();
                        }
                    }
                    if left == 0 && right == original.width && cols == 0 {
                        wraps[row] = original.rows[src_row as usize].wrap;
                    }
                }

                let frames = replay(
                    &mut model,
                    (
                        ("grid_scroll", (1, top, bottom, left, right, rows, cols)),
                        ("flush", ()),
                    ),
                )
                .unwrap();
                let result = &frames[0].state.grids[&GridId(1)];
                for row in 0..5 {
                    assert_eq!(
                        result.rows[row].cells, expected[row],
                        "row {row}, region {top}..{bottom}/{left}..{right}, scroll {rows}/{cols}"
                    );
                    assert_eq!(
                        result.rows[row].wrap, wraps[row],
                        "row {row}, scroll {rows}/{cols}"
                    );
                    assert_eq!(original.rows[row].cells, snapshot[row]);
                }
            }
        }
    }
}

#[test]
fn wrap_only_grid_lines_mark_the_row_and_preserve_cells() {
    let mut model = Model::default();
    let initial = replay(
        &mut model,
        (
            ("grid_resize", (1, 4, 2)),
            (
                "grid_line",
                (1, 1, 0, (("a", 0), ("b", 1), ("c", 2), ("d", 3)), false),
            ),
            ("flush", ()),
        ),
    )
    .unwrap();
    let original = &initial[0].state.grids[&GridId(1)];

    for wrap in [true, false] {
        let frames = replay(
            &mut model,
            (("grid_line", (1, 1, 4, (), wrap)), ("flush", ())),
        )
        .unwrap();
        let result = &frames[0].state.grids[&GridId(1)];
        assert_eq!(result.rows[1].wrap, wrap);
        assert_eq!(result.rows[1].cells, original.rows[1].cells);
        assert!(Arc::ptr_eq(&original.rows[0], &result.rows[0]));
        assert!(!original.rows[1].wrap);
        assert_eq!(
            frames[0].damage.grids,
            [GridDamage {
                grid: GridId(1),
                top: 1,
                bottom: 2,
                left: 0,
                right: 4
            }]
        );
    }
}

#[test]
fn event_names_obey_string_limits_before_retention() {
    let mut model = Model::new(Limits {
        max_string_bytes: 8,
        ..Limits::default()
    });
    assert!(matches!(
        replay(&mut model, (("future_event", ()), ("flush", ()))),
        Err(ProtocolError::Limit("event name bytes"))
    ));
    assert!(model.is_failed());
}

#[test]
fn ui_send_retains_arbitrary_terminal_bytes() {
    let bytes = b"\x93\x02\xa6redraw\x92\x92\xa7ui_send\x91\xa2\x00\xff\x92\xa5flush\x90";
    let Message::Notification(notification) = nvim_rpc::rpc::decode(&mut &bytes[..]).unwrap()
    else {
        panic!("expected redraw notification");
    };
    let mut model = Model::default();
    let mut frames = Vec::new();
    model
        .apply_redraw(notification, |frame| frames.push(frame))
        .unwrap();
    assert_eq!(frames.len(), 1);
    let [Event::UiSend(content)] = frames[0].events.as_slice() else {
        panic!("expected terminal output event");
    };
    assert_eq!(&**content, b"\x00\xff");
    assert!(!model.is_failed());
}
