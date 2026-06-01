use std::{any::TypeId, sync::Arc};

use anyhow::anyhow;
use editor::{Editor, EditorEvent, MultiBuffer, PullRequestCommentThread};
use gpui::{
    AnyElement, App, Context, Entity, EventEmitter, FocusHandle, Focusable, FontWeight,
    InteractiveElement, IntoElement, KeyContext, ParentElement, Render, SharedString, Styled, Task,
    WeakEntity, Window, actions, div, prelude::*,
};
use language::Buffer;
use markdown::{Markdown, MarkdownElement, MarkdownFont, MarkdownStyle};
use project::Project;
use ui::{Color, Icon, IconName, Label, prelude::*};
use util::paths::PathStyle;
use workspace::{
    Item, ItemNavHistory, Workspace,
    item::{ItemEvent, SaveOptions, TabContentParams},
    searchable::SearchableItemHandle,
};

use crate::{
    github_pull_request::{
        PullRequestDetails, PullRequestStatusCheck, create_pull_request,
        fetch_pull_request_details, fetch_pull_request_template, update_current_pull_request_body,
    },
    pull_request_panel::{PullRequestPanel, Refresh},
};

actions!(
    pull_request,
    [
        /// Creates a pull request from the current branch.
        Create,
        /// Opens the current pull request description for editing.
        Edit,
        /// Saves the active pull request create/edit buffer.
        SaveChanges,
        /// Opens the current pull request details.
        Details,
    ]
);

pub fn register(workspace: &mut Workspace) {
    workspace.register_action(|workspace, _: &Create, window, cx| {
        open_create_buffer(workspace, window, cx);
    });
    workspace.register_action(|workspace, _: &Edit, window, cx| {
        open_edit_buffer(workspace, window, cx);
    });
    workspace.register_action(|workspace, _: &SaveChanges, window, cx| {
        let Some(editor) = workspace.active_item_as::<PullRequestBodyEditor>(cx) else {
            workspace.show_error(&anyhow!("No pull request description buffer is active"), cx);
            return;
        };
        editor.update(cx, |editor, cx| {
            editor.save_changes(window, cx);
        });
    });
    workspace.register_action(|workspace, _: &Details, window, cx| {
        open_or_focus_details_from_panel(workspace, true, window, cx);
    });
}

fn active_repository_work_directory(
    workspace: &Workspace,
    cx: &App,
) -> anyhow::Result<Arc<std::path::Path>> {
    let repository = workspace
        .project()
        .read(cx)
        .active_repository(cx)
        .ok_or_else(|| anyhow!("Pull request commands require a GitHub repository"))?;
    Ok(repository.read(cx).work_directory_abs_path.clone())
}

fn open_create_buffer(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    let work_directory = match active_repository_work_directory(workspace, cx) {
        Ok(work_directory) => work_directory,
        Err(error) => {
            workspace.show_error(&error, cx);
            return;
        }
    };
    let workspace = workspace.weak_handle();
    cx.spawn_in(window, async move |_, cx| {
        let body = cx
            .background_spawn(fetch_pull_request_template(work_directory.clone()))
            .await?;
        workspace.update_in(cx, |workspace, window, cx| {
            let editor = PullRequestBodyEditor::new(
                PullRequestBodyMode::Create,
                work_directory,
                body,
                workspace.weak_handle(),
                "Create Pull Request.md",
                window,
                cx,
            );
            workspace.add_item_to_active_pane(Box::new(editor), None, true, window, cx);
        })?;
        anyhow::Ok(())
    })
    .detach_and_log_err(cx);
}

fn open_edit_buffer(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    let work_directory = match active_repository_work_directory(workspace, cx) {
        Ok(work_directory) => work_directory,
        Err(error) => {
            workspace.show_error(&error, cx);
            return;
        }
    };
    let workspace = workspace.weak_handle();
    cx.spawn_in(window, async move |_, cx| {
        let details = cx
            .background_spawn(fetch_pull_request_details(work_directory.clone()))
            .await?;
        workspace.update_in(cx, |workspace, window, cx| {
            let editor = PullRequestBodyEditor::new(
                PullRequestBodyMode::Edit,
                work_directory,
                details.body,
                workspace.weak_handle(),
                format!("Edit PR #{}.md", details.number),
                window,
                cx,
            );
            workspace.add_item_to_active_pane(Box::new(editor), None, true, window, cx);
        })?;
        anyhow::Ok(())
    })
    .detach_and_log_err(cx);
}

