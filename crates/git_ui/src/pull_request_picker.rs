use std::path::PathBuf;
use std::sync::Arc;

use anyhow::anyhow;
use fuzzy::StringMatchCandidate;
use git::repository::Worktree as GitWorktree;
use gpui::{
    AnyElement, App, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable,
    IntoElement, KeyContext, ParentElement, Render, SharedString, Styled, Subscription, Task,
    WeakEntity, Window, actions, rems,
};
use picker::{Picker, PickerDelegate, PickerEditorPosition};
use ui::{
    Color, HighlightedLabel, Icon, IconName, IconSize, Label, LabelSize, ListItem, ListItemSpacing,
    ToggleButtonGroup, ToggleButtonGroupStyle, ToggleButtonSimple, prelude::*,
};
use util::ResultExt as _;
use workspace::{ModalView, Workspace, dock::DockPosition};
use zed_actions::OpenWorktreeInNewWindow;

use crate::github_pull_request::{
    OpenPullRequest, fetch_current_pull_request_number, fetch_open_pull_requests,
    open_current_pull_request_in_browser,
};

actions!(
    pull_request_picker,
    [
        /// Opens a picker for GitHub pull requests in the current repository.
        OpenPullRequestPicker,
        /// Opens a picker for GitHub pull requests awaiting your review.
        OpenPullRequestsAwaitingReviewPicker,
        /// Opens a picker for GitHub pull requests created by you.
        OpenMyPullRequestsPicker,
        /// Opens a picker for GitHub pull requests involving you.
        OpenPullRequestsInvolvingMePicker,
        /// Opens a picker for GitHub pull requests assigned to you.
        OpenPullRequestsAssignedToMePicker,
        /// Opens the current GitHub pull request in the browser.
        OpenCurrentPullRequestInBrowser,
        /// Shows all open pull requests in the pull request picker.
        ShowAllPullRequests,
        /// Shows pull requests awaiting your review in the pull request picker.
        ShowPullRequestsAwaitingReview,
        /// Shows pull requests created by you in the pull request picker.
        ShowMyPullRequests,
        /// Shows pull requests involving you in the pull request picker.
        ShowPullRequestsInvolvingMe,
        /// Shows pull requests assigned to you in the pull request picker.
        ShowPullRequestsAssignedToMe,
        /// Selects the next pull request picker filter.
        NextPullRequestFilter,
        /// Selects the previous pull request picker filter.
        PreviousPullRequestFilter,
    ]
);

pub fn register(workspace: &mut Workspace) {
    workspace.register_action(|workspace, _: &OpenPullRequestPicker, window, cx| {
        open_pull_request_picker(workspace, PullRequestPickerFilter::All, window, cx);
    });
    workspace.register_action(|workspace, _: &ShowAllPullRequests, window, cx| {
        open_pull_request_picker(workspace, PullRequestPickerFilter::All, window, cx);
    });
    workspace.register_action(
        |workspace, _: &OpenPullRequestsAwaitingReviewPicker, window, cx| {
            open_pull_request_picker(
                workspace,
                PullRequestPickerFilter::AwaitingReview,
                window,
                cx,
            );
        },
    );
    workspace.register_action(
        |workspace, _: &ShowPullRequestsAwaitingReview, window, cx| {
            open_pull_request_picker(
                workspace,
                PullRequestPickerFilter::AwaitingReview,
                window,
                cx,
            );
        },
    );
    workspace.register_action(|workspace, _: &OpenMyPullRequestsPicker, window, cx| {
        open_pull_request_picker(workspace, PullRequestPickerFilter::CreatedByMe, window, cx);
    });
    workspace.register_action(|workspace, _: &ShowMyPullRequests, window, cx| {
        open_pull_request_picker(workspace, PullRequestPickerFilter::CreatedByMe, window, cx);
    });
    workspace.register_action(
        |workspace, _: &OpenPullRequestsInvolvingMePicker, window, cx| {
            open_pull_request_picker(workspace, PullRequestPickerFilter::InvolvesMe, window, cx);
        },
    );
    workspace.register_action(|workspace, _: &ShowPullRequestsInvolvingMe, window, cx| {
        open_pull_request_picker(workspace, PullRequestPickerFilter::InvolvesMe, window, cx);
    });
    workspace.register_action(
        |workspace, _: &OpenPullRequestsAssignedToMePicker, window, cx| {
            open_pull_request_picker(workspace, PullRequestPickerFilter::AssignedToMe, window, cx);
        },
    );
    workspace.register_action(|workspace, _: &ShowPullRequestsAssignedToMe, window, cx| {
        open_pull_request_picker(workspace, PullRequestPickerFilter::AssignedToMe, window, cx);
    });

    workspace.register_action(
        |workspace, _: &OpenCurrentPullRequestInBrowser, window, cx| {
            let Some(repository) = workspace.project().read(cx).active_repository(cx) else {
                workspace.show_error(
                    &anyhow!("Open current pull request requires a GitHub repository"),
                    cx,
                );
                return;
            };

            let work_directory = repository.read(cx).work_directory_abs_path.clone();
            let workspace = workspace.weak_handle();
            cx.spawn_in(window, async move |_, cx| {
                let result = cx
                    .background_spawn(open_current_pull_request_in_browser(work_directory))
                    .await;
                if let Err(error) = result {
                    workspace
                        .update(cx, |workspace, cx| {
                            workspace.show_error(&error, cx);
                        })
                        .ok();
                }
            })
            .detach();
        },
    );
}

