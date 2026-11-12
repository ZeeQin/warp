use std::sync::Arc;

use pathfinder_geometry::vector::vec2f;
use warp_util::local_or_remote_path::LocalOrRemotePath;
use warpui::elements::{
    ChildAnchor, ConstrainedBox, Container, Empty, Flex, OffsetPositioning, ParentAnchor,
    ParentElement, ParentOffsetBounds, Shrinkable, Stack,
};
use warpui::keymap::EditableBinding;
use warpui::platform::TerminationMode;
use warpui::windowing::WindowManager;
use warpui::{
    AppContext, Element, Entity, EntityId, SingletonEntity, TypedActionView, View, ViewContext,
    ViewHandle, WindowId,
};

use super::{FileNotebookAction, FileNotebookEvent, MarkdownDisplayMode, SourceFile};
use crate::appearance::Appearance;
use crate::code::editor_management::CodeSource;
use crate::notebooks::link::LinkEvent;
use crate::pane_group::pane::file_pane::FilePane;
use crate::pane_group::{self, PaneContent, PaneEvent};
use crate::terminal::model::session::Session;
use crate::util::openable_file_type::FileTarget;
use crate::util::traffic_lights::{TrafficLightMouseStates, traffic_light_data};
use crate::window_settings::WindowSettings;
use crate::workflows::WorkflowSelectionSource;
use crate::workspace::{TOTAL_TAB_BAR_HEIGHT, Workspace, WorkspaceRegistry};

#[derive(Default)]
struct PreviewWindow {
    window_id: Option<WindowId>,
}

impl Entity for PreviewWindow {
    type Event = ();
}

impl SingletonEntity for PreviewWindow {}

#[derive(Debug, Clone)]
enum PreviewAction {
    Close,
    ToggleDisplayMode,
}

pub(super) fn init(ctx: &mut AppContext) {
    use warpui::keymap::macros::*;

    ctx.add_singleton_model(|_| PreviewWindow::default());
    ctx.add_action("root_view:close_window", FilePreviewWindow::close_window);
    ctx.add_action(
        "root_view:minimize_window",
        FilePreviewWindow::minimize_window,
    );
    ctx.add_action(
        "root_view:toggle_maximize_window",
        FilePreviewWindow::toggle_maximize_window,
    );
    ctx.add_action(
        "root_view:toggle_fullscreen",
        FilePreviewWindow::toggle_fullscreen,
    );
    ctx.register_editable_bindings([
        EditableBinding::new(
            "file_preview:close",
            "Close file preview",
            PreviewAction::Close,
        )
        .with_context_predicate(id!("FilePreviewWindow"))
        .with_key_binding("cmdorctrl-w"),
        EditableBinding::new(
            super::TOGGLE_MARKDOWN_DISPLAY_MODE_BINDING_NAME,
            super::TOGGLE_MARKDOWN_DISPLAY_MODE_BINDING_DESCRIPTION,
            PreviewAction::ToggleDisplayMode,
        )
        .with_context_predicate(id!("FilePreviewWindow"))
        .with_key_binding(super::TOGGLE_MARKDOWN_DISPLAY_MODE_KEY_BINDING),
    ]);
}

pub(crate) fn open(
    path: LocalOrRemotePath,
    session: Option<Arc<Session>>,
    code_source: Option<CodeSource>,
    source_window_id: Option<WindowId>,
    ctx: &mut AppContext,
) {
    let existing = PreviewWindow::as_ref(ctx)
        .window_id
        .and_then(|id| ctx.root_view::<FilePreviewWindow>(id));
    if let Some(window) = existing {
        let window_id = window.window_id(ctx);
        window.update(ctx, |window, ctx| {
            // Links opened within the preview retain the original workspace.
            if source_window_id != Some(ctx.window_id()) {
                window.source_window_id = source_window_id;
            }
            window.open(path, session, code_source, ctx);
        });
        ctx.windows().show_window_and_focus_app(window_id);
        return;
    }

    let options = crate::root_view::default_window_options(WindowSettings::as_ref(ctx), ctx);
    let (window_id, _) = ctx.add_window(options, |ctx| {
        let pane = FilePane::new(None, None, None, ctx);
        let file_view = pane.file_view(ctx);
        file_view.update(ctx, |view, _| view.preview_content = Some(String::new()));
        ctx.subscribe_to_view(&file_view, FilePreviewWindow::handle_file_event);
        ctx.subscribe_to_model(
            &file_view.as_ref(ctx).links(),
            FilePreviewWindow::handle_link_event,
        );

        let mut window = FilePreviewWindow {
            pane,
            source_window_id,
            mouse_states: TrafficLightMouseStates::default(),
        };
        window.open(path, session, code_source, ctx);
        window
    });
    PreviewWindow::handle(ctx).update(ctx, |preview, _| preview.window_id = Some(window_id));
    ctx.windows().show_window_and_focus_app(window_id);
}

