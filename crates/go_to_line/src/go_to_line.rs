pub mod cursor_position;

use cursor_position::LineIndicatorFormat;
use editor::{scroll::Autoscroll, Anchor, Editor, MultiBuffer, ToPoint};
use gpui::{
    div, prelude::*, AnyWindowHandle, AppContext, DismissEvent, EventEmitter, FocusHandle,
    FocusableView, Model, Render, SharedString, Styled, Subscription, View, ViewContext,
    VisualContext,
};
use language::Buffer;
use settings::Settings;
use text::Point;
use theme::ActiveTheme;
use ui::prelude::*;
use util::paths::FILE_ROW_COLUMN_DELIMITER;
use workspace::ModalView;

pub fn init(cx: &mut AppContext) {
    LineIndicatorFormat::register(cx);
    cx.observe_new_views(GoToLine::register).detach();
}

pub struct GoToLine {
    line_editor: View<Editor>,
    active_editor: View<Editor>,
    active_buffer: Model<Buffer>,
    current_text: SharedString,
    prev_scroll_position: Option<gpui::Point<f32>>,
    _subscriptions: Vec<Subscription>,
}

impl ModalView for GoToLine {}

impl FocusableView for GoToLine {
    fn focus_handle(&self, cx: &AppContext) -> FocusHandle {
        self.line_editor.focus_handle(cx)
    }
}
impl EventEmitter<DismissEvent> for GoToLine {}

enum GoToLineRowHighlights {}

impl GoToLine {
    fn register(editor: &mut Editor, cx: &mut ViewContext<Editor>) {
        let handle = cx.view().downgrade();
        editor
            .register_action(move |_: &editor::actions::ToggleGoToLine, cx| {
                let Some(editor_handle) = handle.upgrade() else {
                    return;
                };
                let Some(workspace) = editor_handle.read(cx).workspace() else {
                    return;
                };
                let editor = editor_handle.read(cx);
                let Some((_, buffer, _)) = editor.active_excerpt(cx) else {
                    return;
                };
                workspace.update(cx, |workspace, cx| {
                    workspace.toggle_modal(cx, move |cx| GoToLine::new(editor_handle, buffer, cx));
                })
            })
            .detach();
    }

    pub fn new(
        active_editor: View<Editor>,
        active_buffer: Model<Buffer>,
        cx: &mut ViewContext<Self>,
    ) -> Self {
        let (cursor, last_line, scroll_position) = active_editor.update(cx, |editor, cx| {
            let cursor = editor.selections.last::<Point>(cx).head();
            let snapshot = active_buffer.read(cx).snapshot();

            let last_line = editor
                .buffer()
                .read(cx)
                .excerpts_for_buffer(&active_buffer, cx)
                .into_iter()
                .map(move |(_, range)| text::ToPoint::to_point(&range.context.end, &snapshot).row)
                .max()
                .unwrap_or(0);

            (cursor, last_line, editor.scroll_position(cx))
        });

        let line = cursor.row + 1;
        let column = cursor.column + 1;

        let line_editor = cx.new_view(|cx| {
            let mut editor = Editor::single_line(cx);
            editor.set_placeholder_text(format!("{line}{FILE_ROW_COLUMN_DELIMITER}{column}"), cx);
            editor
        });
        let line_editor_change = cx.subscribe(&line_editor, Self::on_line_editor_event);

        let current_text = format!(
            "Current Line: {} of {} (column {})",
            line,
            last_line + 1,
            column
        );

        Self {
            line_editor,
            active_editor,
            active_buffer,
            current_text: current_text.into(),
            prev_scroll_position: Some(scroll_position),
            _subscriptions: vec![line_editor_change, cx.on_release(Self::release)],
        }
    }

    fn release(&mut self, window: AnyWindowHandle, cx: &mut AppContext) {
        window
            .update(cx, |_, cx| {
                let scroll_position = self.prev_scroll_position.take();
                self.active_editor.update(cx, |editor, cx| {
                    editor.clear_row_highlights::<GoToLineRowHighlights>();
                    if let Some(scroll_position) = scroll_position {
                        editor.set_scroll_position(scroll_position, cx);
                    }
                    cx.notify();
                })
            })
            .ok();
    }

