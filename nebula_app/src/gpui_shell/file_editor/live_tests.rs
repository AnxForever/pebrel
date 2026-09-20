//! Native input, source preservation and cross-mode history regression tests.

use super::*;
use gpui::{EntityInputHandler, Modifiers, TestAppContext};

#[gpui::test]
fn unchanged_activation_skips_parse_without_losing_an_earlier_pending_edit(
    cx: &mut TestAppContext,
) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("activation.md");
    std::fs::write(&path, "# Heading\n\n中文段落\n").unwrap();
    let (file, mut cx) = tests::open(path, cx);
    cx.update(|_, cx| file.update(cx, |view, cx| view.finish_live_edit(cx)));
    cx.run_until_parked();
    cx.update(|window, cx| {
        file.update(cx, |view, cx| {
            let revision = view.revision;
            for _ in 0..10 {
                view.begin_live_edit(0, window, cx);
                view.finish_live_edit(cx);
                view.begin_live_edit(1, window, cx);
                view.finish_live_edit(cx);
            }
            assert_eq!(view.revision, revision, "unchanged activations must not schedule parsing");
            view.begin_live_edit(1, window, cx);
        });
    });
    cx.simulate_input("新内容");
    cx.run_until_parked();
    cx.update(|window, cx| {
        file.update(cx, |view, cx| {
            view.finish_live_edit(cx);
            assert!(view.preview_stale);
            view.begin_live_edit(0, window, cx);
            view.finish_live_edit(cx);
        });
    });
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_millis(300));
    cx.run_until_parked();
    file.read_with(&cx, |view, cx| {
        assert!(!view.preview_stale);
        assert!(view.outline.blocks[1].contains("新内容"));
        assert_eq!(
            view.source_slice(view.outline.source_ranges[1].clone(), cx).as_deref(),
            Some(view.outline.blocks[1].as_str())
        );
    });
}

#[gpui::test]
fn formatted_edit_saves_source_without_rewriting_other_blocks(cx: &mut TestAppContext) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("inline.md");
    let original = "## 中文 **标题** ##\n\n[link](https://example.test \"title\")\n\n~~~rust\nlet n = 1;\n~~~\n";
    std::fs::write(&path, original).unwrap();
    let (file, mut cx) = tests::open(path.clone(), cx);
    let input = cx.update(|window, cx| {
        file.update(cx, |view, cx| {
            view.live_mode = true;
            view.begin_live_edit(0, window, cx);
            let edit = view.live_edit.as_ref().unwrap();
            assert_eq!(edit.input.read(cx).value(), "中文 标题");
            edit.input.update(cx, |input, cx| input.set_selected_range(7..13, cx));
            edit.input.clone()
        })
    });
    cx.simulate_input("新标题🌿");
    cx.run_until_parked();
    assert!(file.read_with(&cx, |view, _| view.live_edit.as_ref().unwrap().input == input));
    let expected = original.replacen("**标题**", "**新标题🌿**", 1);
    assert_eq!(file.read_with(&cx, |view, cx| view.draft(cx).to_string()), expected);
    cx.simulate_keystrokes("ctrl-s");
    cx.run_until_parked();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), expected);
    assert!(!file.read_with(&cx, |view, _| view.is_dirty()));
}

#[gpui::test]
fn undo_redo_crosses_formatted_source_and_finished_blocks(cx: &mut TestAppContext) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("history.md");
    let original = "# Title\n\n**Body**\n";
    std::fs::write(&path, original).unwrap();
    let (file, mut cx) = tests::open(path, cx);
    cx.update(|window, cx| {
        file.update(cx, |view, cx| {
            view.live_mode = true;
            view.begin_live_edit(1, window, cx);
        })
    });
    cx.simulate_input("!");
    cx.run_until_parked();
    let changed = file.read_with(&cx, |view, cx| view.draft(cx).to_string());
    assert_ne!(changed, original);
    cx.simulate_keystrokes("ctrl-enter");
    cx.run_until_parked();
    cx.simulate_keystrokes("ctrl-z");
    cx.run_until_parked();
    assert_eq!(file.read_with(&cx, |view, cx| view.draft(cx).to_string()), original);
    cx.update(|window, cx| file.update(cx, |view, cx| view.toggle_preview(window, cx)));
    cx.simulate_keystrokes("ctrl-shift-z");
    cx.run_until_parked();
    assert_eq!(file.read_with(&cx, |view, cx| view.draft(cx).to_string()), changed);
    cx.simulate_keystrokes("ctrl-z");
    cx.run_until_parked();
    assert_eq!(file.read_with(&cx, |view, cx| view.draft(cx).to_string()), original);
}