/// Returns whether the closing window is a transient preview, which must not enter undo-close.
pub(crate) fn handle_window_closed(window_id: WindowId, ctx: &mut AppContext) -> bool {
    let preview_id = PreviewWindow::as_ref(ctx).window_id;
    if preview_id == Some(window_id) {
        PreviewWindow::handle(ctx).update(ctx, |preview, _| preview.window_id = None);
        return true;
    }
    if let Some(preview_id) = preview_id
        && let Some(preview) = ctx.root_view::<FilePreviewWindow>(preview_id)
        && preview.as_ref(ctx).source_window_id == Some(window_id)
    {
        ctx.windows()
            .close_window(preview_id, TerminationMode::ForceTerminate);
    }
    false
}

pub(crate) fn is_preview_window(window_id: WindowId, ctx: &AppContext) -> bool {
    PreviewWindow::as_ref(ctx).window_id == Some(window_id)
}

struct FilePreviewWindow {
    pane: FilePane,
    source_window_id: Option<WindowId>,
    mouse_states: TrafficLightMouseStates,
}

impl FilePreviewWindow {
    fn close_window(&mut self, _: &(), ctx: &mut ViewContext<Self>) -> bool {
        ctx.close_window();
        true
    }

    fn minimize_window(&mut self, _: &(), ctx: &mut ViewContext<Self>) -> bool {
        ctx.minimize_window();
        true
    }

    fn toggle_maximize_window(&mut self, _: &(), ctx: &mut ViewContext<Self>) -> bool {
        ctx.toggle_maximized_window();
        true
    }

    fn toggle_fullscreen(&mut self, _: &(), ctx: &mut ViewContext<Self>) -> bool {
        let window_id = ctx.window_id();
        WindowManager::handle(ctx).update(ctx, |state, ctx| {
            state.toggle_fullscreen(window_id, ctx);
        });
        true
    }

    fn open(
        &mut self,
        path: LocalOrRemotePath,
        session: Option<Arc<Session>>,
        code_source: Option<CodeSource>,
        ctx: &mut ViewContext<Self>,
    ) {
        let previous = self.pane.file_view(ctx);
        if previous
            .as_ref(ctx)
            .path()
            .is_some_and(|old_path| old_path != &path)
            || previous.as_ref(ctx).context_window_id != self.source_window_id
        {
            ctx.unsubscribe_to_view(&previous);
            ctx.unsubscribe_to_model(&previous.as_ref(ctx).links());
            previous.update(ctx, |view, ctx| view.release_file_model(ctx));
            self.pane = FilePane::new(None, None, None, ctx);
            let file_view = self.pane.file_view(ctx);
            file_view.update(ctx, |view, _| view.preview_content = Some(String::new()));
            ctx.subscribe_to_view(&file_view, Self::handle_file_event);
            ctx.subscribe_to_model(&file_view.as_ref(ctx).links(), Self::handle_link_event);
        }
        self.pane.file_view(ctx).update(ctx, |view, ctx| {
            view.context_window_id = self.source_window_id;
            view.set_code_source(code_source);
            view.location = None;
            if view.path() != Some(&path) {
                view.markdown_display_mode = MarkdownDisplayMode::Rendered;
                view.display_mode_segmented_control
                    .update(ctx, |control, ctx| {
                        control.set_selected_mode(MarkdownDisplayMode::Rendered, ctx);
                    });
            }
            view.open(path, session, ctx);
            view.focus(ctx);
        });
        self.update_title(ctx);
        ctx.notify();
    }

