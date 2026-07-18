use maud::{DOCTYPE, Markup, PreEscaped, html};

use super::{
    ChannelView, ConflictView, EmptyFeed, EntryPageView, EntrySummaryView, EntryView, FeedPageView,
    FeedView, RenderError, Renderer, SettingsView,
};

#[derive(Debug, Default, Clone, Copy)]
pub struct MaudRenderer;

impl Renderer for MaudRenderer {
    fn feed_page(&self, view: &FeedPageView) -> Result<String, RenderError> {
        Ok(feed_page(view).into_string())
    }

    fn feed_fragment(&self, view: &FeedView) -> Result<String, RenderError> {
        Ok(html! { title { "Plainfeed" } (feed_shell(view)) }.into_string())
    }

    fn entry_page(&self, view: &EntryPageView) -> Result<String, RenderError> {
        Ok(entry_page(view).into_string())
    }

    fn entry_reader_fragment(&self, view: &EntryView) -> Result<String, RenderError> {
        Ok(html! {
            title { (view.title) " · Plainfeed" }
            (entry_reader(view))
        }
        .into_string())
    }

    fn entry_fragment(&self, view: &EntryView) -> Result<String, RenderError> {
        Ok(entry(view).into_string())
    }

    fn settings_page(&self, view: &SettingsView) -> Result<String, RenderError> {
        Ok(settings_page(view).into_string())
    }
}

fn feed_page(view: &FeedPageView) -> Markup {
    html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                meta name="color-scheme" content="light dark";
                title { "Plainfeed" }
                link rel="stylesheet" href="/style.css";
                script defer src="/vendor/htmx.min.js" {}
                script defer src="/app.js" {}
            }
            body {
                (site_header(view.unread, view.total, &view.sync_summary, view.show_settings))
                main #reader-surface hx-history-elt {
                    (feed_shell(&view.feed))
                }
            }
        }
    }
}

fn entry_page(view: &EntryPageView) -> Markup {
    html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                meta name="color-scheme" content="light dark";
                title { (view.entry.title) " · Plainfeed" }
                link rel="stylesheet" href="/style.css";
                script defer src="/vendor/htmx.min.js" {}
                script defer src="/app.js" {}
            }
            body {
                (site_header(view.unread, view.total, &view.sync_summary, view.show_settings))
                main #reader-surface hx-history-elt {
                    (entry_reader(&view.entry))
                }
            }
        }
    }
}

fn site_header(unread: usize, total: usize, sync_summary: &str, show_settings: bool) -> Markup {
    html! {
        header.site-header {
            a.brand href="/" aria-label="Plainfeed home" { "Plainfeed" }
            div.header-status {
                p {
                    strong { (unread) } " unread · "
                    strong { (total) } " total "
                    span.sync-summary { "· " (sync_summary) }
                }
                @if show_settings {
                    a.settings-link href="/settings" aria-label="Settings" title="Settings" {
                        svg aria-hidden="true" viewBox="0 0 24 24" {
                            path d="M19.1 13a7.7 7.7 0 0 0 0-2l2.1-1.6-2-3.4-2.5 1a8 8 0 0 0-1.7-1L14.6 3h-4l-.4 3a8 8 0 0 0-1.7 1L6 6 4 9.4 6.1 11a7.7 7.7 0 0 0 0 2L4 14.6 6 18l2.5-1a8 8 0 0 0 1.7 1l.4 3h4l.4-3a8 8 0 0 0 1.7-1l2.5 1 2-3.4L19.1 13ZM12.6 15.5a3.5 3.5 0 1 1 0-7 3.5 3.5 0 0 1 0 7Z" {}
                        }
                    }
                }
            }
        }
    }
}

fn feed_shell(view: &FeedView) -> Markup {
    html! {
        section #feed-shell.feed-shell {
            @if let Some(conflict) = &view.conflict {
                (conflict_banner(conflict))
            }
            nav.channel-tabs aria-label="Feed channels" {
                @for channel in &view.channels {
                    (channel_link(channel))
                }
            }
            div #feed.feed {
                @if view.entries.is_empty() {
                    section.empty {
                        h2 { "Nothing here yet" }
                        @match view.empty {
                            EmptyFeed::Channel => p { "No entries in this channel." },
                            EmptyFeed::All => p {
                                "Your feed is empty. Add a v1 Markdown entry under "
                                code { "content/" }
                                "."
                            },
                        }
                    }
                } @else {
                    @for item in &view.entries {
                        (entry_summary(item))
                    }
                }
            }
        }
    }
}

fn entry_summary(view: &EntrySummaryView) -> Markup {
    let article_id = format!("entry-{}", view.id);
    html! {
        article id=(article_id) class=(&view.article_class) {
            div.entry-meta {
                span.unread-dot aria-label=[view.unread.then_some("Unread")] {}
                time datetime=(&view.published) { (&view.published) }
                span { "·" }
                span { (&view.source_name) }
            }
            h2 {
                a.entry-title-link
                    href=(&view.page_url)
                    hx-get=(&view.fragment_url)
                    hx-target="#reader-surface"
                    hx-swap="innerHTML"
                    hx-push-url=(&view.page_url) {
                    (&view.title)
                }
            }
            @if let Some(summary) = &view.summary {
                p.summary { (summary) }
            }
            div.tags {
                @for tag in &view.tags {
                    span.tag { (tag) }
                }
            }
        }
    }
}

fn entry_reader(view: &EntryView) -> Markup {
    html! {
        section.entry-reader {
            nav.reader-nav aria-label="Article navigation" {
                a.reader-back href="/"
                    data-history-back
                    hx-get="/fragments/feed"
                    hx-target="#reader-surface"
                    hx-swap="innerHTML"
                    hx-push-url="/" {
                    "← Back to feed"
                }
            }
            (entry(view))
        }
    }
}

