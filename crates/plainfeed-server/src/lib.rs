#![cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]

use plainfeed_core::{Channel, Entry, Error as StoreError, Store};
use pulldown_cmark::{Event, Parser};
use std::borrow::Cow;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

const APP_JS: &str = include_str!("../../../web/app.js");
const STYLE_CSS: &str = include_str!("../../../web/style.css");
const HTMX_JS: &[u8] = include_bytes!("../../../web/vendor/htmx.min.js");

#[derive(Debug)]
pub struct Response {
    pub status: u16,
    pub content_type: &'static str,
    pub body: Cow<'static, [u8]>,
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
    route(method, path_with_query, body, data_root)
}

impl Response {
    fn text(status: u16, content_type: &'static str, body: impl Into<String>) -> Self {
        Self {
            status,
            content_type,
            body: Cow::Owned(body.into().into_bytes()),
        }
    }

    fn static_bytes(content_type: &'static str, body: &'static [u8]) -> Self {
        Self {
            status: 200,
            content_type,
            body: Cow::Borrowed(body),
        }
    }

    fn no_content() -> Self {
        Self {
            status: 204,
            content_type: "text/plain; charset=utf-8",
            body: Cow::Borrowed(&[]),
        }
    }
}

fn route(method: &str, path_with_query: &str, body: &[u8], data_root: &Path) -> Response {
    let path = path_with_query.split('?').next().unwrap_or(path_with_query);
    if plainfeed_sync_core::update_is_locked(data_root)
        && (path == "/" || path == "/fragments/feed" || method == "POST")
    {
        return Response::text(
            503,
            "text/plain; charset=utf-8",
            "Plainfeed is activating a synchronized snapshot; retry shortly.\n",
        );
    }
    let selected_channel = query_value(path_with_query, "channel");
    match (method, path) {
        ("GET", "/") => render_feed(data_root, selected_channel.as_deref(), false),
        ("GET", "/fragments/feed") => render_feed(data_root, selected_channel.as_deref(), true),
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
        ("POST", _) => route_mutation(path, body, data_root),
        _ => Response::text(404, "text/plain; charset=utf-8", "not found\n"),
    }
}

