#![cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]

use plainfeed_core::{Entry, Error as StoreError, Store};
use pulldown_cmark::{CowStr, Event, Options, Parser, Tag, html};
use std::borrow::Cow;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

const APP_JS: &str = include_str!("../../../web/app.js");
const STYLE_CSS: &str = include_str!("../../../web/style.css");
const HTMX_JS: &[u8] = include_bytes!("../../../web/vendor/htmx.min.js");

#[derive(Debug)]
struct Response {
    status: u16,
    content_type: &'static str,
    body: Cow<'static, [u8]>,
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
    match (method, path) {
        ("GET", "/") => render_feed(data_root),
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

fn render_feed(data_root: &Path) -> Response {
    match Store::open(data_root).entries() {
        Ok(entries) => {
            let unread = entries
                .iter()
                .filter(|entry| entry.state.read_at.is_none())
                .count();
            let cards = if entries.is_empty() {
                "<section class=\"empty\"><h2>Your feed is empty</h2><p>Add a v1 Markdown entry under <code>content/</code>.</p></section>".to_owned()
            } else {
                entries.iter().map(render_entry).collect::<String>()
            };
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
    <p><strong>{}</strong> unread · <strong>{}</strong> total</p>
  </header>
  <main id="feed" class="feed">{}</main>
</body>
</html>"#,
                unread,
                entries.len(),
                cards
            );
            Response::text(200, "text/html; charset=utf-8", document)
        }
        Err(error) => server_error(error),
    }
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
        .map(|summary| format!("<p class=\"summary\">{}</p>", escape_html(summary)))
        .unwrap_or_default();
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
    <div class="entry-meta"><span class="unread-dot" aria-label="Unread"></span><time datetime="{published}">{published}</time><span>·</span><a href="{source_url}" rel="noreferrer">{source_name}</a></div>
    <h2>{title}</h2>
    {summary}
    <div class="tags">{tags}</div>
  </header>
  <div class="entry-body">{body}</div>
  <footer class="entry-actions">
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
        summary = summary,
        tags = tags,
        body = render_markdown(&entry.body),
        favorite_value = favorite_value,
        favorite_mark = favorite_mark,
        favorite_label = favorite_label,
        comment_count = state.comments.len(),
        comment_suffix = if state.comments.len() == 1 { "" } else { "s" },
        comments = comments,
    )
}

fn render_markdown(markdown: &str) -> String {
    let parser = Parser::new_ext(markdown, Options::ENABLE_STRIKETHROUGH).map(sanitize_event);
    let mut rendered = String::new();
    html::push_html(&mut rendered, parser);
    rendered
}

fn sanitize_event(event: Event<'_>) -> Event<'_> {
    match event {
        Event::Html(value) | Event::InlineHtml(value) => Event::Text(value),
        Event::Start(Tag::Link {
            link_type,
            dest_url,
            title,
            id,
        }) => Event::Start(Tag::Link {
            link_type,
            dest_url: safe_markdown_url(dest_url),
            title,
            id,
        }),
        Event::Start(Tag::Image {
            link_type,
            dest_url,
            title,
            id,
        }) => Event::Start(Tag::Image {
            link_type,
            dest_url: safe_markdown_url(dest_url),
            title,
            id,
        }),
        event => event,
    }
}

fn safe_markdown_url(url: CowStr<'_>) -> CowStr<'_> {
    let lowercase = url.trim().to_ascii_lowercase();
    if lowercase.starts_with("https://")
        || lowercase.starts_with("http://")
        || lowercase.starts_with("mailto:")
        || lowercase.starts_with('/')
        || lowercase.starts_with('#')
    {
        url
    } else {
        CowStr::Borrowed("#")
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

#[cfg(target_arch = "wasm32")]
mod wasi_http {
    use super::{Response, route};
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
                route(route_method, &path, &body, &data_root),
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

#[cfg(target_arch = "wasm32")]
use wasi_http::Handler;

#[cfg(target_arch = "wasm32")]
wasip2::http::proxy::export!(Handler);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn form_decoding_handles_utf8() {
        assert_eq!(
            parse_form(b"comment=hello+%E4%B8%96%E7%95%8C"),
            [("comment".to_owned(), "hello 世界".to_owned())]
        );
    }

    #[test]
    fn markdown_does_not_emit_raw_html_or_script_urls() {
        let html = render_markdown("<script>alert(1)</script> [click](javascript:alert(1))");
        assert!(html.contains("&lt;script&gt;"));
        assert!(!html.contains("href=\"javascript:"));
    }
}
