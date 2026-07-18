#![cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]

mod render;

use plainfeed_core::{Channel, Entry, Error as StoreError, Store};
use std::borrow::Cow;
use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use render::{
    ChannelView, ConflictView, EmptyFeed, EntryPageView, EntrySummaryView, EntryView, FeedPageView,
    FeedView,
};
pub use render::{MaudRenderer, RenderError, Renderer, SettingsNotice, SettingsView};

const APP_JS: &str = include_str!("../../../web/app.js");
const STYLE_CSS: &str = include_str!("../../../web/style.css");
const HTMX_JS: &[u8] = include_bytes!("../../../web/vendor/htmx.min.js");

#[derive(Debug)]
pub struct Response {
    pub status: u16,
    pub content_type: &'static str,
    pub cache_control: &'static str,
    pub body: Cow<'static, [u8]>,
}

#[derive(Clone)]
pub struct Reader {
    renderer: Arc<dyn Renderer>,
}

impl std::fmt::Debug for Reader {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("Reader").finish_non_exhaustive()
    }
}

impl Default for Reader {
    fn default() -> Self {
        ReaderBuilder::default().build()
    }
}

impl Reader {
    pub fn builder() -> ReaderBuilder {
        ReaderBuilder::default()
    }

    pub fn handle_request(
        &self,
        method: &str,
        path_with_query: &str,
        body: &[u8],
        data_root: &Path,
    ) -> Response {
        route_with_renderer(
            method,
            path_with_query,
            body,
            data_root,
            false,
            self.renderer.as_ref(),
        )
    }

    pub fn handle_service_request(
        &self,
        method: &str,
        path_with_query: &str,
        body: &[u8],
        data_root: &Path,
    ) -> Response {
        route_with_renderer(
            method,
            path_with_query,
            body,
            data_root,
            true,
            self.renderer.as_ref(),
        )
    }

    pub fn render_settings(&self, view: &SettingsView) -> Result<String, RenderError> {
        self.renderer.settings_page(view)
    }
}

pub struct ReaderBuilder {
    renderer: Arc<dyn Renderer>,
}

impl Default for ReaderBuilder {
    fn default() -> Self {
        Self {
            renderer: Arc::new(MaudRenderer),
        }
    }
}

impl ReaderBuilder {
    pub fn renderer(mut self, renderer: impl Renderer + 'static) -> Self {
        self.renderer = Arc::new(renderer);
        self
    }

    pub fn build(self) -> Reader {
        Reader {
            renderer: self.renderer,
        }
    }
}

/// Handle one reader request without depending on a particular HTTP runtime.
///
/// Both the `wasi:http/proxy` compatibility component and the long-running
/// Axum service use this boundary, so file-format and rendering behavior stay
/// identical across deployment modes.
pub fn handle_request(
    method: &str,
    path_with_query: &str,
    body: &[u8],
    data_root: &Path,
) -> Response {
    Reader::default().handle_request(method, path_with_query, body, data_root)
}

/// Handle one request for the combined service, including its settings link.
pub fn handle_service_request(
    method: &str,
    path_with_query: &str,
    body: &[u8],
    data_root: &Path,
) -> Response {
    Reader::default().handle_service_request(method, path_with_query, body, data_root)
}

impl Response {
    fn text(status: u16, content_type: &'static str, body: impl Into<String>) -> Self {
        Self {
            status,
            content_type,
            cache_control: "no-store",
            body: Cow::Owned(body.into().into_bytes()),
        }
    }

    fn static_bytes(content_type: &'static str, body: &'static [u8]) -> Self {
        Self {
            status: 200,
            content_type,
            cache_control: "public, max-age=3600",
            body: Cow::Borrowed(body),
        }
    }

    fn no_content() -> Self {
        Self {
            status: 204,
            content_type: "text/plain; charset=utf-8",
            cache_control: "no-store",
            body: Cow::Borrowed(&[]),
        }
    }
}

#[cfg(test)]
fn route(
    method: &str,
    path_with_query: &str,
    body: &[u8],
    data_root: &Path,
    show_settings: bool,
) -> Response {
    let renderer = MaudRenderer;
    route_with_renderer(
        method,
        path_with_query,
        body,
        data_root,
        show_settings,
        &renderer,
    )
}