pub(crate) fn open_or_focus_details_from_panel(
    workspace: &mut Workspace,
    focus: bool,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let languages = workspace.project().read(cx).languages().clone();
    let content = workspace
        .panel::<PullRequestPanel>(cx)
        .and_then(|panel| panel.read(cx).details_content(cx))
        .unwrap_or_else(PullRequestDetailsContent::loading);

    let details = workspace.item_of_type::<PullRequestDetailsView>(cx);
    if let Some(details) = details {
        details.update(cx, |details, cx| {
            details.replace_content(content, cx);
        });
        workspace.activate_item(&details, true, focus, window, cx);
    } else {
        let details = cx
            .new(|cx| PullRequestDetailsView::new(content, languages, workspace.weak_handle(), cx));
        workspace.add_item_to_active_pane(Box::new(details), None, focus, window, cx);
    }
}

#[derive(Clone, Copy)]
enum PullRequestBodyMode {
    Create,
    Edit,
}

pub struct PullRequestBodyEditor {
    mode: PullRequestBodyMode,
    work_directory: Arc<std::path::Path>,
    editor: Entity<Editor>,
    buffer: Entity<Buffer>,
    title: SharedString,
    workspace: WeakEntity<Workspace>,
}

impl PullRequestBodyEditor {
    fn new(
        mode: PullRequestBodyMode,
        work_directory: Arc<std::path::Path>,
        body: String,
        workspace: WeakEntity<Workspace>,
        title: impl Into<SharedString>,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) -> Entity<Self> {
        let title = title.into();
        let buffer = cx.new(|cx| Buffer::local(body, cx));
        let multibuffer =
            cx.new(|cx| MultiBuffer::singleton(buffer.clone(), cx).with_title(title.to_string()));
        let editor = cx.new(|cx| Editor::for_multibuffer(multibuffer, None, window, cx));

        cx.new(|_| Self {
            mode,
            work_directory,
            editor,
            buffer,
            title,
            workspace,
        })
    }

    fn body(&self, cx: &App) -> String {
        self.buffer.read(cx).text_snapshot().text()
    }

    fn save_changes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let body = self.body(cx);
        let work_directory = self.work_directory.clone();
        let workspace = self.workspace.clone();
        let mode = self.mode;
        cx.spawn_in(window, async move |_, cx| {
            let result = match mode {
                PullRequestBodyMode::Create => {
                    cx.background_spawn(create_pull_request(work_directory, body))
                        .await
                }
                PullRequestBodyMode::Edit => {
                    cx.background_spawn(update_current_pull_request_body(work_directory, body))
                        .await
                }
            };

            workspace.update_in(cx, |workspace, window, cx| match result {
                Ok(_) => {
                    if let Some(panel) = workspace.panel::<PullRequestPanel>(cx) {
                        panel.update(cx, |panel, cx| {
                            panel.refresh(window, cx);
                        });
                    }
                }
                Err(error) => {
                    workspace.show_error(&error, cx);
                }
            })?;
            anyhow::Ok(())
        })
        .detach_and_log_err(cx);
    }
}

impl EventEmitter<EditorEvent> for PullRequestBodyEditor {}

impl Focusable for PullRequestBodyEditor {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.editor.focus_handle(cx)
    }
}

impl Item for PullRequestBodyEditor {
    type Event = EditorEvent;

    fn tab_icon(&self, _window: &Window, _cx: &App) -> Option<Icon> {
        Some(Icon::new(IconName::PullRequest).color(Color::Muted))
    }

    fn tab_content(&self, params: TabContentParams, _window: &Window, _cx: &App) -> AnyElement {
        Label::new(self.title.clone())
            .color(if params.selected {
                Color::Default
            } else {
                Color::Muted
            })
            .into_any_element()
    }

    fn tab_content_text(&self, _detail: usize, _cx: &App) -> SharedString {
        self.title.clone()
    }

    fn to_item_events(event: &EditorEvent, f: &mut dyn FnMut(ItemEvent)) {
        Editor::to_item_events(event, f)
    }

