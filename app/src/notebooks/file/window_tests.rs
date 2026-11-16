use warp_files::FileModel;
use warpui::{App, SingletonEntity, View, WindowId};

use super::{FilePreviewWindow, PreviewWindow, handle_window_closed, open};
use crate::notebooks::editor::keys::NotebookKeybindings;
use crate::test_util::terminal::initialize_app_for_terminal_view;

fn initialize(app: &mut App) {
    initialize_app_for_terminal_view(app);
    app.add_singleton_model(FileModel::new);
    app.add_singleton_model(NotebookKeybindings::new);
    app.update(super::init);
}

#[test]
fn reuses_preview_window_and_releases_previous_file() {
    App::test((), |mut app| async move {
        initialize(&mut app);
        let directory = tempfile::tempdir().unwrap();
        let first = directory.path().join("first.md");
        let second = directory.path().join("second.md");
        std::fs::write(&first, "# First").unwrap();
        std::fs::write(&second, "# Second").unwrap();
        let first_source = WindowId::new();
        let second_source = WindowId::new();

        app.update(|ctx| open(first.into(), None, None, Some(first_source), ctx));
        let (preview_id, previous_file) = app.read(|ctx| {
            let id = PreviewWindow::as_ref(ctx).window_id.unwrap();
            let window = ctx.root_view::<FilePreviewWindow>(id).unwrap();
            (id, window.as_ref(ctx).pane.file_view(ctx))
        });
        app.update(|ctx| open(second.clone().into(), None, None, Some(second_source), ctx));

        app.read(|ctx| {
            assert_eq!(PreviewWindow::as_ref(ctx).window_id, Some(preview_id));
            assert_eq!(ctx.window_ids().count(), 1);
            let window = ctx.root_view::<FilePreviewWindow>(preview_id).unwrap();
            assert_eq!(window.as_ref(ctx).source_window_id, Some(second_source));
            assert_eq!(
                window
                    .as_ref(ctx)
                    .pane
                    .file_view(ctx)
                    .as_ref(ctx)
                    .local_path(),
                Some(second)
            );
            assert_eq!(previous_file.as_ref(ctx).file_id_for_test(), None);
        });
    });
}

#[test]
fn closing_preview_releases_file_and_reopening_creates_a_window() {
    App::test((), |mut app| async move {
        initialize(&mut app);
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("notes.md");
        std::fs::write(&path, "# Notes").unwrap();
        app.update(|ctx| open(path.clone().into(), None, None, None, ctx));
        let preview_id = app.read(|ctx| PreviewWindow::as_ref(ctx).window_id.unwrap());

        app.update(|ctx| {
            let window = ctx.root_view::<FilePreviewWindow>(preview_id).unwrap();
            let file = window.as_ref(ctx).pane.file_view(ctx);
            file.update(ctx, |view, ctx| view.on_window_closed(ctx));
            assert_eq!(file.as_ref(ctx).file_id_for_test(), None);
            assert!(handle_window_closed(preview_id, ctx));
            assert_eq!(PreviewWindow::as_ref(ctx).window_id, None);
            open(path.into(), None, None, None, ctx);
            assert_ne!(PreviewWindow::as_ref(ctx).window_id, Some(preview_id));
        });
    });
}

#[test]
fn markdown_link_switches_content_in_the_existing_window() {
    App::test((), |mut app| async move {
        initialize(&mut app);
        let directory = tempfile::tempdir().unwrap();
        let first = directory.path().join("first.md");
        let second = directory.path().join("second.md");
        std::fs::write(&first, "# First").unwrap();
        std::fs::write(&second, "# Second").unwrap();
        app.update(|ctx| open(first.into(), None, None, None, ctx));
        app.update(|ctx| {
            let id = PreviewWindow::as_ref(ctx).window_id.unwrap();
            let window = ctx.root_view::<FilePreviewWindow>(id).unwrap();
            window.update(ctx, |window, ctx| {
                let links = window.pane.file_view(ctx).as_ref(ctx).links();
                window.handle_link_event(
                    links,
                    &crate::notebooks::link::LinkEvent::OpenFileWithTarget {
                        path: second.clone(),
                        target: crate::util::openable_file_type::FileTarget::MarkdownViewer(
                            crate::util::openable_file_type::EditorLayout::SplitPane,
                        ),
                        line_col: None,
                    },
                    ctx,
                );
            });
            assert_eq!(PreviewWindow::as_ref(ctx).window_id, Some(id));
            assert_eq!(
                window
                    .as_ref(ctx)
                    .pane
                    .file_view(ctx)
                    .as_ref(ctx)
                    .local_path(),
                Some(second)
            );
        });
    });
}