    fn on_line_editor_event(
        &mut self,
        _: View<Editor>,
        event: &editor::EditorEvent,
        cx: &mut ViewContext<Self>,
    ) {
        match event {
            editor::EditorEvent::Blurred => cx.emit(DismissEvent),
            editor::EditorEvent::BufferEdited { .. } => self.highlight_current_line(cx),
            _ => {}
        }
    }

    fn highlight_current_line(&mut self, cx: &mut ViewContext<Self>) {
        self.active_editor.update(cx, |editor, cx| {
            editor.clear_row_highlights::<GoToLineRowHighlights>();
            let multibuffer = editor.buffer().read(cx);
            let snapshot = multibuffer.snapshot(cx);
            let Some(start) = self.anchor_from_query(&multibuffer, cx) else {
                return;
            };
            let start_point = start.to_point(&snapshot);
            let end_point = start_point + Point::new(1, 0);
            let end = snapshot.anchor_after(end_point);
            editor.highlight_rows::<GoToLineRowHighlights>(
                start..end,
                cx.theme().colors().editor_highlighted_line_background,
                true,
                cx,
            );
            editor.request_autoscroll(Autoscroll::center(), cx);
        });
        cx.notify();
    }

    fn anchor_from_query(
        &self,
        multibuffer: &MultiBuffer,
        cx: &ViewContext<Editor>,
    ) -> Option<Anchor> {
        let (Some(row), column) = self.line_column_from_query(cx) else {
            return None;
        };
        let point = Point::new(row.saturating_sub(1), column.unwrap_or(0).saturating_sub(1));
        multibuffer.buffer_point_to_anchor(&self.active_buffer, point, cx)
    }

    fn line_column_from_query(&self, cx: &AppContext) -> (Option<u32>, Option<u32>) {
        let input = self.line_editor.read(cx).text(cx);
        let mut components = input
            .splitn(2, FILE_ROW_COLUMN_DELIMITER)
            .map(str::trim)
            .fuse();
        let row = components.next().and_then(|row| row.parse::<u32>().ok());
        let column = components.next().and_then(|col| col.parse::<u32>().ok());
        (row, column)
    }

    fn cancel(&mut self, _: &menu::Cancel, cx: &mut ViewContext<Self>) {
        cx.emit(DismissEvent);
    }

    fn confirm(&mut self, _: &menu::Confirm, cx: &mut ViewContext<Self>) {
        self.active_editor.update(cx, |editor, cx| {
            let multibuffer = editor.buffer().read(cx);
            let Some(start) = self.anchor_from_query(&multibuffer, cx) else {
                return;
            };
            editor.change_selections(Some(Autoscroll::center()), cx, |s| {
                s.select_anchor_ranges([start..start])
            });
            editor.focus(cx);
            cx.notify()
        });
        self.prev_scroll_position.take();

        cx.emit(DismissEvent);
    }
}