    fn telemetry_event_text(&self) -> Option<&'static str> {
        Some("Pull Request Body Editor Opened")
    }

    fn deactivated(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.editor
            .update(cx, |editor, cx| editor.deactivated(window, cx));
    }

    fn act_as_type<'a>(
        &'a self,
        type_id: TypeId,
        self_handle: &'a Entity<Self>,
        _: &'a App,
    ) -> Option<gpui::AnyEntity> {
        if type_id == TypeId::of::<Self>() {
            Some(self_handle.clone().into())
        } else if type_id == TypeId::of::<Editor>() {
            Some(self.editor.clone().into())
        } else {
            None
        }
    }

    fn as_searchable(&self, _: &Entity<Self>, _: &App) -> Option<Box<dyn SearchableItemHandle>> {
        Some(Box::new(self.editor.clone()))
    }

    fn set_nav_history(
        &mut self,
        nav_history: ItemNavHistory,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.editor.update(cx, |editor, _| {
            editor.set_nav_history(Some(nav_history));
        });
    }

    fn navigate(
        &mut self,
        data: Arc<dyn std::any::Any + Send>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.editor
            .update(cx, |editor, cx| editor.navigate(data, window, cx))
    }

    fn added_to_workspace(
        &mut self,
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.editor.update(cx, |editor, cx| {
            editor.added_to_workspace(workspace, window, cx)
        });
    }

    fn can_save(&self, _cx: &App) -> bool {
        true
    }

    fn save(
        &mut self,
        _options: SaveOptions,
        _project: Entity<Project>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<anyhow::Result<()>> {
        self.save_changes(window, cx);
        Task::ready(Ok(()))
    }
}

impl Render for PullRequestBodyEditor {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut key_context = KeyContext::new_with_defaults();
        key_context.add("PullRequest");

        div()
            .size_full()
            .key_context(key_context)
            .on_action(cx.listener(|this, _: &SaveChanges, window, cx| {
                this.save_changes(window, cx);
            }))
            .child(self.editor.clone())
    }
}

pub struct PullRequestDetailsView {
    content: PullRequestDetailsContent,
    body_markdown: Entity<Markdown>,
    focus_handle: FocusHandle,
    workspace: WeakEntity<Workspace>,
}

impl PullRequestDetailsView {
    fn new(
        content: PullRequestDetailsContent,
        languages: Arc<language::LanguageRegistry>,
        workspace: WeakEntity<Workspace>,
        cx: &mut Context<Self>,
    ) -> Self {
        let body_markdown = cx.new(|cx| {
            Markdown::new(
                description_markdown(&content.details.body).into(),
                Some(languages),
                None,
                cx,
            )
        });
        Self {
            content,
            body_markdown,
            focus_handle: cx.focus_handle(),
            workspace,
        }
    }

    pub(crate) fn replace_content(
        &mut self,
        content: PullRequestDetailsContent,
        cx: &mut Context<Self>,
    ) {
        self.body_markdown.update(cx, |markdown, cx| {
            markdown.replace(description_markdown(&content.details.body), cx);
        });
        self.content = content;
        cx.notify();
    }

    fn refresh(&mut self, _: &Refresh, window: &mut Window, cx: &mut Context<Self>) {
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        workspace.update(cx, |workspace, cx| {
            let Some(panel) = workspace.panel::<PullRequestPanel>(cx) else {
                return;
            };
            panel.update(cx, |panel, cx| panel.refresh(window, cx));
        });
    }
}

impl EventEmitter<()> for PullRequestDetailsView {}

impl Item for PullRequestDetailsView {
    type Event = ();

    fn tab_icon(&self, _window: &Window, _cx: &App) -> Option<Icon> {
        Some(Icon::new(IconName::FileDoc).color(Color::Muted))
    }

    fn tab_content_text(&self, _detail: usize, _cx: &App) -> SharedString {
        "Pull Request Details".into()
    }

    fn telemetry_event_text(&self) -> Option<&'static str> {
        Some("Pull Request Details Opened")
    }
}

impl Focusable for PullRequestDetailsView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for PullRequestDetailsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut key_context = KeyContext::new_with_defaults();
        key_context.add("PullRequestDetails");
        let content = self.content.clone();

        v_flex()
            .size_full()
            .key_context(key_context)
            .on_action(cx.listener(Self::refresh))
            .overflow_hidden()
            .bg(cx.theme().colors().editor_background)
            .child(
                v_flex()
                    .w_full()
                    .max_w(rems(76.))
                    .p_4()
                    .gap_4()
                    .child(self.render_header(&content, cx))
                    .child(
                        h_flex()
                            .w_full()
                            .items_start()
                            .gap_6()
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .gap_4()
                                    .child(self.render_description(window, cx))
                                    .child(self.render_threads(&content, cx)),
                            )
                            .child(self.render_sidebar(&content, cx)),
                    ),
            )
    }
}

