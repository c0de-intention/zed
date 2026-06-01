use crate::git_panel::GitPanel;
use crate::git_panel_settings::GitPanelSettings;
use crate::pull_request::{Details, status_summary};
use gpui::{
    Action, AnyElement, App, AsyncWindowContext, Context, Entity, EventEmitter, FocusHandle,
    Focusable, IntoElement, KeyContext, ParentElement, Render, Styled, WeakEntity, Window, actions,
    div, rems,
};
use panel::PanelHeader;
use project::ProjectPath;
use settings::Settings;
use ui::{
    Button, ButtonStyle, Chip, Color, Icon, IconButton, IconName, IconSize, Label,
    LabelCommon as _, LabelSize, SpinnerLabel, Tab, Tooltip, prelude::*,
};
use workspace::{
    Workspace,
    dock::{DockPosition, Panel, PanelEvent},
};

actions!(
    pull_request_panel,
    [
        /// Opens the pull request panel for the current branch.
        Open,
        /// Refreshes pull request metadata for the current branch.
        Refresh,
        /// Toggles the selected pull request file's viewed state on GitHub.
        ToggleViewed,
    ]
);

pub fn register(workspace: &mut Workspace) {
    workspace.register_action(|workspace, _: &Open, window, cx| {
        workspace.toggle_panel_focus::<PullRequestPanel>(window, cx);
        if let Some(panel) = workspace.panel::<PullRequestPanel>(cx) {
            panel.update(cx, |panel, cx| {
                panel.refresh(window, cx);
            });
            crate::pull_request::open_or_focus_details_from_panel(workspace, true, window, cx);
        }
    });
}

pub struct PullRequestPanel {
    panel: Entity<GitPanel>,
}

impl PullRequestPanel {
    pub async fn load(
        workspace: WeakEntity<Workspace>,
        mut cx: AsyncWindowContext,
    ) -> anyhow::Result<Entity<Self>> {
        workspace.update_in(&mut cx, |workspace, window, cx| {
            let panel = GitPanel::new(workspace, window, cx);
            panel.update(cx, |panel, _cx| {
                panel.set_pull_request_render_mode();
            });
            let panel = cx.new(|_| Self { panel });
            let weak_panel = panel.downgrade();
            window.defer(cx, move |window, cx| {
                weak_panel
                    .update(cx, |panel, cx| panel.refresh(window, cx))
                    .ok();
            });
            panel
        })
    }

    pub(crate) fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.panel.update(cx, |panel, cx| {
            panel.open_pull_request_view(window, cx);
        });
    }

    pub(crate) fn details_content(
        &self,
        cx: &App,
    ) -> Option<crate::pull_request::PullRequestDetailsContent> {
        self.panel.read(cx).details_content(cx)
    }

    fn refresh_action(&mut self, _: &Refresh, window: &mut Window, cx: &mut Context<Self>) {
        self.refresh(window, cx);
    }

    pub(crate) fn toggle_viewed_for_project_path(
        &mut self,
        project_path: &ProjectPath,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.panel.update(cx, |panel, cx| {
            panel.toggle_viewed_for_project_path(project_path, window, cx)
        })
    }

    pub(crate) fn view_selected_in_github(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.panel.update(cx, |panel, cx| {
            panel.view_selected_in_github(window, cx);
        });
    }

    pub(crate) fn pull_request_context(
        &self,
        cx: &App,
    ) -> (
        Vec<editor::PullRequestFileLink>,
        Vec<editor::PullRequestCommentThread>,
    ) {
        self.panel.read(cx).pull_request_context(cx)
    }
}

impl EventEmitter<PanelEvent> for PullRequestPanel {}

impl Focusable for PullRequestPanel {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.panel.focus_handle(cx)
    }
}

impl Render for PullRequestPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut key_context = KeyContext::new_with_defaults();
        key_context.add("PullRequestPanel");

        div()
            .size_full()
            .key_context(key_context)
            .on_action(cx.listener(Self::refresh_action))
            .child(self.panel.clone())
    }
}