fn open_pull_request_picker(
    workspace: &mut Workspace,
    filter: PullRequestPickerFilter,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let Some(repository) = workspace.project().read(cx).active_repository(cx) else {
        workspace.show_error(
            &anyhow!("Open pull request picker requires a GitHub repository"),
            cx,
        );
        return;
    };

    let focused_dock = workspace.focused_dock_position(window, cx);
    let workspace_handle = workspace.weak_handle();
    workspace.toggle_modal(window, cx, |window, cx| {
        PullRequestPicker::new(
            repository,
            workspace_handle,
            focused_dock,
            filter,
            window,
            cx,
        )
    });
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PullRequestPickerFilter {
    All,
    AwaitingReview,
    CreatedByMe,
    InvolvesMe,
    AssignedToMe,
}

impl PullRequestPickerFilter {
    const ALL: [Self; 5] = [
        Self::All,
        Self::AwaitingReview,
        Self::CreatedByMe,
        Self::InvolvesMe,
        Self::AssignedToMe,
    ];

    fn search_query(self) -> Option<&'static str> {
        match self {
            Self::All => None,
            Self::AwaitingReview => Some("review-requested:@me"),
            Self::CreatedByMe => Some("author:@me"),
            Self::InvolvesMe => Some("involves:@me"),
            Self::AssignedToMe => Some("assignee:@me"),
        }
    }

    fn placeholder(self) -> &'static str {
        match self {
            Self::All => "Open pull request…",
            Self::AwaitingReview => "Open pull request awaiting review…",
            Self::CreatedByMe => "Open my pull request…",
            Self::InvolvesMe => "Open pull request involving me…",
            Self::AssignedToMe => "Open pull request assigned to me…",
        }
    }

    fn no_matches_text(self) -> &'static str {
        match self {
            Self::All => "No open pull requests",
            Self::AwaitingReview => "No pull requests awaiting your review",
            Self::CreatedByMe => "No open pull requests created by you",
            Self::InvolvesMe => "No open pull requests involving you",
            Self::AssignedToMe => "No open pull requests assigned to you",
        }
    }

    fn tab_label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::AwaitingReview => "Review requested",
            Self::CreatedByMe => "Created by me",
            Self::InvolvesMe => "Involves me",
            Self::AssignedToMe => "Assigned to me",
        }
    }

    fn index(self) -> usize {
        Self::ALL
            .iter()
            .position(|filter| *filter == self)
            .unwrap_or_default()
    }

    fn adjacent(self, offset: isize) -> Self {
        let filter_count = Self::ALL.len() as isize;
        let next_index = (self.index() as isize + offset).rem_euclid(filter_count);
        Self::ALL[next_index as usize]
    }
}