impl Render for GoToLine {
    fn render(&mut self, cx: &mut ViewContext<Self>) -> impl IntoElement {
        let mut help_text = self.current_text.clone();
        let query = self.line_column_from_query(cx);
        if let Some(line) = query.0 {
            if let Some(column) = query.1 {
                help_text = format!("Go to line {line}, column {column}").into();
            } else {
                help_text = format!("Go to line {line}").into();
            }
        }

        v_flex()
            .w(rems(24.))
            .elevation_2(cx)
            .key_context("GoToLine")
            .on_action(cx.listener(Self::cancel))
            .on_action(cx.listener(Self::confirm))
            .child(
                div()
                    .border_b_1()
                    .border_color(cx.theme().colors().border_variant)
                    .px_2()
                    .py_1()
                    .child(self.line_editor.clone()),
            )
            .child(
                h_flex()
                    .px_2()
                    .py_1()
                    .gap_1()
                    .child(Label::new(help_text).color(Color::Muted)),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cursor_position::{CursorPosition, SelectionStats};
    use editor::actions::SelectAll;
    use gpui::{TestAppContext, VisualTestContext};
    use indoc::indoc;
    use project::{FakeFs, Project};
    use serde_json::json;
    use std::{sync::Arc, time::Duration};
    use workspace::{AppState, Workspace};

    #[gpui::test]
    async fn test_go_to_line_view_row_highlights(cx: &mut TestAppContext) {
        init_test(cx);
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            "/dir",
            json!({
                "a.rs": indoc!{"
                    struct SingleLine; // display line 0
                                       // display line 1
                    struct MultiLine { // display line 2
                        field_1: i32,  // display line 3
                        field_2: i32,  // display line 4
                    }                  // display line 5
                                       // display line 6
                    struct Another {   // display line 7
                        field_1: i32,  // display line 8
                        field_2: i32,  // display line 9
                        field_3: i32,  // display line 10
                        field_4: i32,  // display line 11
                    }                  // display line 12
                "}
            }),
        )
        .await;

        let project = Project::test(fs, ["/dir".as_ref()], cx).await;
        let (workspace, cx) = cx.add_window_view(|cx| Workspace::test_new(project.clone(), cx));
        let worktree_id = workspace.update(cx, |workspace, cx| {
            workspace.project().update(cx, |project, cx| {
                project.worktrees(cx).next().unwrap().read(cx).id()
            })
        });
        let _buffer = project
            .update(cx, |project, cx| project.open_local_buffer("/dir/a.rs", cx))
            .await
            .unwrap();
        let editor = workspace
            .update(cx, |workspace, cx| {
                workspace.open_path((worktree_id, "a.rs"), None, true, cx)
            })
            .await
            .unwrap()
            .downcast::<Editor>()
            .unwrap();

        let go_to_line_view = open_go_to_line_view(&workspace, cx);
        assert_eq!(
            highlighted_display_rows(&editor, cx),
            Vec::<u32>::new(),
            "Initially opened go to line modal should not highlight any rows"
        );
        assert_single_caret_at_row(&editor, 0, cx);

        cx.simulate_input("1");
        assert_eq!(
            highlighted_display_rows(&editor, cx),
            vec![0],
            "Go to line modal should highlight a row, corresponding to the query"
        );
        assert_single_caret_at_row(&editor, 0, cx);

        cx.simulate_input("8");
        assert_eq!(
            highlighted_display_rows(&editor, cx),
            vec![13],
            "If the query is too large, the last row should be highlighted"
        );
        assert_single_caret_at_row(&editor, 0, cx);

        cx.dispatch_action(menu::Cancel);
        drop(go_to_line_view);
        editor.update(cx, |_, _| {});
        assert_eq!(
            highlighted_display_rows(&editor, cx),
            Vec::<u32>::new(),
            "After cancelling and closing the modal, no rows should be highlighted"
        );
        assert_single_caret_at_row(&editor, 0, cx);

        let go_to_line_view = open_go_to_line_view(&workspace, cx);
        assert_eq!(
            highlighted_display_rows(&editor, cx),
            Vec::<u32>::new(),
            "Reopened modal should not highlight any rows"
        );
        assert_single_caret_at_row(&editor, 0, cx);

        let expected_highlighted_row = 4;
        cx.simulate_input("5");
        assert_eq!(
            highlighted_display_rows(&editor, cx),
            vec![expected_highlighted_row]
        );
        assert_single_caret_at_row(&editor, 0, cx);
        cx.dispatch_action(menu::Confirm);
        drop(go_to_line_view);
        editor.update(cx, |_, _| {});
        assert_eq!(
            highlighted_display_rows(&editor, cx),
            Vec::<u32>::new(),
            "After confirming and closing the modal, no rows should be highlighted"
        );
        // On confirm, should place the caret on the highlighted row.
        assert_single_caret_at_row(&editor, expected_highlighted_row, cx);
    }

    #[gpui::test]
    async fn test_unicode_characters_selection(cx: &mut TestAppContext) {
        init_test(cx);

        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            "/dir",
            json!({
                "a.rs": "ēlo"
            }),
        )
        .await;

        let project = Project::test(fs, ["/dir".as_ref()], cx).await;
        let (workspace, cx) = cx.add_window_view(|cx| Workspace::test_new(project.clone(), cx));
        workspace.update(cx, |workspace, cx| {
            let cursor_position = cx.new_view(|_| CursorPosition::new(workspace));
            workspace.status_bar().update(cx, |status_bar, cx| {
                status_bar.add_right_item(cursor_position, cx);
            });
        });

        let worktree_id = workspace.update(cx, |workspace, cx| {
            workspace.project().update(cx, |project, cx| {
                project.worktrees(cx).next().unwrap().read(cx).id()
            })
        });
        let _buffer = project
            .update(cx, |project, cx| project.open_local_buffer("/dir/a.rs", cx))
            .await
            .unwrap();
        let editor = workspace
            .update(cx, |workspace, cx| {
                workspace.open_path((worktree_id, "a.rs"), None, true, cx)
            })
            .await
            .unwrap()
            .downcast::<Editor>()
            .unwrap();

        cx.executor().advance_clock(Duration::from_millis(200));
        workspace.update(cx, |workspace, cx| {
            assert_eq!(
                &SelectionStats {
                    lines: 0,
                    characters: 0,
                    selections: 1,
                },
                workspace
                    .status_bar()
                    .read(cx)
                    .item_of_type::<CursorPosition>()
                    .expect("missing cursor position item")
                    .read(cx)
                    .selection_stats(),
                "No selections should be initially"
            );
        });
        editor.update(cx, |editor, cx| editor.select_all(&SelectAll, cx));
        cx.executor().advance_clock(Duration::from_millis(200));
        workspace.update(cx, |workspace, cx| {
            assert_eq!(
                &SelectionStats {
                    lines: 1,
                    characters: 3,
                    selections: 1,
                },
                workspace
                    .status_bar()
                    .read(cx)
                    .item_of_type::<CursorPosition>()
                    .expect("missing cursor position item")
                    .read(cx)
                    .selection_stats(),
                "After selecting a text with multibyte unicode characters, the character count should be correct"
            );
        });
    }

    fn open_go_to_line_view(
        workspace: &View<Workspace>,
        cx: &mut VisualTestContext,
    ) -> View<GoToLine> {
        cx.dispatch_action(editor::actions::ToggleGoToLine);
        workspace.update(cx, |workspace, cx| {
            workspace.active_modal::<GoToLine>(cx).unwrap().clone()
        })
    }

    fn highlighted_display_rows(editor: &View<Editor>, cx: &mut VisualTestContext) -> Vec<u32> {
        editor.update(cx, |editor, cx| {
            editor
                .highlighted_display_rows(cx)
                .into_keys()
                .map(|r| r.0)
                .collect()
        })
    }

    #[track_caller]
    fn assert_single_caret_at_row(
        editor: &View<Editor>,
        buffer_row: u32,
        cx: &mut VisualTestContext,
    ) {
        let selections = editor.update(cx, |editor, cx| {
            editor
                .selections
                .all::<rope::Point>(cx)
                .into_iter()
                .map(|s| s.start..s.end)
                .collect::<Vec<_>>()
        });
        assert!(
            selections.len() == 1,
            "Expected one caret selection but got: {selections:?}"
        );
        let selection = &selections[0];
        assert!(
            selection.start == selection.end,
            "Expected a single caret selection, but got: {selection:?}"
        );
        assert_eq!(selection.start.row, buffer_row);
    }

    fn init_test(cx: &mut TestAppContext) -> Arc<AppState> {
        cx.update(|cx| {
            let state = AppState::test(cx);
            language::init(cx);
            crate::init(cx);
            editor::init(cx);
            workspace::init_settings(cx);
            Project::init_settings(cx);
            state
        })
    }
}