fn conflict_banner(view: &ConflictView) -> Markup {
    html! {
        aside.sync-conflict role="alert" {
            h2 { "Synchronization needs attention" }
            p { (&view.reason) }
            dl {
                div { dt { "Local base" } dd { code { (&view.local_base) } } }
                div { dt { "Remote tip" } dd { code { (&view.remote_tip) } } }
            }
            p.sync-conflict-help {
                "The last valid feed remains available. Inspect "
                code { ".plainfeed/conflict.toml" }
                ", repair the repository, acknowledge the report, and force synchronization."
            }
        }
    }
}

fn channel_link(view: &ChannelView) -> Markup {
    let class = if view.selected {
        "channel-tab is-active"
    } else {
        "channel-tab"
    };
    html! {
        a class=(class)
            href=(&view.page_url)
            hx-get=(&view.fragment_url)
            hx-target="#feed-shell"
            hx-swap="outerHTML"
            hx-push-url=(&view.page_url)
            aria-current=[view.selected.then_some("page")] {
            (&view.label)
            span { (view.count) }
        }
    }
}

fn entry(view: &EntryView) -> Markup {
    let favorite_label = if view.favorite {
        "Unfavorite"
    } else {
        "Favorite"
    };
    let favorite_value = if view.favorite { "false" } else { "true" };
    let favorite_mark = if view.favorite { "★" } else { "☆" };
    let favorite_url = format!("/entries/{}/favorite", view.id);
    let comments_url = format!("/entries/{}/comments", view.id);
    let comment_id = format!("comment-{}", view.id);
    let article_id = format!("entry-{}", view.id);
    let comment_count = view.comments.len();
    html! {
        article id=(article_id)
            class=(&view.article_class)
            data-entry-id=(&view.id)
            data-unread=(view.unread) {
            header.entry-header {
                div.entry-meta {
                    span.unread-dot aria-label="Unread" {}
                    time datetime=(&view.published) { (&view.published) }
                    span { "·" }
                    span { (&view.source_name) }
                }
                h1 { (&view.title) }
                @if let Some(summary) = &view.summary {
                    p.summary { (summary) }
                }
                div.tags {
                    @for tag in &view.tags {
                        span.tag { (tag) }
                    }
                }
            }
            div.entry-body { (PreEscaped(view.body.as_str())) }
            footer.entry-actions {
                form hx-post=(favorite_url) hx-target="closest article" hx-swap="outerHTML" {
                    input type="hidden" name="favorite" value=(favorite_value);
                    button.favorite type="submit" aria-label=(favorite_label) {
                        (favorite_mark) " " (favorite_label)
                    }
                }
                details {
                    summary {
                        (comment_count) " comment" @if comment_count != 1 { "s" }
                    }
                    div.comments {
                        @if view.comments.is_empty() {
                            p.no-comments { "No comments yet." }
                        } @else {
                            @for comment in &view.comments {
                                blockquote {
                                    p { (&comment.body) }
                                    footer { (&comment.created_at) }
                                }
                            }
                        }
                    }
                    form.comment-form hx-post=(comments_url) hx-target="closest article" hx-swap="outerHTML" {
                        label for=(&comment_id) { "Add a personal comment" }
                        textarea id=(&comment_id) name="comment" rows="3" maxlength="4000" required {}
                        button type="submit" { "Save comment" }
                    }
                }
            }
        }
    }
}

fn settings_page(view: &SettingsView) -> Markup {
    html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                meta name="color-scheme" content="light dark";
                title { "Settings · Plainfeed" }
                link rel="stylesheet" href="/style.css";
            }
            body {
                header.site-header {
                    a.brand href="/" { "Plainfeed" }
                    a.back-link href="/" { "Back to feed" }
                }
                main.settings-page {
                    section.settings-panel {
                        p.settings-kicker { "Service configuration" }
                        h1 { "Connect your data repository" }
                        p.settings-intro { "Plainfeed uses this Git remote for incoming content and reader-state publication." }
                        @if let Some(notice) = &view.notice {
                            @if notice.error {
                                p.settings-notice.is-error role="alert" { (&notice.message) }
                            } @else {
                                p.settings-notice.is-success role="status" { (&notice.message) }
                            }
                        }
                        @if view.environment_override {
                            p.settings-hint {
                                strong { "Environment override active." }
                                " Environment credentials or URL take priority until the service is restarted without them."
                            }
                        }
                        form.settings-form method="post" action="/settings" {
                            label for="remote-url" { "Remote URL" }
                            input #remote-url name="remote_url" type="url" value=(&view.remote_url)
                                placeholder="https://github.com/owner/plainfeed-data.git" required
                                spellcheck="false" autocomplete="url";
                            p.field-help { "Use the HTTPS clone URL. Environment variable: " code { "PLAINFEED_REMOTE_URL" } "." }
                            label for="github-token" { "GitHub personal access token" }
                            input #github-token name="github_token" type="password" value=""
                                placeholder="Leave blank to keep the current token" autocomplete="new-password";
                            p.field-help {
                                "Status: " strong { (&view.token_status) }
                                ". The saved token is never sent back to this page."
                            }
                            label.checkbox-row {
                                input name="clear_token" type="checkbox" value="yes";
                                " Remove the locally stored token"
                            }
                            button.primary-button type="submit" { "Save and synchronize" }
                        }
                        aside.settings-security {
                            strong { "Local secret storage" }
                            p {
                                "The token is stored as plain text in "
                                code { "/data/.plainfeed/service-settings.toml" }
                                ". Keep the host data directory private and restrict its filesystem permissions."
                            }
                        }
                    }
                }
            }
        }
    }
}