impl PullRequestDetailsView {
    fn render_header(
        &self,
        content: &PullRequestDetailsContent,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        v_flex()
            .gap_1()
            .pb_2()
            .border_b_1()
            .border_color(cx.theme().colors().border)
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .text_2xl()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(content.details.title.clone()),
                    )
                    .when(content.details.number > 0, |this| {
                        this.child(
                            div()
                                .text_2xl()
                                .text_color(cx.theme().colors().text_muted)
                                .child(format!("#{}", content.details.number)),
                        )
                    }),
            )
            .when(!content.details.url.is_empty(), |this| {
                this.child(
                    Label::new(content.details.url.clone())
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                )
            })
    }

    fn render_sidebar(
        &self,
        content: &PullRequestDetailsContent,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        v_flex()
            .w(rems(20.))
            .flex_none()
            .gap_4()
            .child(self.render_reviewers(content, cx))
            .child(self.render_checks(content, cx))
    }

    fn render_reviewers(
        &self,
        content: &PullRequestDetailsContent,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        v_flex()
            .gap_2()
            .pb_4()
            .border_b_1()
            .border_color(cx.theme().colors().border)
            .child(section_heading(
                IconName::UserCheck,
                "Reviewers",
                Color::Muted,
            ))
            .when(content.details.review_requests.is_empty(), |this| {
                this.child(
                    v_flex()
                        .gap_0p5()
                        .child(
                            Label::new("No reviewers requested")
                                .size(LabelSize::Small)
                                .color(Color::Muted),
                        )
                        .when_some(
                            content.details.review_decision.as_ref(),
                            |this, decision| {
                                this.child(
                                    Label::new(decision.clone())
                                        .size(LabelSize::XSmall)
                                        .color(Color::Muted),
                                )
                            },
                        ),
                )
            })
            .children(content.details.review_requests.iter().map(|reviewer| {
                status_row(
                    IconName::Person,
                    Color::Muted,
                    reviewer.login.clone(),
                    "Review requested",
                )
            }))
    }

    fn render_checks(
        &self,
        content: &PullRequestDetailsContent,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        v_flex()
            .gap_2()
            .pb_4()
            .border_b_1()
            .border_color(cx.theme().colors().border)
            .child(section_heading(
                check_summary_icon(content.details.status_checks.as_slice()),
                "Checks",
                check_summary_color(content.details.status_checks.as_slice()),
            ))
            .when(content.details.status_checks.is_empty(), |this| {
                this.child(status_row(
                    IconName::CircleHelp,
                    Color::Muted,
                    "No checks reported",
                    "GitHub has not reported CI status for this pull request",
                ))
            })
            .children(content.details.status_checks.iter().map(render_check_row))
    }

    fn render_description(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .border_1()
            .border_color(cx.theme().colors().border)
            .rounded_sm()
            .overflow_hidden()
            .child(
                h_flex()
                    .gap_2()
                    .p_3()
                    .bg(cx.theme().colors().editor_subheader_background)
                    .border_b_1()
                    .border_color(cx.theme().colors().border)
                    .child(Icon::new(IconName::Person).color(Color::Muted))
                    .child(
                        Label::new("Description")
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    ),
            )
            .child(
                div().p_3().child(
                    MarkdownElement::new(
                        self.body_markdown.clone(),
                        MarkdownStyle::themed(MarkdownFont::Preview, window, cx),
                    )
                    .key_context("PullRequestDetails"),
                ),
            )
    }

    fn render_threads(
        &self,
        content: &PullRequestDetailsContent,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        v_flex()
            .gap_2()
            .child(
                div()
                    .text_lg()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Threads"),
            )
            .when(content.comments.is_empty(), |this| {
                this.child(
                    h_flex()
                        .gap_2()
                        .p_3()
                        .border_1()
                        .border_color(cx.theme().colors().border)
                        .rounded_sm()
                        .child(Icon::new(IconName::Circle).color(Color::Muted))
                        .child(
                            Label::new("No review threads")
                                .size(LabelSize::Small)
                                .color(Color::Muted),
                        ),
                )
            })
            .children(content.comments.iter().map(|comment| {
                v_flex()
                    .gap_1()
                    .p_3()
                    .border_1()
                    .border_color(cx.theme().colors().border)
                    .rounded_sm()
                    .child(
                        h_flex()
                            .gap_2()
                            .child(Icon::new(IconName::PullRequest).color(Color::Muted))
                            .child(Label::new(comment.thread_label()).size(LabelSize::Small)),
                    )
                    .child(
                        Label::new(comment.body.clone())
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    )
            }))
    }
}

#[derive(Clone, Debug)]
pub(crate) struct PullRequestDetailsContent {
    pub(crate) details: PullRequestDetails,
    pub(crate) comments: Vec<PullRequestDetailsComment>,
}

