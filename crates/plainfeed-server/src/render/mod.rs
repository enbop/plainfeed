mod markdown;
mod maud;

use std::fmt;

pub use maud::MaudRenderer;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderError(String);

impl RenderError {
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for RenderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for RenderError {}

pub trait Renderer: fmt::Debug + Send + Sync {
    fn feed_page(&self, view: &FeedPageView) -> Result<String, RenderError>;
    fn feed_fragment(&self, view: &FeedView) -> Result<String, RenderError>;
    fn entry_page(&self, view: &EntryPageView) -> Result<String, RenderError>;
    fn entry_reader_fragment(&self, view: &EntryView) -> Result<String, RenderError>;
    fn entry_fragment(&self, view: &EntryView) -> Result<String, RenderError>;
    fn settings_page(&self, view: &SettingsView) -> Result<String, RenderError>;
}

#[derive(Debug, Clone)]
pub struct FeedPageView {
    pub unread: usize,
    pub total: usize,
    pub sync_summary: String,
    pub show_settings: bool,
    pub feed: FeedView,
}

#[derive(Debug, Clone)]
pub struct FeedView {
    pub conflict: Option<ConflictView>,
    pub channels: Vec<ChannelView>,
    pub entries: Vec<EntrySummaryView>,
    pub empty: EmptyFeed,
}

#[derive(Debug, Clone)]
pub struct EntryPageView {
    pub unread: usize,
    pub total: usize,
    pub sync_summary: String,
    pub show_settings: bool,
    pub entry: EntryView,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmptyFeed {
    All,
    Channel,
}

#[derive(Debug, Clone)]
pub struct ConflictView {
    pub reason: String,
    pub local_base: String,
    pub remote_tip: String,
}

#[derive(Debug, Clone)]
pub struct ChannelView {
    pub label: String,
    pub page_url: String,
    pub fragment_url: String,
    pub count: usize,
    pub selected: bool,
}

#[derive(Debug, Clone)]
pub struct EntryView {
    pub id: String,
    pub article_class: String,
    pub unread: bool,
    pub published: String,
    pub source_name: String,
    pub title: String,
    pub summary: Option<String>,
    pub tags: Vec<String>,
    pub body: TrustedHtml,
    pub favorite: bool,
    pub comments: Vec<CommentView>,
}

#[derive(Debug, Clone)]
pub struct EntrySummaryView {
    pub id: String,
    pub article_class: String,
    pub unread: bool,
    pub published: String,
    pub source_name: String,
    pub title: String,
    pub summary: Option<String>,
    pub tags: Vec<String>,
    pub page_url: String,
    pub fragment_url: String,
}

impl EntrySummaryView {
    pub fn from_entry(entry: &plainfeed_core::Entry) -> Self {
        let unread = entry.state.read_at.is_none();
        let id = entry.metadata.id.clone();
        Self {
            id: id.clone(),
            article_class: if unread {
                "entry-summary is-unread".to_owned()
            } else {
                "entry-summary".to_owned()
            },
            unread,
            published: entry.metadata.published.clone(),
            source_name: entry.metadata.source.name.clone(),
            title: entry.metadata.title.clone(),
            summary: entry.metadata.summary.clone(),
            tags: entry.metadata.tags.clone(),
            page_url: format!("/entries/{id}"),
            fragment_url: format!("/fragments/entries/{id}"),
        }
    }
}

impl EntryView {
    pub fn from_entry(entry: &plainfeed_core::Entry) -> Self {
        let unread = entry.state.read_at.is_none();
        Self {
            id: entry.metadata.id.clone(),
            article_class: if unread {
                "entry-card is-unread".to_owned()
            } else {
                "entry-card".to_owned()
            },
            unread,
            published: entry.metadata.published.clone(),
            source_name: entry.metadata.source.name.clone(),
            title: entry.metadata.title.clone(),
            summary: entry.metadata.summary.clone(),
            tags: entry.metadata.tags.clone(),
            body: markdown::render(&entry.body),
            favorite: entry.state.favorite,
            comments: entry
                .state
                .comments
                .iter()
                .map(|comment| CommentView {
                    body: comment.body.clone(),
                    created_at: comment.created_at.clone(),
                })
                .collect(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct CommentView {
    pub body: String,
    pub created_at: String,
}

#[derive(Debug, Clone)]
pub struct TrustedHtml(String);

impl TrustedHtml {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone)]
pub struct SettingsView {
    pub remote_url: String,
    pub token_status: String,
    pub notice: Option<SettingsNotice>,
    pub environment_override: bool,
}

#[derive(Debug, Clone)]
pub struct SettingsNotice {
    pub message: String,
    pub error: bool,
}