fn route_with_renderer(
    method: &str,
    path_with_query: &str,
    body: &[u8],
    data_root: &Path,
    show_settings: bool,
    renderer: &dyn Renderer,
) -> Response {
    let path = path_with_query.split('?').next().unwrap_or(path_with_query);
    if plainfeed_sync_core::update_is_locked(data_root)
        && (path == "/"
            || path == "/fragments/feed"
            || path.starts_with("/entries/")
            || path.starts_with("/fragments/entries/")
            || method == "POST")
    {
        return Response::text(
            503,
            "text/plain; charset=utf-8",
            "Plainfeed is activating a synchronized snapshot; retry shortly.\n",
        );
    }
    let selected_channel = query_value(path_with_query, "channel");
    if method == "GET" {
        if let Some(entry_id) = route_entry_id(path, "/entries/") {
            return render_entry(data_root, entry_id, false, show_settings, renderer);
        }
        if let Some(entry_id) = route_entry_id(path, "/fragments/entries/") {
            return render_entry(data_root, entry_id, true, show_settings, renderer);
        }
    }
    match (method, path) {
        ("GET", "/") => render_feed(
            data_root,
            selected_channel.as_deref(),
            false,
            show_settings,
            renderer,
        ),
        ("GET", "/fragments/feed") => render_feed(
            data_root,
            selected_channel.as_deref(),
            true,
            show_settings,
            renderer,
        ),
        ("GET", "/app.js") => {
            Response::static_bytes("text/javascript; charset=utf-8", APP_JS.as_bytes())
        }
        ("GET", "/style.css") => {
            Response::static_bytes("text/css; charset=utf-8", STYLE_CSS.as_bytes())
        }
        ("GET", "/vendor/htmx.min.js") => {
            Response::static_bytes("text/javascript; charset=utf-8", HTMX_JS)
        }
        ("GET", "/health") => Response::text(200, "text/plain; charset=utf-8", "ok\n"),
        ("POST", _) => route_mutation(path, body, data_root, renderer),
        _ => Response::text(404, "text/plain; charset=utf-8", "not found\n"),
    }
}

fn route_entry_id<'a>(path: &'a str, prefix: &str) -> Option<&'a str> {
    let id = path.strip_prefix(prefix)?;
    (!id.is_empty() && !id.contains('/')).then_some(id)
}

fn route_mutation(path: &str, body: &[u8], data_root: &Path, renderer: &dyn Renderer) -> Response {
    let Some(remainder) = path.strip_prefix("/entries/") else {
        return Response::text(404, "text/plain; charset=utf-8", "not found\n");
    };
    let Some((entry_id, action)) = remainder.split_once('/') else {
        return Response::text(404, "text/plain; charset=utf-8", "not found\n");
    };
    let store = Store::open(data_root);
    let timestamp = now_rfc3339();
    let result = match action {
        "read" => store.mark_read(entry_id, &timestamp).map(|_| None),
        "favorite" => {
            let form = parse_form(body);
            let favorite = form
                .iter()
                .find(|(key, _)| key == "favorite")
                .map(|(_, value)| value == "true");
            match favorite {
                Some(favorite) => store
                    .set_favorite(entry_id, favorite)
                    .map(|_| Some(entry_id)),
                None => return bad_request("missing favorite field"),
            }
        }
        "comments" => {
            let form = parse_form(body);
            let comment = form
                .iter()
                .find(|(key, _)| key == "comment")
                .map(|(_, value)| value.as_str());
            match comment {
                Some(comment) => store
                    .add_comment(entry_id, &new_comment_id(), &timestamp, comment)
                    .map(|_| Some(entry_id)),
                None => return bad_request("missing comment field"),
            }
        }
        _ => return Response::text(404, "text/plain; charset=utf-8", "not found\n"),
    };

    match result {
        Ok(None) => Response::no_content(),
        Ok(Some(entry_id)) => match find_entry(&store, entry_id) {
            Ok(entry) => match renderer.entry_fragment(&EntryView::from_entry(&entry)) {
                Ok(html) => Response::text(200, "text/html; charset=utf-8", html),
                Err(error) => render_error(error),
            },
            Err(error) => server_error(error),
        },
        Err(StoreError::EntryNotFound(_)) => {
            Response::text(404, "text/plain; charset=utf-8", "entry not found\n")
        }
        Err(StoreError::EmptyComment) => bad_request("comment cannot be empty"),
        Err(error) => server_error(error),
    }
}