pub struct PullRequestPicker {
    repository: Entity<project::git_store::Repository>,
    picker: Entity<Picker<PullRequestPickerDelegate>>,
    focus_handle: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl PullRequestPicker {
    fn new(
        repository: Entity<project::git_store::Repository>,
        workspace: WeakEntity<Workspace>,
        focused_dock: Option<DockPosition>,
        filter: PullRequestPickerFilter,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let remote_name = repository
            .read(cx)
            .branch
            .as_ref()
            .and_then(|branch| {
                branch
                    .upstream
                    .as_ref()
                    .and_then(|upstream| upstream.remote_name())
                    .or_else(|| branch.remote_name())
            })
            .unwrap_or("origin")
            .to_string();

        let delegate = PullRequestPickerDelegate {
            pull_requests: Vec::new(),
            matches: Vec::new(),
            selected_index: 0,
            loading: true,
            load_generation: 0,
            error: None,
            counts_by_filter: [None; PullRequestPickerFilter::ALL.len()],
            current_pull_request_number: None,
            filter,
            workspace,
            focused_dock,
            all_worktrees: Vec::new(),
            remote_name,
        };

        let picker = cx.new(|cx| {
            Picker::uniform_list(delegate, window, cx)
                .show_scrollbar(true)
                .modal(true)
        });
        let focus_handle = picker.focus_handle(cx);

        let mut this = Self {
            repository,
            picker,
            focus_handle,
            _subscriptions: Vec::new(),
        };
        this.load_filter(filter, window, cx);
        this._subscriptions = vec![cx.subscribe(&this.picker, |_, _, _, cx| {
            cx.emit(DismissEvent);
        })];
        this
    }

    fn load_filter(
        &mut self,
        filter: PullRequestPickerFilter,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let work_directory = self.repository.read(cx).work_directory_abs_path.clone();
        let worktrees_request = self
            .repository
            .update(cx, |repository, _| repository.worktrees());
        let picker = self.picker.clone();
        let load_generation = picker.update(cx, |picker, cx| {
            picker.delegate.load_generation += 1;
            picker.delegate.filter = filter;
            picker.delegate.loading = true;
            picker.delegate.error = None;
            picker.delegate.pull_requests.clear();
            picker.delegate.matches.clear();
            picker.delegate.selected_index = 0;
            picker.refresh(window, cx);
            picker.delegate.load_generation
        });

        let picker = self.picker.downgrade();
        cx.spawn_in(window, async move |_this, cx| {
            let pull_requests_task = cx.background_spawn(fetch_open_pull_requests(
                work_directory.clone(),
                filter.search_query().map(ToOwned::to_owned),
            ));
            let count_tasks = PullRequestPickerFilter::ALL
                .into_iter()
                .filter(|count_filter| *count_filter != filter)
                .map(|count_filter| {
                    let work_directory = work_directory.clone();
                    let task = cx.background_spawn(fetch_open_pull_requests(
                        work_directory,
                        count_filter.search_query().map(ToOwned::to_owned),
                    ));
                    async move {
                        (
                            count_filter,
                            task.await.map(|pull_requests| pull_requests.len()),
                        )
                    }
                })
                .collect::<Vec<_>>();
            let current_pull_request_task =
                cx.background_spawn(fetch_current_pull_request_number(work_directory));
            let (pull_requests, (current_pull_request, (all_worktrees, counts_by_filter))) =
                futures::future::join(
                    pull_requests_task,
                    futures::future::join(
                        current_pull_request_task,
                        futures::future::join(
                            async move {
                                worktrees_request.await.map(|result| {
                                    result.map(|worktrees| {
                                        worktrees
                                            .into_iter()
                                            .filter(|worktree| !worktree.is_bare)
                                            .collect::<Vec<_>>()
                                    })
                                })
                            },
                            futures::future::join_all(count_tasks),
                        ),
                    ),
                )
                .await;

            picker.update_in(cx, |picker, window, cx| {
                if picker.delegate.filter != filter
                    || picker.delegate.load_generation != load_generation
                {
                    return;
                }

                picker.delegate.loading = false;
                match pull_requests {
                    Ok(pull_requests) => {
                        picker.delegate.counts_by_filter[filter.index()] =
                            Some(pull_requests.len());
                        picker.delegate.pull_requests = pull_requests;
                    }
                    Err(error) => {
                        picker.delegate.error = Some(format!("{error:#}").into());
                    }
                }

                for (count_filter, count_result) in counts_by_filter {
                    match count_result {
                        Ok(count) => {
                            picker.delegate.counts_by_filter[count_filter.index()] = Some(count);
                        }
                        Err(error) => {
                            log::debug!(
                                "failed to load pull request count for {:?}: {error:#}",
                                count_filter
                            );
                        }
                    }
                }

                match current_pull_request {
                    Ok(number) => {
                        picker.delegate.current_pull_request_number = number;
                    }
                    Err(error) => {
                        picker.delegate.error = Some(format!("{error:#}").into());
                    }
                }

                match all_worktrees {
                    Ok(Ok(worktrees)) => {
                        picker.delegate.all_worktrees = worktrees;
                    }
                    Ok(Err(error)) => {
                        picker.delegate.error = Some(format!("{error:#}").into());
                    }
                    Err(_) => {
                        picker.delegate.error = Some("Loading git worktrees was cancelled".into());
                    }
                }

                picker.refresh(window, cx);
            })?;

            anyhow::Ok(())
        })
        .detach_and_log_err(cx);
    }

    fn set_filter(
        &mut self,
        filter: PullRequestPickerFilter,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let current_filter = self.picker.read(cx).delegate.filter;
        if current_filter != filter {
            self.load_filter(filter, window, cx);
        }
    }

    fn show_all(&mut self, _: &ShowAllPullRequests, window: &mut Window, cx: &mut Context<Self>) {
        self.set_filter(PullRequestPickerFilter::All, window, cx);
    }

    fn show_awaiting_review(
        &mut self,
        _: &ShowPullRequestsAwaitingReview,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_filter(PullRequestPickerFilter::AwaitingReview, window, cx);
    }

    fn show_created_by_me(
        &mut self,
        _: &ShowMyPullRequests,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_filter(PullRequestPickerFilter::CreatedByMe, window, cx);
    }

    fn show_involving_me(
        &mut self,
        _: &ShowPullRequestsInvolvingMe,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_filter(PullRequestPickerFilter::InvolvesMe, window, cx);
    }

    fn show_assigned_to_me(
        &mut self,
        _: &ShowPullRequestsAssignedToMe,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_filter(PullRequestPickerFilter::AssignedToMe, window, cx);
    }

    fn next_filter(
        &mut self,
        _: &NextPullRequestFilter,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let filter = self.picker.read(cx).delegate.filter.adjacent(1);
        self.set_filter(filter, window, cx);
    }

    fn previous_filter(
        &mut self,
        _: &PreviousPullRequestFilter,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let filter = self.picker.read(cx).delegate.filter.adjacent(-1);
        self.set_filter(filter, window, cx);
    }

    fn render_filter_tabs(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let delegate = &self.picker.read(cx).delegate;
        let current_filter = delegate.filter;
        let counts_by_filter = delegate.counts_by_filter;

        h_flex().p_2().pb_0p5().w_full().child(
            ToggleButtonGroup::single_row(
                "pull-request-picker-filters",
                PullRequestPickerFilter::ALL.map(|filter| {
                    let label = if let Some(count) = counts_by_filter[filter.index()] {
                        format!("{} ({count})", filter.tab_label())
                    } else {
                        filter.tab_label().to_string()
                    };
                    ToggleButtonSimple::new(
                        label,
                        cx.listener(move |this, _, window, cx| {
                            this.set_filter(filter, window, cx);
                        }),
                    )
                }),
            )
            .label_size(LabelSize::Small)
            .style(ToggleButtonGroupStyle::Outlined)
            .selected_index(current_filter.index()),
        )
    }
}

impl Focusable for PullRequestPicker {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<DismissEvent> for PullRequestPicker {}
impl ModalView for PullRequestPicker {}

impl Render for PullRequestPicker {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut key_context = KeyContext::new_with_defaults();
        key_context.add("PullRequestPicker");