impl PullRequestDetailsContent {
    pub(crate) fn loading() -> Self {
        Self {
            details: PullRequestDetails {
                number: 0,
                title: "Loading pull request details…".to_string(),
                url: String::new(),
                body: String::new(),
                review_decision: None,
                review_requests: Vec::new(),
                status_checks: Vec::new(),
            },
            comments: Vec::new(),
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct PullRequestDetailsComment {
    path: String,
    line: Option<u32>,
    original_line: Option<u32>,
    body: String,
    author: Option<String>,
}

impl PullRequestDetailsComment {
    pub(crate) fn from_thread(thread: &PullRequestCommentThread) -> Self {
        Self {
            path: thread.path.display(PathStyle::Posix).to_string(),
            line: thread.line,
            original_line: thread.original_line,
            body: thread.body.to_string(),
            author: thread.author.as_ref().map(ToString::to_string),
        }
    }

    fn thread_label(&self) -> String {
        let mut label = self.path.clone();
        if let Some(line) = self.line.or(self.original_line) {
            label.push_str(&format!(":{line}"));
        }
        if let Some(author) = &self.author {
            label.push_str(" by ");
            label.push_str(author);
        }
        label
    }
}

fn description_markdown(body: &str) -> String {
    if body.trim().is_empty() {
        "_No description._".to_string()
    } else {
        body.to_string()
    }
}

fn section_heading(icon: IconName, label: &'static str, color: Color) -> impl IntoElement {
    h_flex()
        .gap_2()
        .child(Icon::new(icon).color(color))
        .child(Label::new(label).size(LabelSize::Small))
}

fn status_row(
    icon: IconName,
    icon_color: Color,
    label: impl Into<SharedString>,
    detail: impl Into<SharedString>,
) -> impl IntoElement {
    h_flex()
        .gap_2()
        .child(Icon::new(icon).color(icon_color))
        .child(
            v_flex()
                .gap_0p5()
                .child(Label::new(label).size(LabelSize::Small))
                .child(
                    Label::new(detail)
                        .size(LabelSize::XSmall)
                        .color(Color::Muted),
                ),
        )
}

fn render_check_row(check: &PullRequestStatusCheck) -> impl IntoElement {
    let (icon, color, detail) = check_status(check);
    status_row(icon, color, check.name.clone(), detail)
}

fn check_status(check: &PullRequestStatusCheck) -> (IconName, Color, SharedString) {
    let state = check
        .conclusion
        .as_deref()
        .or(check.status.as_deref())
        .unwrap_or("Unknown");
    match state {
        "SUCCESS" | "SKIPPED" | "NEUTRAL" => (IconName::Check, Color::Success, state.into()),
        "FAILURE" | "ERROR" | "CANCELLED" | "TIMED_OUT" | "ACTION_REQUIRED" => {
            (IconName::XCircle, Color::Error, state.into())
        }
        "PENDING" | "QUEUED" | "IN_PROGRESS" | "REQUESTED" | "WAITING" => {
            (IconName::LoadCircle, Color::Warning, state.into())
        }
        _ => (IconName::CircleHelp, Color::Muted, state.into()),
    }
}

fn check_summary_icon(status_checks: &[PullRequestStatusCheck]) -> IconName {
    if status_checks.is_empty() {
        return IconName::CircleHelp;
    }
    let failed = status_checks.iter().any(|check| {
        check
            .conclusion
            .as_deref()
            .is_some_and(|conclusion| !matches!(conclusion, "SUCCESS" | "SKIPPED" | "NEUTRAL"))
    });
    if failed {
        IconName::XCircle
    } else {
        IconName::Check
    }
}

fn check_summary_color(status_checks: &[PullRequestStatusCheck]) -> Color {
    if status_checks.is_empty() {
        return Color::Muted;
    }
    let failed = status_checks.iter().any(|check| {
        check
            .conclusion
            .as_deref()
            .is_some_and(|conclusion| !matches!(conclusion, "SUCCESS" | "SKIPPED" | "NEUTRAL"))
    });
    if failed { Color::Error } else { Color::Success }
}

pub(crate) fn status_summary(status_checks: &[PullRequestStatusCheck]) -> String {
    if status_checks.is_empty() {
        return "Unknown".to_string();
    }

    let failed = status_checks
        .iter()
        .filter(|check| {
            check
                .conclusion
                .as_deref()
                .is_some_and(|conclusion| !matches!(conclusion, "SUCCESS" | "SKIPPED" | "NEUTRAL"))
        })
        .count();
    if failed > 0 {
        return format!("{failed} failing");
    }

    let pending = status_checks
        .iter()
        .filter(|check| {
            !check
                .conclusion
                .as_deref()
                .is_some_and(|conclusion| matches!(conclusion, "SUCCESS" | "SKIPPED" | "NEUTRAL"))
        })
        .count();
    if pending > 0 {
        format!("{pending} pending")
    } else {
        "Passing".to_string()
    }
}