#[gpui::test]
fn composition_keeps_native_input_identity_and_marked_selection(cx: &mut TestAppContext) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("ime.md");
    std::fs::write(&path, "**你好**\n").unwrap();
    let (file, mut cx) = tests::open(path, cx);
    let input = cx.update(|window, cx| {
        file.update(cx, |view, cx| {
            view.live_mode = true;
            view.begin_live_edit(0, window, cx);
            view.live_edit.as_ref().unwrap().input.clone()
        })
    });
    for (text, length) in [("s", 1), ("sh", 2), ("世", 1)] {
        cx.update(|window, cx| {
            input.update(cx, |input, cx| {
                input.replace_and_mark_text_in_range(None, text, Some(length..length), window, cx);
            })
        });
        cx.run_until_parked();
        assert!(file.read_with(&cx, |view, _| view.live_edit.as_ref().unwrap().input == input));
    }
    cx.update(|window, cx| input.update(cx, |input, cx| input.unmark_text(window, cx)));
    assert_eq!(file.read_with(&cx, |view, cx| view.draft(cx).to_string()), "**你好世**\n");
    assert_eq!(input.read_with(&cx, |input, _| input.selected_range()), 9..9);
}

#[gpui::test]
fn composition_before_the_first_layout_keeps_its_caret(cx: &mut TestAppContext) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("first-frame.md");
    std::fs::write(&path, "你好").unwrap();
    let (file, mut cx) = tests::open(path, cx);
    let input = cx.update(|window, cx| {
        let input = file.update(cx, |view, cx| {
            let click = gpui::ClickEvent::Mouse(gpui::MouseClickEvent {
                down: Default::default(),
                up: Default::default(),
            });
            view.begin_live_edit_at(0, Some(&click), window, cx);
            view.live_edit.as_ref().unwrap().input.clone()
        });
        input.update(cx, |input, cx| {
            input.replace_and_mark_text_in_range(None, "世", Some(1..1), window, cx);
        });
        let _ = window.draw(cx);
        window.simulate_next_frame(cx);
        input
    });
    cx.run_until_parked();
    assert_eq!(input.read_with(&cx, |input, _| input.selected_range()), 9..9);
    assert_eq!(file.read_with(&cx, |view, cx| view.draft(cx)), "你好世");
}

#[gpui::test]
fn enter_creates_an_editable_paragraph_and_undo_restores_the_heading(cx: &mut TestAppContext) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("paragraph.md");
    std::fs::write(&path, "# Title").unwrap();
    let (file, mut cx) = tests::open(path, cx);
    cx.update(|window, cx| {
        file.update(cx, |view, cx| {
            view.live_mode = true;
            view.begin_live_edit(0, window, cx);
        })
    });
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(file.read_with(&cx, |view, cx| view.draft(cx).to_string()), "# Title\n\n");
    cx.simulate_input("New paragraph");
    cx.run_until_parked();
    assert_eq!(
        file.read_with(&cx, |view, cx| view.draft(cx).to_string()),
        "# Title\n\nNew paragraph"
    );
    cx.simulate_keystrokes("ctrl-z");
    cx.run_until_parked();
    cx.simulate_keystrokes("ctrl-z");
    cx.run_until_parked();
    assert_eq!(file.read_with(&cx, |view, cx| view.draft(cx).to_string()), "# Title");
}

#[gpui::test]
fn clicking_near_the_start_places_the_caret_near_the_start(cx: &mut TestAppContext) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("caret.md");
    std::fs::write(&path, "A paragraph with a long tail.").unwrap();
    let (file, mut cx) = tests::open(path, cx);
    cx.update(|window, cx| {
        file.update(cx, |view, cx| {
            view.live_mode = true;
            cx.notify();
        });
        let _ = window.draw(cx);
    });
    let bounds = cx.debug_bounds("markdown-preview-block-0").unwrap();
    let point = gpui::point(bounds.left() + px(2.0), bounds.top() + px(12.0));
    cx.simulate_mouse_down(point, MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_up(point, MouseButton::Left, Modifiers::default());
    cx.run_until_parked();
    cx.update(|window, cx| {
        let _ = window.draw(cx);
        window.simulate_next_frame(cx);
    });
    cx.run_until_parked();
    let caret =
        file.read_with(&cx, |view, cx| view.live_edit.as_ref().unwrap().input.read(cx).cursor());
    assert!(caret <= 1, "click at {point:?}, block {bounds:?}, actual caret {caret}");
}