    fn update_title(&self, ctx: &AppContext) {
        let title = self.pane.file_view(ctx).as_ref(ctx).title();
        ctx.windows()
            .set_window_title(self.pane.file_view(ctx).window_id(ctx), &title);
    }

    fn source_workspace(&mut self, ctx: &mut ViewContext<Self>) -> Option<ViewHandle<Workspace>> {
        if let Some(source) = self
            .source_window_id
            .and_then(|id| WorkspaceRegistry::as_ref(ctx).get(id, ctx))
        {
            return Some(source);
        }
        let (window_id, _) = crate::root_view::open_new_window_get_handles(None, ctx);
        self.source_window_id = Some(window_id);
        WorkspaceRegistry::as_ref(ctx).get(window_id, ctx)
    }

    fn forward(&mut self, event: pane_group::Event, ctx: &mut ViewContext<Self>) {
        let focus_source = matches!(
            &event,
            pane_group::Event::OpenFileWithTarget {
                target: FileTarget::CodeEditor(_),
                ..
            } | pane_group::Event::RunWorkflow { .. }
                | pane_group::Event::OpenWarpDriveLink { .. }
                | pane_group::Event::OpenDirectoryInNewTab { .. }
        );
        if let Some(source) = self.source_workspace(ctx) {
            source.update(ctx, |workspace, ctx| {
                workspace.handle_file_tree_event(
                    workspace.active_tab_pane_group().clone(),
                    &event,
                    ctx,
                );
            });
            if focus_source {
                ctx.windows()
                    .show_window_and_focus_app(source.window_id(ctx));
            }
        }
    }

    fn handle_file_event(
        &mut self,
        _: warpui::ViewHandle<super::FileNotebookView>,
        event: &FileNotebookEvent,
        ctx: &mut ViewContext<Self>,
    ) {
        match event {
            FileNotebookEvent::TitleUpdated | FileNotebookEvent::FileLoaded => {
                self.update_title(ctx)
            }
            FileNotebookEvent::RunWorkflow { workflow, source } => self.forward(
                pane_group::Event::RunWorkflow {
                    workflow: workflow.clone(),
                    workflow_source: *source,
                    workflow_selection_source: WorkflowSelectionSource::Notebook,
                    argument_override: None,
                },
                ctx,
            ),
            FileNotebookEvent::OpenFileWithTarget {
                path,
                target,
                line_col,
            } => self.forward(
                pane_group::Event::OpenFileWithTarget {
                    path: path.clone(),
                    target: target.clone(),
                    line_col: *line_col,
                },
                ctx,
            ),
            FileNotebookEvent::Pane(PaneEvent::Close | PaneEvent::CloseAndFocus { .. }) => {
                ctx.close_window()
            }
            FileNotebookEvent::Pane(PaneEvent::FocusActiveSession) => {
                if let Some(window_id) = self.source_window_id {
                    ctx.windows().show_window_and_focus_app(window_id);
                }
            }
            FileNotebookEvent::Pane(PaneEvent::ReplaceWithCodePane { path, source, .. }) => {
                if let Some(workspace) = self.source_workspace(ctx) {
                    workspace.update(ctx, |workspace, ctx| {
                        workspace.open_preview_in_editor(path.clone(), source.clone(), ctx);
                    });
                    ctx.windows()
                        .show_window_and_focus_app(workspace.window_id(ctx));
                }
            }
            FileNotebookEvent::Pane(
                PaneEvent::SplitLeft(_)
                | PaneEvent::SplitRight(_)
                | PaneEvent::SplitUp(_)
                | PaneEvent::SplitDown(_)
                | PaneEvent::ToggleMaximized
                | PaneEvent::FocusSelf
                | PaneEvent::AppStateChanged
                | PaneEvent::RepoChanged
                | PaneEvent::RemoteRepoNavigated { .. }
                | PaneEvent::NewPaneInAIMode { .. }
                | PaneEvent::ClearHoveredTabIndex
                | PaneEvent::ReplaceWithFilePane { .. },
            ) => {}
        }
    }