impl Panel for PullRequestPanel {
    fn persistent_name() -> &'static str {
        "PullRequestPanel"
    }

    fn panel_key() -> &'static str {
        "PullRequestPanel"
    }

    fn position(&self, window: &Window, cx: &App) -> DockPosition {
        self.panel.read(cx).position(window, cx)
    }

    fn position_is_valid(&self, position: DockPosition) -> bool {
        matches!(position, DockPosition::Left | DockPosition::Right)
    }

    fn set_position(
        &mut self,
        position: DockPosition,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.panel.update(cx, |panel, cx| {
            panel.set_position(position, window, cx);
        });
    }

    fn default_size(&self, window: &Window, cx: &App) -> Pixels {
        self.panel.read(cx).default_size(window, cx)
    }

    fn icon(&self, _: &Window, cx: &App) -> Option<ui::IconName> {
        Some(ui::IconName::PullRequest).filter(|_| GitPanelSettings::get_global(cx).button)
    }

    fn icon_tooltip(&self, _window: &Window, _cx: &App) -> Option<&'static str> {
        Some("Pull Request")
    }

    fn icon_label(&self, _: &Window, cx: &App) -> Option<String> {
        if !GitPanelSettings::get_global(cx).show_count_badge {
            return None;
        }
        let total = self.panel.read(cx).pull_request_change_count();
        (total > 0).then(|| total.to_string())
    }

    fn toggle_action(&self) -> Box<dyn Action> {
        Box::new(Open)
    }

    fn activation_priority(&self) -> u32 {
        4
    }
}

impl PanelHeader for PullRequestPanel {}

impl GitPanel {
    pub(crate) fn render_pull_request_contents(
        &self,
        has_write_access: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let has_entries = self.pull_request_file_count() > 0
            && !self.pull_request_is_loading()
            && !self.pull_request_not_found();
        let local_only_file_count = self.pull_request_local_only_file_count();
        let upstream_status = self.pull_request_upstream_status(cx);

        let panel = v_flex().size_full().child(self.render_pull_request_header(
            local_only_file_count,
            upstream_status,
            cx,
        ));
        if let Some(repo) = self.active_repository.clone()
            && has_entries
        {
            panel
                .when_some(
                    self.render_pull_request_description(cx),
                    |this, description| this.child(description),
                )
                .child(self.render_entries(has_write_access, repo, window, cx))
                .into_any_element()
        } else {
            panel
                .child(
                    v_flex()
                        .flex_1()
                        .items_center()
                        .justify_center()
                        .child(self.render_pull_request_empty_state(cx)),
                )
                .into_any_element()
        }
    }

    fn render_pull_request_header(
        &self,
        local_only_file_count: usize,
        upstream_status: Option<(u32, u32)>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let viewed_count = self.pull_request_viewed_count();
        let file_count = self.pull_request_file_count();
        let refresh_button = IconButton::new("refresh-pull-request", IconName::RotateCw)
            .icon_size(IconSize::Small)
            .tooltip(|_window, cx| Tooltip::for_action("Refresh Pull Request", &Refresh, cx))
            .on_click(cx.listener(|this, _, window, cx| this.open_pull_request_view(window, cx)));

        h_flex()
            .h(Tab::container_height(cx))
            .w_full()
            .px_2()
            .flex_none()
            .justify_between()
            .child(
                v_flex().min_w_0().flex_1().mr_2().child(
                    h_flex()
                        .min_w_0()
                        .gap_1()
                        .child(
                            Icon::new(IconName::PullRequest)
                                .size(IconSize::Small)
                                .color(Color::Muted),
                        )
                        .child(Label::new("Pull Request").size(LabelSize::Small)),
                ),
            )
            .child(
                h_flex()
                    .gap_1p5()
                    .items_center()
                    .when(local_only_file_count > 0, |this| {
                        this.child(self.render_warning_chip(
                            "Local diff",
                            format!(
                                "{local_only_file_count} local changed file(s) are not in GitHub's pull request file list yet."
                            ),
                            cx,
                        ))
                    })
                    .when_some(upstream_status, |this, (ahead, behind)| {
                        let label = match (ahead > 0, behind > 0) {
                            (true, true) => "Out of sync",
                            (true, false) => "Unpushed",
                            (false, true) => "Behind",
                            (false, false) => return this,
                        };
                        this.child(self.render_warning_chip(
                            label,
                            format!(
                                "Current branch is {ahead} commit(s) ahead and {behind} commit(s) behind its upstream."
                            ),
                            cx,
                        ))
                    })
                    .when(
                        !self.pull_request_is_loading() && self.pull_request_has_id(),
                        |this| {
                            this.child(
                                Chip::new(format!("{viewed_count}/{file_count} viewed"))
                                    .label_color(Color::Muted),
                            )
                        },
                    )
                    .child(refresh_button),
            )
    }