        div()
            .w(rems(40.))
            .key_context(key_context)
            .on_action(cx.listener(Self::show_all))
            .on_action(cx.listener(Self::show_awaiting_review))
            .on_action(cx.listener(Self::show_created_by_me))
            .on_action(cx.listener(Self::show_involving_me))
            .on_action(cx.listener(Self::show_assigned_to_me))
            .on_action(cx.listener(Self::next_filter))
            .on_action(cx.listener(Self::previous_filter))
            .child(
                v_flex()
                    .w_full()
                    .child(self.render_filter_tabs(cx))
                    .child(self.picker.clone()),
            )
    }
}

#[derive(Clone)]
struct PullRequestMatch {
    pull_request: OpenPullRequest,
    positions: Vec<usize>,
}

struct PullRequestPickerDelegate {
    pull_requests: Vec<OpenPullRequest>,
    matches: Vec<PullRequestMatch>,
    selected_index: usize,
    loading: bool,
    load_generation: usize,
    error: Option<SharedString>,
    counts_by_filter: [Option<usize>; PullRequestPickerFilter::ALL.len()],
    current_pull_request_number: Option<u32>,
    filter: PullRequestPickerFilter,
    workspace: WeakEntity<Workspace>,
    focused_dock: Option<DockPosition>,
    all_worktrees: Vec<GitWorktree>,
    remote_name: String,
}

impl PullRequestPickerDelegate {
    fn existing_worktree_for_branch(&self, branch: &str) -> Option<PathBuf> {
        self.all_worktrees
            .iter()
            .find(|worktree| worktree.branch_name() == Some(branch))
            .map(|worktree| worktree.path.clone())
    }