fn route_mutation(path: &str, body: &[u8], data_root: &Path) -> Response {
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
            Ok(entry) => Response::text(200, "text/html; charset=utf-8", render_entry(&entry)),
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

fn render_feed(data_root: &Path, selected_channel: Option<&str>, fragment: bool) -> Response {
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
            let shell = render_feed_shell(&entries, &channels, selected_channel, conflict.as_ref());
            let sync_summary = render_sync_summary(data_root);
            if fragment {
                return Response::text(200, "text/html; charset=utf-8", shell);
            }
            let document = format!(
                r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <meta name="color-scheme" content="light dark">
  <title>Plainfeed</title>
  <link rel="stylesheet" href="/style.css">
  <script defer src="/vendor/htmx.min.js"></script>
  <script defer src="/app.js"></script>
</head>
<body hx-history="false">
  <header class="site-header">
    <a class="brand" href="/" aria-label="Plainfeed home">Plainfeed</a>
    <p><strong>{}</strong> unread · <strong>{}</strong> total <span class="sync-summary">· {}</span></p>
  </header>
  {}
</body>
</html>"#,
                unread,
                entries.len(),
                sync_summary,
                shell
            );
            Response::text(200, "text/html; charset=utf-8", document)
        }
        (Err(error), _) | (_, Err(error)) => server_error(error),
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

fn render_feed_shell(
    entries: &[Entry],
    channels: &[Channel],
    selected_channel: Option<&str>,
    conflict: Option<&plainfeed_sync_core::ConflictReport>,
) -> String {
    let mut navigation = channel_link("All", None, selected_channel, entries.len());
    for channel in channels {
        let count = entries
            .iter()
            .filter(|entry| entry.metadata.channels.contains(&channel.id))
            .count();
        navigation.push_str(&channel_link(
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
        .collect::<Vec<_>>();
    let cards = if visible.is_empty() {
        let message = if selected_channel.is_some() {
            "No entries in this channel."
        } else {
            "Your feed is empty. Add a v1 Markdown entry under <code>content/</code>."
        };
        format!("<section class=\"empty\"><h2>Nothing here yet</h2><p>{message}</p></section>")
    } else {
        visible.into_iter().map(render_entry).collect::<String>()
    };

    let conflict = conflict.map(render_conflict_banner).unwrap_or_default();
    format!(
        r#"<section id="feed-shell" class="feed-shell">
  {conflict}
  <nav class="channel-tabs" aria-label="Feed channels">{navigation}</nav>
  <main id="feed" class="feed">{cards}</main>
</section>"#
    )
}

fn render_conflict_banner(report: &plainfeed_sync_core::ConflictReport) -> String {
    let local_base = report.local_base.as_deref().unwrap_or("unknown");
    let remote_tip = report.remote_tip.as_deref().unwrap_or("unknown");
    format!(
        r#"<aside class="sync-conflict" role="alert">
  <h2>Synchronization needs attention</h2>
  <p>{}</p>
  <dl><div><dt>Local base</dt><dd><code>{}</code></dd></div><div><dt>Remote tip</dt><dd><code>{}</code></dd></div></dl>
  <p class="sync-conflict-help">The last valid feed remains available. Inspect <code>.plainfeed/conflict.toml</code>, repair the repository, acknowledge the report, and force synchronization.</p>
</aside>"#,
        escape_html(&report.reason),
        escape_html(local_base),
        escape_html(remote_tip),
    )
}

fn channel_link(
    label: &str,
    channel: Option<&str>,
    selected_channel: Option<&str>,
    count: usize,
) -> String {
    let selected = channel == selected_channel;
    let page_url = channel
        .map(|id| format!("/?channel={id}"))
        .unwrap_or_else(|| "/".to_owned());
    let fragment_url = channel
        .map(|id| format!("/fragments/feed?channel={id}"))
        .unwrap_or_else(|| "/fragments/feed".to_owned());
    format!(
        "<a class=\"channel-tab{}\" href=\"{}\" hx-get=\"{}\" hx-target=\"#feed-shell\" hx-swap=\"outerHTML\" hx-push-url=\"{}\"{}>{}<span>{}</span></a>",
        if selected { " is-active" } else { "" },
        escape_html(&page_url),
        escape_html(&fragment_url),
        escape_html(&page_url),
        if selected {
            " aria-current=\"page\""
        } else {
            ""
        },
        escape_html(label),
        count
    )
}

fn render_entry(entry: &Entry) -> String {
    let metadata = &entry.metadata;
    let state = &entry.state;
    let unread = state.read_at.is_none();
    let favorite_label = if state.favorite {
        "Unfavorite"
    } else {
        "Favorite"
    };
    let favorite_value = if state.favorite { "false" } else { "true" };
    let favorite_mark = if state.favorite { "★" } else { "☆" };
    let tags = metadata
        .tags
        .iter()
        .map(|tag| format!("<span class=\"tag\">{}</span>", escape_html(tag)))
        .collect::<String>();
    let summary = metadata
        .summary
        .as_ref()
        .cloned()
        .unwrap_or_else(|| plain_text_summary(&entry.body, 280));
    let comments = if state.comments.is_empty() {
        "<p class=\"no-comments\">No comments yet.</p>".to_owned()
    } else {
        state
            .comments
            .iter()
            .map(|comment| {
                format!(
                    "<blockquote><p>{}</p><footer>{}</footer></blockquote>",
                    escape_html(&comment.body).replace('\n', "<br>"),
                    escape_html(&comment.created_at)
                )
            })
            .collect::<String>()
    };
    format!(
        r#"<article id="entry-{id}" class="entry-card{unread_class}" data-entry-id="{id}" data-unread="{unread}">
  <header class="entry-header">
    <div class="entry-meta"><span class="unread-dot" aria-label="Unread"></span><time datetime="{published}">{published}</time><span>·</span><span>{source_name}</span></div>
    <h2><a href="{source_url}" rel="noreferrer">{title}</a></h2>
    <p class="summary">{summary}</p>
    <div class="tags">{tags}</div>
  </header>
  <footer class="entry-actions">
    <a class="read-original" href="{source_url}" rel="noreferrer">Read original <span aria-hidden="true">↗</span></a>
    <form hx-post="/entries/{id}/favorite" hx-target="closest article" hx-swap="outerHTML">
      <input type="hidden" name="favorite" value="{favorite_value}">
      <button class="favorite" type="submit" aria-label="{favorite_label}">{favorite_mark} {favorite_label}</button>
    </form>
    <details>
      <summary>{comment_count} comment{comment_suffix}</summary>
      <div class="comments">{comments}</div>
      <form class="comment-form" hx-post="/entries/{id}/comments" hx-target="closest article" hx-swap="outerHTML">
        <label for="comment-{id}">Add a personal comment</label>
        <textarea id="comment-{id}" name="comment" rows="3" maxlength="4000" required></textarea>
        <button type="submit">Save comment</button>
      </form>
    </details>
  </footer>
</article>"#,
        id = escape_html(&metadata.id),
        unread = unread,
        unread_class = if unread { " is-unread" } else { "" },
        published = escape_html(&metadata.published),
        source_url = escape_attribute_url(&metadata.source.url),
        source_name = escape_html(&metadata.source.name),
        title = escape_html(&metadata.title),
        summary = escape_html(&summary),
        tags = tags,
        favorite_value = favorite_value,
        favorite_mark = favorite_mark,
        favorite_label = favorite_label,
        comment_count = state.comments.len(),
        comment_suffix = if state.comments.len() == 1 { "" } else { "s" },
        comments = comments,
    )
}

fn plain_text_summary(markdown: &str, maximum_characters: usize) -> String {
    let mut text = String::new();
    for event in Parser::new(markdown) {
        match event {
            Event::Text(value) | Event::Code(value) => {
                if !text.is_empty() && !text.ends_with(char::is_whitespace) {
                    text.push(' ');
                }
                text.push_str(&value);
            }
            Event::SoftBreak | Event::HardBreak => text.push(' '),
            _ => {}
        }
    }
    let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut characters = normalized.chars();
    let excerpt = characters
        .by_ref()
        .take(maximum_characters)
        .collect::<String>();
    if characters.next().is_some() {
        format!("{}…", excerpt.trim_end())
    } else {
        excerpt
    }
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn escape_attribute_url(value: &str) -> String {
    let lowercase = value.trim().to_ascii_lowercase();
    if lowercase.starts_with("https://") || lowercase.starts_with("http://") {
        escape_html(value)
    } else {
        "#".to_owned()
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
            ("cache-control".to_owned(), b"no-store".to_vec()),
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

        let response = route("GET", "/", &[], temporary.path());
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
    fn data_routes_are_retryable_while_an_update_is_locked() {
        let temporary = tempfile::tempdir().unwrap();
        let _lock = plainfeed_sync_core::UpdateLock::acquire(temporary.path()).unwrap();

        let feed = route("GET", "/", &[], temporary.path());
        let mutation = route("POST", "/entries/example/read", &[], temporary.path());
        let health = route("GET", "/health", &[], temporary.path());

        assert_eq!(feed.status, 503);
        assert_eq!(mutation.status, 503);
        assert_eq!(health.status, 200);
    }

    #[test]
    fn derives_plain_text_summary_without_markup_or_link_targets() {
        let summary = plain_text_summary(
            "<script>alert(1)</script>\n\nIntro **bold** [click](javascript:alert(1))",
            280,
        );
        assert_eq!(summary, "Intro bold click");
    }

    #[test]
    fn channel_route_returns_summary_cards_for_matching_entries() {
        let data = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/data");
        let response = route("GET", "/?channel=technology", &[], &data);
        let body = String::from_utf8(response.body.into_owned()).unwrap();
        assert_eq!(response.status, 200);
        assert!(body.contains("Git synchronization is viable"));
        assert!(!body.contains("A file-backed reader running under Wasmtime"));
        assert!(body.contains("Read original"));
        assert!(!body.contains("entry-body"));
    }
}