    fn render_pull_request_description(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let body = self.pull_request_body()?;
        let title = self
            .pull_request_title()
            .unwrap_or_else(|| "Pull Request".into());
        let details_button = IconButton::new("pull-request-details", IconName::FileDoc)
            .icon_size(IconSize::Small)
            .tooltip(|_window, cx| Tooltip::for_action("Pull Request Details", &Details, cx))
            .on_click(|_, window, cx| {
                window.dispatch_action(Box::new(Details), cx);
            });
        let review_count = self.pull_request_review_request_count();
        let review_status = if review_count == 0 {
            self.pull_request_review_decision()
                .as_ref()
                .map(|status| format!("Review: {status}"))
                .unwrap_or_else(|| "No reviewers".to_string())
        } else {
            format!("{review_count} reviewer(s)")
        };
        let checks_status = status_summary(self.pull_request_status_checks());
        let checks_color = pull_request_panel_checks_color(self.pull_request_status_checks());
        let checks_icon = pull_request_panel_checks_icon(self.pull_request_status_checks());

        Some(
            v_flex()
                .flex_basis(gpui::relative(0.25))
                .min_h(rems(6.))
                .max_h(rems(14.))
                .border_b_1()
                .border_color(cx.theme().colors().border)
                .p_2()
                .gap_1()
                .overflow_hidden()
                .child(
                    h_flex()
                        .justify_between()
                        .gap_2()
                        .child(Label::new(title).size(LabelSize::Small).truncate())
                        .child(details_button),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            h_flex()
                                .gap_1()
                                .child(Icon::new(IconName::UserCheck).color(Color::Muted))
                                .child(
                                    Label::new(review_status)
                                        .size(LabelSize::Small)
                                        .color(Color::Muted),
                                ),
                        )
                        .child(
                            h_flex()
                                .gap_1()
                                .child(Icon::new(checks_icon).color(checks_color))
                                .child(
                                    Label::new(checks_status)
                                        .size(LabelSize::Small)
                                        .color(Color::Muted),
                                ),
                        ),
                )
                .child(
                    Label::new(if body.trim().is_empty() {
                        "No description".into()
                    } else {
                        body.clone()
                    })
                    .size(LabelSize::Small)
                    .color(Color::Muted)
                    .truncate(),
                )
                .into_any_element(),
        )
    }

    fn render_warning_chip(
        &self,
        label: &'static str,
        tooltip: String,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        Chip::new(label)
            .icon(IconName::Warning)
            .icon_color(Color::Warning)
            .label_color(Color::Warning)
            .bg_color(cx.theme().status().warning_background.opacity(0.2))
            .border_color(cx.theme().status().warning_border)
            .tooltip(Tooltip::text(tooltip))
    }

    fn render_pull_request_empty_state(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.pull_request_is_loading() {
            return h_flex()
                .gap_1()
                .child(
                    SpinnerLabel::new()
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                )
                .child("Loading pull request")
                .into_any_element();
        }

        if self.pull_request_not_found() {
            let this = cx.weak_entity();
            return v_flex()
                .gap_1()
                .items_center()
                .child(
                    div()
                        .self_stretch()
                        .text_center()
                        .child("No pull request for current branch"),
                )
                .child(
                    div()
                        .self_stretch()
                        .text_center()
                        .child("Create a pull request in GitHub to review changed files."),
                )
                .child(
                    Button::new("create_pull_request", "Create Pull Request")
                        .label_size(LabelSize::Small)
                        .style(ButtonStyle::Filled)
                        .tooltip(|_window, cx| {
                            Tooltip::for_action(
                                "Create Pull Request",
                                &zed_actions::git::CreatePullRequest,
                                cx,
                            )
                        })
                        .on_click(move |_, window, cx| {
                            this.update(cx, |this, cx| {
                                this.create_pull_request(window, cx);
                            })
                            .ok();
                        }),
                )
                .into_any_element();
        }

        div()
            .self_stretch()
            .text_center()
            .child("No pull request changes")
            .into_any_element()
    }
}

fn pull_request_panel_checks_icon(
    status_checks: &[crate::github_pull_request::PullRequestStatusCheck],
) -> IconName {
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

fn pull_request_panel_checks_color(
    status_checks: &[crate::github_pull_request::PullRequestStatusCheck],
) -> Color {
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