fn find_entry(store: &Store, id: &str) -> Result<Entry, StoreError> {
    store
        .entries()?
        .into_iter()
        .find(|entry| entry.metadata.id == id)
        .ok_or_else(|| StoreError::EntryNotFound(id.to_owned()))
}

fn render_feed(
    data_root: &Path,
    selected_channel: Option<&str>,
    fragment: bool,
    show_settings: bool,
    renderer: &dyn Renderer,
) -> Response {
    let store = Store::open(data_root);
    let conflict = plainfeed_sync_core::ConflictReport::read_from(data_root)
        .ok()
        .flatten();
    match (store.entries(), store.channels()) {
        (Ok(entries), Ok(channels)) => {
            let unread = entries
                .iter()
                .filter(|entry| entry.state.read_at.is_none())
                .count();
            let feed = build_feed_view(&entries, &channels, selected_channel, conflict.as_ref());
            let rendered = if fragment {
                renderer.feed_fragment(&feed)
            } else {
                renderer.feed_page(&FeedPageView {
                    unread,
                    total: entries.len(),
                    sync_summary: render_sync_summary(data_root),
                    show_settings,
                    feed,
                })
            };
            match rendered {
                Ok(html) => Response::text(200, "text/html; charset=utf-8", html),
                Err(error) => render_error(error),
            }
        }
        (Err(error), _) | (_, Err(error)) => server_error(error),
    }
}

fn render_entry(
    data_root: &Path,
    entry_id: &str,
    fragment: bool,
    show_settings: bool,
    renderer: &dyn Renderer,
) -> Response {
    let store = Store::open(data_root);
    let entries = match store.entries() {
        Ok(entries) => entries,
        Err(error) => return server_error(error),
    };
    let unread = entries
        .iter()
        .filter(|entry| entry.state.read_at.is_none())
        .count();
    let total = entries.len();
    let Some(entry) = entries
        .into_iter()
        .find(|entry| entry.metadata.id == entry_id)
    else {
        return Response::text(404, "text/plain; charset=utf-8", "entry not found\n");
    };
    let entry = EntryView::from_entry(&entry);
    let rendered = if fragment {
        renderer.entry_reader_fragment(&entry)
    } else {
        renderer.entry_page(&EntryPageView {
            unread,
            total,
            sync_summary: render_sync_summary(data_root),
            show_settings,
            entry,
        })
    };
    match rendered {
        Ok(html) => Response::text(200, "text/html; charset=utf-8", html),
        Err(error) => render_error(error),
    }
}

fn render_sync_summary(data_root: &Path) -> String {
    if plainfeed_sync_core::ConflictReport::read_from(data_root)
        .ok()
        .flatten()
        .is_some()
    {
        return "sync paused".to_owned();
    }
    if plainfeed_sync_core::PendingPush::read_from(data_root)
        .ok()
        .flatten()
        .is_some()
    {
        return "sync recovery pending".to_owned();
    }
    let dirty = plainfeed_sync_core::DirtyJournal::new(data_root)
        .snapshot()
        .map(|markers| markers.len())
        .unwrap_or_default();
    if dirty > 0 {
        return format!(
            "{dirty} local change{} pending",
            if dirty == 1 { "" } else { "s" }
        );
    }
    match plainfeed_sync_core::SyncState::read_from(data_root) {
        Ok(Some(state)) if state.last_error.is_some() => "sync delayed".to_owned(),
        Ok(Some(state)) if state.last_pull_at.is_some() => "synced".to_owned(),
        _ => "local only".to_owned(),
    }
}