    fn is_current_pull_request(&self, pull_request: &OpenPullRequest) -> bool {
        self.current_pull_request_number == Some(pull_request.number)
    }

    fn worktree_name_for_branch(branch: &str) -> String {
        let mut name = branch
            .chars()
            .map(|ch| {
                if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '.' {
                    ch
                } else {
                    '-'
                }
            })
            .collect::<String>();
        while name.contains("--") {
            name = name.replace("--", "-");
        }
        let name = name.trim_matches('-');
        if name.is_empty() {
            "pull-request".to_string()
        } else {
            name.to_string()
        }
    }

    fn clamp_selected_index(&mut self) {
        self.selected_index = self
            .selected_index
            .min(self.matches.len().saturating_sub(1));
    }
}

impl PickerDelegate for PullRequestPickerDelegate {
    type ListItem = AnyElement;

    fn placeholder_text(&self, _window: &mut Window, _cx: &mut App) -> Arc<str> {
        self.filter.placeholder().into()
    }

    fn editor_position(&self) -> PickerEditorPosition {
        PickerEditorPosition::Start
    }

    fn no_matches_text(&self, _window: &mut Window, _cx: &mut App) -> Option<SharedString> {
        if self.loading {
            Some("Loading pull requests…".into())
        } else if let Some(error) = &self.error {
            Some(format!("Failed to load pull requests: {error}").into())
        } else {
            Some(self.filter.no_matches_text().into())
        }
    }

    fn match_count(&self) -> usize {
        self.matches.len()
    }

    fn selected_index(&self) -> usize {
        self.selected_index
    }

    fn set_selected_index(
        &mut self,
        ix: usize,
        _window: &mut Window,
        _cx: &mut Context<Picker<Self>>,
    ) {
        self.selected_index = ix.min(self.matches.len().saturating_sub(1));
    }

    fn can_select(&self, ix: usize, _window: &mut Window, _cx: &mut Context<Picker<Self>>) -> bool {
        self.matches.get(ix).is_some()
    }