    fn handle_link_event(
        &mut self,
        _: warpui::ModelHandle<crate::notebooks::link::NotebookLinks>,
        event: &LinkEvent,
        ctx: &mut ViewContext<Self>,
    ) {
        match event {
            LinkEvent::OpenFileNotebook { path, session } => {
                self.open(path.clone().into(), Some(session.clone()), None, ctx);
            }
            LinkEvent::OpenFileWithTarget {
                path,
                target: FileTarget::MarkdownViewer(_),
                line_col,
            } => {
                let session = self
                    .pane
                    .file_view(ctx)
                    .as_ref(ctx)
                    .file_state
                    .source()
                    .and_then(|source| match source {
                        SourceFile::FileBased { session, .. } => session.clone(),
                        SourceFile::Static { .. } => None,
                    });
                self.open(
                    path.clone().into(),
                    session,
                    Some(CodeSource::Link {
                        path: path.clone(),
                        range_start: *line_col,
                        range_end: None,
                    }),
                    ctx,
                );
            }
            LinkEvent::OpenFileWithTarget {
                path,
                target,
                line_col,
            } => self.forward(
                pane_group::Event::OpenFileWithTarget {
                    path: path.clone(),
                    target: target.clone(),
                    line_col: *line_col,
                },
                ctx,
            ),
            LinkEvent::OpenWarpDriveLink {
                open_warp_drive_args,
            } => self.forward(
                pane_group::Event::OpenWarpDriveLink {
                    open_warp_drive_args: open_warp_drive_args.clone(),
                },
                ctx,
            ),
            LinkEvent::StartLocalSession { path } => self.forward(
                pane_group::Event::OpenDirectoryInNewTab { path: path.clone() },
                ctx,
            ),
            LinkEvent::RefreshLinks => {}
        }
    }
}

impl Entity for FilePreviewWindow {
    type Event = ();
}

impl View for FilePreviewWindow {
    fn ui_name() -> &'static str {
        "FilePreviewWindow"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        let window_id = self.pane.file_view(app).window_id(app);
        let theme = Appearance::as_ref(app).theme();
        let title_bar = ConstrainedBox::new(Empty::new().finish())
            .with_height(TOTAL_TAB_BAR_HEIGHT)
            .finish();
        let column = Flex::column().with_children([
            title_bar,
            Shrinkable::new(1., self.pane.id().render(app)).finish(),
        ]);
        let mut stack = Stack::new().with_child(column.finish());
        if let Some(data) = traffic_light_data(app, window_id) {
            let fullscreen = app
                .windows()
                .platform_window(window_id)
                .map(|window| window.fullscreen_state())
                .unwrap_or_default();
            stack.add_positioned_child(
                data.render(fullscreen, &self.mouse_states, theme, app),
                OffsetPositioning::offset_from_parent(
                    vec2f(0., 0.),
                    ParentOffsetBounds::WindowByPosition,
                    ParentAnchor::TopRight,
                    ChildAnchor::TopRight,
                ),
            );
        }
        Container::new(stack.finish())
            .with_background(theme.background())
            .finish()
    }

    fn child_view_ids(&self, _app: &AppContext) -> Vec<EntityId> {
        vec![self.pane.id().creation_order_id()]
    }
}

impl TypedActionView for FilePreviewWindow {
    type Action = PreviewAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        match action {
            PreviewAction::Close => ctx.close_window(),
            PreviewAction::ToggleDisplayMode => {
                self.pane.file_view(ctx).update(ctx, |view, ctx| {
                    if view.shows_markdown_toggle() {
                        let mode = match view.markdown_display_mode {
                            MarkdownDisplayMode::Rendered => MarkdownDisplayMode::Raw,
                            MarkdownDisplayMode::Raw => MarkdownDisplayMode::Rendered,
                        };
                        view.handle_action(
                            &FileNotebookAction::ToggleMarkdownDisplayMode(mode),
                            ctx,
                        );
                    }
                })
            }
        }
    }
}

#[cfg(test)]
#[path = "window_tests.rs"]
mod tests;