fn build_feed_view(
    entries: &[Entry],
    channels: &[Channel],
    selected_channel: Option<&str>,
    conflict: Option<&plainfeed_sync_core::ConflictReport>,
) -> FeedView {
    let mut navigation = vec![channel_view("All", None, selected_channel, entries.len())];
    for channel in channels {
        let count = entries
            .iter()
            .filter(|entry| entry.metadata.channels.contains(&channel.id))
            .count();
        navigation.push(channel_view(
            &channel.label,
            Some(&channel.id),
            selected_channel,
            count,
        ));
    }

    let visible = entries
        .iter()
        .filter(|entry| {
            selected_channel
                .map(|channel| entry.metadata.channels.iter().any(|id| id == channel))
                .unwrap_or(true)
        })
        .map(EntrySummaryView::from_entry)
        .collect();
    FeedView {
        conflict: conflict.map(|report| ConflictView {
            reason: report.reason.clone(),
            local_base: report
                .local_base
                .clone()
                .unwrap_or_else(|| "unknown".to_owned()),
            remote_tip: report
                .remote_tip
                .clone()
                .unwrap_or_else(|| "unknown".to_owned()),
        }),
        channels: navigation,
        entries: visible,
        empty: if selected_channel.is_some() {
            EmptyFeed::Channel
        } else {
            EmptyFeed::All
        },
    }
}

fn channel_view(
    label: &str,
    channel: Option<&str>,
    selected_channel: Option<&str>,
    count: usize,
) -> ChannelView {
    let selected = channel == selected_channel;
    let page_url = channel
        .map(|id| format!("/?channel={id}"))
        .unwrap_or_else(|| "/".to_owned());
    let fragment_url = channel
        .map(|id| format!("/fragments/feed?channel={id}"))
        .unwrap_or_else(|| "/fragments/feed".to_owned());
    ChannelView {
        label: label.to_owned(),
        page_url,
        fragment_url,
        count,
        selected,
    }
}

fn parse_form(body: &[u8]) -> Vec<(String, String)> {
    String::from_utf8_lossy(body)
        .split('&')
        .filter_map(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            Some((percent_decode(key)?, percent_decode(value)?))
        })
        .collect()
}

fn query_value(path_with_query: &str, key: &str) -> Option<String> {
    let query = path_with_query.split_once('?')?.1;
    parse_form(query.as_bytes())
        .into_iter()
        .find_map(|(name, value)| (name == key).then_some(value))
}

fn percent_decode(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => decoded.push(b' '),
            b'%' if index + 2 < bytes.len() => {
                let high = hex(bytes[index + 1])?;
                let low = hex(bytes[index + 2])?;
                decoded.push(high * 16 + low);
                index += 2;
            }
            b'%' => return None,
            byte => decoded.push(byte),
        }
        index += 1;
    }
    String::from_utf8(decoded).ok()
}

fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn now_rfc3339() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_owned())
}

fn new_comment_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("comment-{nanos}")
}

fn bad_request(message: &str) -> Response {
    Response::text(400, "text/plain; charset=utf-8", format!("{message}\n"))
}

fn render_error(error: RenderError) -> Response {
    eprintln!("plainfeed renderer: {error}");
    Response::text(
        500,
        "text/plain; charset=utf-8",
        "plainfeed could not render this page\n",
    )
}

fn server_error(error: StoreError) -> Response {
    eprintln!("plainfeed: {error}");
    Response::text(
        500,
        "text/plain; charset=utf-8",
        "plainfeed could not read the data directory\n",
    )
}

#[cfg(all(target_arch = "wasm32", feature = "proxy-component"))]
mod wasi_http {
    use super::{Response, handle_request};
    use std::io::{Read, Write};
    use std::path::PathBuf;
    use wasip2::http::types::{
        Fields, IncomingBody, IncomingRequest, Method, OutgoingBody, OutgoingResponse,
        ResponseOutparam,
    };

    pub struct Handler;