    fn update_matches(
        &mut self,
        query: String,
        window: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) -> Task<()> {
        let pull_requests = self.pull_requests.clone();
        if query.is_empty() {
            self.matches = pull_requests
                .into_iter()
                .map(|pull_request| PullRequestMatch {
                    pull_request,
                    positions: Vec::new(),
                })
                .collect();
            self.clamp_selected_index();
            return Task::ready(());
        }

        let candidates = pull_requests
            .iter()
            .enumerate()
            .map(|(ix, pull_request)| {
                if let Some(author) = &pull_request.author {
                    StringMatchCandidate::new(
                        ix,
                        &format!("{} {} {}", pull_request.title, pull_request.branch, author),
                    )
                } else {
                    StringMatchCandidate::new(
                        ix,
                        &format!("{} {}", pull_request.title, pull_request.branch),
                    )
                }
            })
            .collect::<Vec<_>>();
        let executor = cx.background_executor().clone();
        let task = cx.background_executor().spawn(async move {
            fuzzy::match_strings(
                &candidates,
                &query,
                true,
                true,
                10000,
                &Default::default(),
                executor,
            )
            .await
        });

        cx.spawn_in(window, async move |picker, cx| {
            let fuzzy_matches = task.await;
            picker
                .update(cx, |picker, cx| {
                    picker.delegate.matches = fuzzy_matches
                        .into_iter()
                        .map(|candidate| PullRequestMatch {
                            pull_request: pull_requests[candidate.candidate_id].clone(),
                            positions: candidate.positions,
                        })
                        .collect();
                    picker.delegate.clamp_selected_index();
                    cx.notify();
                })
                .log_err();
        })
    }

    fn confirm(&mut self, _secondary: bool, window: &mut Window, cx: &mut Context<Picker<Self>>) {
        let Some(entry) = self.matches.get(self.selected_index).cloned() else {
            return;
        };
        if self.is_current_pull_request(&entry.pull_request) {
            return;
        }

        if let Some(path) = self.existing_worktree_for_branch(&entry.pull_request.branch) {
            window.dispatch_action(Box::new(OpenWorktreeInNewWindow { path }), cx);
            cx.emit(DismissEvent);
            return;
        }

        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };

        workspace.update(cx, |workspace, cx| {
            crate::worktree_service::handle_create_branch_worktree_in_new_window(
                workspace,
                Self::worktree_name_for_branch(&entry.pull_request.branch),
                self.remote_name.clone(),
                entry.pull_request.branch.clone(),
                window,
                self.focused_dock,
                cx,
            );
        });
        cx.emit(DismissEvent);
    }

    fn dismissed(&mut self, _window: &mut Window, cx: &mut Context<Picker<Self>>) {
        cx.emit(DismissEvent);
    }

    fn render_match(
        &self,
        ix: usize,
        selected: bool,
        _window: &mut Window,
        _cx: &mut Context<Picker<Self>>,
    ) -> Option<Self::ListItem> {
        let entry = self.matches.get(ix)?;
        let is_current_pull_request = self.is_current_pull_request(&entry.pull_request);
        let title_len = entry.pull_request.title.len();
        let title_positions = entry
            .positions
            .iter()
            .copied()
            .filter(|position| *position < title_len)
            .collect::<Vec<_>>();
        let icon_color = if entry.pull_request.is_draft {
            Color::Muted
        } else {
            Color::Success
        };
        let mut subtitle = entry.pull_request.branch.clone();
        if let Some(author) = &entry.pull_request.author {
            subtitle.push_str(" · ");
            subtitle.push_str(author);
        }

        Some(
            ListItem::new(SharedString::from(format!(
                "pull-request-{}",
                entry.pull_request.number
            )))
            .inset(true)
            .spacing(ListItemSpacing::Sparse)
            .disabled(is_current_pull_request)
            .toggle_state(selected)
            .child(
                h_flex()
                    .w_full()
                    .gap_2p5()
                    .child(
                        div().flex_shrink_0().child(
                            Icon::new(IconName::PullRequest)
                                .size(IconSize::Small)
                                .color(icon_color),
                        ),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .w_full()
                            .min_w_0()
                            .child(
                                HighlightedLabel::new(
                                    entry.pull_request.title.clone(),
                                    title_positions,
                                )
                                .truncate(),
                            )
                            .child(
                                Label::new(subtitle)
                                    .size(LabelSize::Small)
                                    .color(Color::Muted)
                                    .truncate(),
                            ),
                    ),
            )
            .into_any_element(),
        )
    }
}