    impl wasip2::exports::http::incoming_handler::Guest for Handler {
        fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
            let method = match request.method() {
                Method::Get => "GET",
                Method::Head => "HEAD",
                Method::Post => "POST",
                _ => "OTHER",
            };
            let path = request.path_with_query().unwrap_or_else(|| "/".to_owned());
            let body = read_body(&request).unwrap_or_default();
            // The first deployment contract deliberately uses one fixed guest
            // path. The host chooses the real directory with `--dir HOST::/data`.
            let data_root = PathBuf::from("/data");
            let route_method = if method == "HEAD" { "GET" } else { method };
            send(
                handle_request(route_method, &path, &body, &data_root),
                response_out,
                method == "HEAD",
            );
        }
    }

    fn read_body(request: &IncomingRequest) -> Result<Vec<u8>, ()> {
        let body = request.consume()?;
        let mut stream = body.stream()?;
        let mut bytes = Vec::new();
        stream.read_to_end(&mut bytes).map_err(|_| ())?;
        drop(stream);
        let _trailers = IncomingBody::finish(body);
        Ok(bytes)
    }

    fn send(response: Response, response_out: ResponseOutparam, suppress_body: bool) {
        let content_length = if suppress_body {
            0
        } else {
            response.body.len()
        };
        let headers = Fields::from_list(&[
            (
                "content-type".to_owned(),
                response.content_type.as_bytes().to_vec(),
            ),
            (
                "cache-control".to_owned(),
                response.cache_control.as_bytes().to_vec(),
            ),
            (
                "content-length".to_owned(),
                content_length.to_string().into_bytes(),
            ),
        ])
        .expect("valid response headers");
        let outgoing = OutgoingResponse::new(headers);
        outgoing
            .set_status_code(response.status)
            .expect("valid response status");
        let body = outgoing.body().expect("new response body");
        ResponseOutparam::set(response_out, Ok(outgoing));

        if suppress_body {
            let _ = OutgoingBody::finish(body, None);
            return;
        }

        let Ok(mut stream) = body.write() else {
            let _ = OutgoingBody::finish(body, None);
            return;
        };
        if stream.write_all(&response.body).is_ok() {
            let _ = stream.flush();
        }
        drop(stream);
        // A browser may cancel a navigation or asset request after the response
        // has been accepted. `closed` is normal in that case and must not trap
        // the component worker.
        let _ = OutgoingBody::finish(body, None);
    }
}

#[cfg(all(target_arch = "wasm32", feature = "proxy-component"))]
use wasi_http::Handler;

#[cfg(all(target_arch = "wasm32", feature = "proxy-component"))]
wasip2::http::proxy::export!(Handler);

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct TestRenderer;

    impl Renderer for TestRenderer {
        fn feed_page(&self, _view: &FeedPageView) -> Result<String, RenderError> {
            Ok("<main>custom reader</main>".to_owned())
        }

        fn feed_fragment(&self, _view: &FeedView) -> Result<String, RenderError> {
            Ok("<main>custom fragment</main>".to_owned())
        }

        fn entry_page(&self, _view: &EntryPageView) -> Result<String, RenderError> {
            Ok("<main>custom entry page</main>".to_owned())
        }

        fn entry_reader_fragment(&self, _view: &EntryView) -> Result<String, RenderError> {
            Ok("<main>custom reader fragment</main>".to_owned())
        }

        fn entry_fragment(&self, _view: &EntryView) -> Result<String, RenderError> {
            Ok("<article>custom entry</article>".to_owned())
        }

        fn settings_page(&self, _view: &SettingsView) -> Result<String, RenderError> {
            Ok("<main>custom settings</main>".to_owned())
        }
    }

    #[test]
    fn reader_builder_selects_a_renderer_without_changing_routes() {
        let temporary = tempfile::tempdir().unwrap();
        let reader = Reader::builder().renderer(TestRenderer).build();

        let response = reader.handle_request("GET", "/", &[], temporary.path());
        let body = String::from_utf8(response.body.into_owned()).unwrap();

        assert_eq!(body, "<main>custom reader</main>");
    }

    #[test]
    fn reader_builder_also_selects_the_entry_page_renderer() {
        let data = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/data");
        let reader = Reader::builder().renderer(TestRenderer).build();

        let response = reader.handle_request("GET", "/entries/20260716-git-wasi", &[], &data);
        let body = String::from_utf8(response.body.into_owned()).unwrap();

        assert_eq!(body, "<main>custom entry page</main>");
    }

    #[test]
    fn conflict_banner_keeps_the_last_valid_feed_available() {
        let temporary = tempfile::tempdir().unwrap();
        let mut report = plainfeed_sync_core::ConflictReport::new(
            "remote <state> changed",
            "2026-07-17T05:00:00Z",
        );
        report.local_base = Some("aaa".to_owned());
        report.remote_tip = Some("bbb".to_owned());
        report.write_to(temporary.path()).unwrap();

        let response = route("GET", "/", &[], temporary.path(), false);
        let body = String::from_utf8(response.body.into_owned()).unwrap();
        assert_eq!(response.status, 200);
        assert!(body.contains("Synchronization needs attention"));
        assert!(body.contains("remote &lt;state&gt; changed"));
        assert!(!body.contains("remote <state> changed"));
        assert!(body.contains("aaa"));
        assert!(body.contains("bbb"));
        assert!(body.contains("Your feed is empty"));
        assert!(body.contains("sync paused"));
    }

    #[test]
    fn form_decoding_handles_utf8() {
        assert_eq!(
            parse_form(b"comment=hello+%E4%B8%96%E7%95%8C"),
            [("comment".to_owned(), "hello 世界".to_owned())]
        );
    }

    #[test]
    fn static_assets_are_cacheable_but_reader_pages_are_not() {
        let temporary = tempfile::tempdir().unwrap();
        let stylesheet = route("GET", "/style.css", &[], temporary.path(), false);
        let page = route("GET", "/", &[], temporary.path(), false);

        assert_eq!(stylesheet.cache_control, "public, max-age=3600");
        assert_eq!(page.cache_control, "no-store");
    }

    #[test]
    fn data_routes_are_retryable_while_an_update_is_locked() {
        let temporary = tempfile::tempdir().unwrap();
        let _lock = plainfeed_sync_core::UpdateLock::acquire(temporary.path()).unwrap();

        let feed = route("GET", "/", &[], temporary.path(), false);
        let entry = route("GET", "/entries/example", &[], temporary.path(), false);
        let mutation = route(
            "POST",
            "/entries/example/read",
            &[],
            temporary.path(),
            false,
        );
        let health = route("GET", "/health", &[], temporary.path(), false);

        assert_eq!(feed.status, 503);
        assert_eq!(entry.status, 503);
        assert_eq!(mutation.status, 503);
        assert_eq!(health.status, 200);
    }

    #[test]
    fn channel_route_returns_summaries_for_matching_entries() {
        let data = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/data");
        let response = route("GET", "/?channel=technology", &[], &data, false);
        let body = String::from_utf8(response.body.into_owned()).unwrap();
        assert_eq!(response.status, 200);
        assert!(body.contains("Git synchronization is viable"));
        assert!(!body.contains("A file-backed reader running under Wasmtime"));
        assert!(body.contains("The earlier experiment proved authenticated HTTPS"));
        assert!(!body.contains("The Git experiment demonstrated"));
        assert!(!body.contains("class=\"entry-body\""));
        assert!(body.contains("href=\"/entries/20260716-git-wasi\""));
        assert!(body.contains("hx-get=\"/fragments/entries/20260716-git-wasi\""));
    }

    #[test]
    fn entry_route_supports_full_page_and_htmx_reader_fragment() {
        let data = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/data");
        let page = route("GET", "/entries/20260716-git-wasi", &[], &data, false);
        let fragment = route(
            "GET",
            "/fragments/entries/20260716-git-wasi",
            &[],
            &data,
            false,
        );
        let page = String::from_utf8(page.body.into_owned()).unwrap();
        let fragment = String::from_utf8(fragment.body.into_owned()).unwrap();

        assert!(page.starts_with("<!DOCTYPE html>"));
        assert!(page.contains("The Git experiment demonstrated"));
        assert!(page.contains("<h1>Git synchronization is viable"));
        assert!(page.contains("id=\"reader-surface\""));
        assert!(page.contains("data-history-back"));
        assert!(fragment.contains("The Git experiment demonstrated"));
        assert!(fragment.contains("Back to feed"));
        assert!(!fragment.contains("<!DOCTYPE html>"));
    }

    #[test]
    fn missing_entry_route_is_not_found() {
        let data = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/data");
        let response = route("GET", "/entries/not-here", &[], &data, false);

        assert_eq!(response.status, 404);
        assert_eq!(response.body.as_ref(), b"entry not found\n");
    }

    #[test]
    fn settings_link_is_only_exposed_by_the_combined_service() {
        let data = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/data");
        let compatibility = handle_request("GET", "/", &[], &data);
        let service = handle_service_request("GET", "/", &[], &data);
        let compatibility = String::from_utf8(compatibility.body.into_owned()).unwrap();
        let service = String::from_utf8(service.body.into_owned()).unwrap();

        assert!(!compatibility.contains("href=\"/settings\""));
        assert!(service.contains("href=\"/settings\""));
        assert!(service.contains("aria-label=\"Settings\""));
    }
}
