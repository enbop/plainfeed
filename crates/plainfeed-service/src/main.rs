mod settings;

use std::env;
use std::error::Error;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{DefaultBodyLimit, Form, State};
use axum::http::{Method, StatusCode, Uri, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{any, get};
use plainfeed_git::{Credentials, Remote};
use plainfeed_server::{Reader, SettingsNotice, SettingsView};
use plainfeed_sync::{
    PublishOutcome, SyncCommand, publish_state, run_pull_cycle, state_publication_is_due,
};
use plainfeed_sync_core::{DirtyJournal, SyncState};
use serde::Deserialize;
use settings::ServiceSettings;
use time::OffsetDateTime;
use tokio::sync::mpsc;

const DEFAULT_ADDRESS: &str = "127.0.0.1:18437";
const MAX_REQUEST_BODY: usize = 64 * 1024;
const DEFAULT_SYNC_TICK: Duration = Duration::from_secs(30);

#[derive(Clone)]
struct AppState {
    data_root: Arc<PathBuf>,
    sync_commands: mpsc::UnboundedSender<SyncCommand>,
    reader: Reader,
}

#[derive(Deserialize)]
struct SettingsForm {
    remote_url: String,
    #[serde(default)]
    github_token: String,
    #[serde(default)]
    clear_token: Option<String>,
}

fn main() -> Result<(), Box<dyn Error>> {
    rustls_rustcrypto::provider()
        .install_default()
        .map_err(|_| "failed to install the RustCrypto TLS provider")?;

    let mut arguments = env::args().skip(1);
    let address = arguments
        .next()
        .or_else(|| env::var("PLAINFEED_ADDR").ok())
        .unwrap_or_else(|| DEFAULT_ADDRESS.to_owned());
    let data_root = PathBuf::from(arguments.next().unwrap_or_else(|| "/data".to_owned()));
    if arguments.next().is_some() {
        return Err("usage: plainfeed-service [ADDRESS] [DATA_ROOT]".into());
    }

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let local = tokio::task::LocalSet::new();
    local.block_on(&runtime, run(address, data_root))?;
    Ok(())
}

async fn run(address: String, data_root: PathBuf) -> Result<(), Box<dyn Error>> {
    let (sync_commands, sync_receiver) = mpsc::unbounded_channel();
    let state = AppState {
        data_root: Arc::new(data_root),
        sync_commands,
        reader: Reader::builder().build(),
    };
    let sync_root = Arc::clone(&state.data_root);
    let sync_tick = sync_tick_duration();
    tokio::task::spawn_local(async move {
        synchronization_loop(sync_root, sync_tick, sync_receiver).await;
    });

    let application = Router::new()
        .route("/settings", get(settings_page).post(save_settings))
        .fallback(any(reader_request))
        .layer(DefaultBodyLimit::max(MAX_REQUEST_BODY))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind(&address).await?;
    println!("Plainfeed listening on http://{address}/");
    axum::serve(listener, application).await?;
    Ok(())
}

async fn reader_request(
    State(state): State<AppState>,
    method: Method,
    uri: Uri,
    body: Bytes,
) -> Response {
    if method == Method::GET && uri.path() == "/" && !configuration_available(&state.data_root) {
        return Redirect::temporary("/settings").into_response();
    }
    let suppress_body = method == Method::HEAD;
    let route_method = if suppress_body {
        "GET"
    } else {
        method.as_str()
    };
    let response = state.reader.handle_service_request(
        route_method,
        &uri.to_string(),
        &body,
        state.data_root.as_path(),
    );
    let status = StatusCode::from_u16(response.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let response_body = if suppress_body {
        Body::empty()
    } else {
        Body::from(response.body.into_owned())
    };
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, response.content_type)
        .header(header::CACHE_CONTROL, response.cache_control)
        .body(response_body)
        .unwrap_or_else(|_| Response::new(Body::from("invalid response\n")))
}

fn configuration_available(data_root: &Path) -> bool {
    env::var("PLAINFEED_REMOTE_URL").is_ok()
        || ServiceSettings::read_from(data_root)
            .ok()
            .flatten()
            .is_some_and(|settings| !settings.remote_url.is_empty())
        || SyncState::read_from(data_root)
            .ok()
            .flatten()
            .and_then(|state| state.remote_url)
            .is_some()
}

async fn settings_page(State(state): State<AppState>, uri: Uri) -> Response {
    let saved = uri
        .query()
        .is_some_and(|query| query.split('&').any(|field| field == "saved=1"));
    settings_response(&state.reader, &state.data_root, saved, None, StatusCode::OK)
}

async fn save_settings(State(state): State<AppState>, Form(form): Form<SettingsForm>) -> Response {
    let remote_url = form.remote_url.trim().to_owned();
    if remote_url.is_empty() {
        return settings_response(
            &state.reader,
            &state.data_root,
            false,
            Some("Remote URL is required."),
            StatusCode::BAD_REQUEST,
        );
    }
    if Remote::new(remote_url.clone(), None).is_err() {
        return settings_response(
            &state.reader,
            &state.data_root,
            false,
            Some("Remote URL must be a supported HTTP or HTTPS Git URL."),
            StatusCode::BAD_REQUEST,
        );
    }

    let existing_token = ServiceSettings::read_from(&state.data_root)
        .ok()
        .flatten()
        .and_then(|settings| settings.github_token);
    let submitted_token = form.github_token.trim();
    let github_token = if form.clear_token.is_some() {
        None
    } else if submitted_token.is_empty() {
        existing_token
    } else {
        Some(submitted_token.to_owned())
    };
    let settings = ServiceSettings::new(remote_url, github_token);
    if settings.write_to(&state.data_root).is_err() {
        return settings_response(
            &state.reader,
            &state.data_root,
            false,
            Some("Settings could not be written to the data directory."),
            StatusCode::INTERNAL_SERVER_ERROR,
        );
    }
    let _ = state.sync_commands.send(SyncCommand::Force);
    Redirect::to("/settings?saved=1").into_response()
}

fn settings_response(
    reader: &Reader,
    data_root: &Path,
    saved: bool,
    error: Option<&str>,
    status: StatusCode,
) -> Response {
    let stored = ServiceSettings::read_from(data_root).ok().flatten();
    let remote_from_environment = env::var("PLAINFEED_REMOTE_URL").ok();
    let remote_url = remote_from_environment
        .clone()
        .or_else(|| stored.as_ref().map(|settings| settings.remote_url.clone()))
        .or_else(|| {
            SyncState::read_from(data_root)
                .ok()
                .flatten()
                .and_then(|state| state.remote_url)
        })
        .unwrap_or_default();
    let token_from_environment =
        env::var("PLAINFEED_GITHUB_TOKEN").is_ok() || env::var("PLAINFEED_GIT_PASSWORD").is_ok();
    let stored_token = stored.as_ref().is_some_and(ServiceSettings::has_token);
    let notice = if saved {
        Some(SettingsNotice {
            message: "Settings saved. Synchronization was requested immediately.".to_owned(),
            error: false,
        })
    } else if let Some(error) = error {
        Some(SettingsNotice {
            message: error.to_owned(),
            error: true,
        })
    } else {
        None
    };
    let token_status = if token_from_environment {
        "Provided by the process environment"
    } else if stored_token {
        "Stored locally"
    } else {
        "Not configured"
    };
    let document = match reader.render_settings(&SettingsView {
        remote_url,
        token_status: token_status.to_owned(),
        notice,
        environment_override: remote_from_environment.is_some() || token_from_environment,
    }) {
        Ok(document) => document,
        Err(error) => {
            eprintln!("plainfeed renderer: {error}");
            return Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
                .body(Body::from("plainfeed could not render settings\n"))
                .unwrap_or_else(|_| Response::new(Body::from("invalid response\n")));
        }
    };
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .header(header::CACHE_CONTROL, "no-store")
        .body(Body::from(document))
        .unwrap_or_else(|_| Response::new(Body::from("invalid response\n")))
}

async fn synchronization_loop(
    data_root: Arc<PathBuf>,
    tick: Duration,
    mut commands: mpsc::UnboundedReceiver<SyncCommand>,
) {
    run_synchronization(SyncCommand::Force, &data_root).await;
    let mut interval = tokio::time::interval(tick);
    interval.tick().await;
    loop {
        let command = tokio::select! {
            _ = interval.tick() => SyncCommand::Tick,
            command = commands.recv() => match command {
                Some(command) => command,
                None => break,
            },
        };
        run_synchronization(command, &data_root).await;
    }
}

fn sync_tick_duration() -> Duration {
    env::var("PLAINFEED_SYNC_TICK_SECONDS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|seconds| *seconds > 0)
        .map(Duration::from_secs)
        .unwrap_or(DEFAULT_SYNC_TICK)
}

async fn run_synchronization(command: SyncCommand, data_root: &Path) {
    let remote = match remote_from_configuration(data_root) {
        Ok(Some(remote)) => remote,
        Ok(None) => return,
        Err(error) => {
            persist_error(data_root, &error);
            eprintln!("plainfeed sync delayed: {error}");
            return;
        }
    };
    let now = OffsetDateTime::now_utc();
    let markers = match DirtyJournal::new(data_root).snapshot() {
        Ok(markers) => markers,
        Err(error) => {
            let error = error.to_string();
            persist_error(data_root, &error);
            eprintln!("plainfeed sync delayed: {error}");
            return;
        }
    };
    let result = if state_publication_is_due(command, &markers, now) {
        publish_state(data_root, remote, now)
            .await
            .map(|outcome| match outcome {
                PublishOutcome::NoDirtyState => "no dirty state",
                PublishOutcome::AlreadyPublished => "state already published",
                PublishOutcome::Pushed(_) => "state published",
            })
            .map_err(|error| error.to_string())
    } else {
        run_pull_cycle(command, data_root, remote, now)
            .await
            .map(|ran| if ran { "content pulled" } else { "not due" })
            .map_err(|error| error.to_string())
    };
    match result {
        Ok("not due") => {}
        Ok(outcome) => println!("plainfeed sync: {outcome}"),
        Err(error) => {
            persist_error(data_root, &error);
            eprintln!("plainfeed sync delayed: {error}");
        }
    }
}

fn remote_from_configuration(data_root: &Path) -> Result<Option<Remote>, String> {
    let state = SyncState::read_from(data_root).map_err(|error| error.to_string())?;
    let settings = ServiceSettings::read_from(data_root)
        .map_err(|_| "local service settings could not be read".to_owned())?;
    let Some(remote_url) = env::var("PLAINFEED_REMOTE_URL")
        .ok()
        .or_else(|| {
            settings
                .as_ref()
                .map(|settings| settings.remote_url.clone())
        })
        .or_else(|| state.and_then(|state| state.remote_url))
    else {
        return Ok(None);
    };
    Remote::new(
        remote_url,
        credentials_from_configuration(settings.as_ref()),
    )
    .map(Some)
    .map_err(|error| error.to_string())
}

fn credentials_from_configuration(settings: Option<&ServiceSettings>) -> Option<Credentials> {
    if let Ok(password) = env::var("PLAINFEED_GIT_PASSWORD") {
        let username = env::var("PLAINFEED_GIT_USERNAME").unwrap_or_else(|_| "git".to_owned());
        return Some(Credentials::basic(username, password));
    }
    env::var("PLAINFEED_GITHUB_TOKEN")
        .ok()
        .or_else(|| settings.and_then(|settings| settings.github_token.clone()))
        .map(|token| Credentials::basic("x-access-token", token))
}

fn persist_error(data_root: &Path, error: &str) {
    let mut state = match SyncState::read_from(data_root) {
        Ok(Some(state)) => state,
        Ok(None) => SyncState::new("origin", "refs/heads/main"),
        Err(_) => return,
    };
    state.last_error = Some(error.chars().take(4096).collect());
    let _ = state.write_to(data_root);
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;

    #[test]
    fn persisted_settings_count_as_initialized() {
        let temporary = tempfile::tempdir().unwrap();
        ServiceSettings::new("https://example.com/feed.git".to_owned(), None)
            .write_to(temporary.path())
            .unwrap();

        assert!(configuration_available(temporary.path()));
    }

    #[tokio::test]
    async fn settings_page_never_discloses_the_saved_token() {
        let temporary = tempfile::tempdir().unwrap();
        ServiceSettings::new(
            "https://github.com/example/plainfeed-data.git".to_owned(),
            Some("plainfeed-test-secret".to_owned()),
        )
        .write_to(temporary.path())
        .unwrap();

        let response = settings_response(
            &Reader::default(),
            temporary.path(),
            false,
            None,
            StatusCode::OK,
        );
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();
        assert!(!body.contains("plainfeed-test-secret"));
        assert!(body.contains("Stored locally"));
        assert!(body.contains("Leave blank to keep the current token"));
    }

    #[tokio::test]
    async fn settings_page_escapes_rendered_errors() {
        let temporary = tempfile::tempdir().unwrap();
        let response = settings_response(
            &Reader::default(),
            temporary.path(),
            false,
            Some("invalid <script>alert(1)</script>"),
            StatusCode::BAD_REQUEST,
        );
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();

        assert!(body.contains("invalid &lt;script&gt;alert(1)&lt;/script&gt;"));
        assert!(!body.contains("<script>alert(1)</script>"));
    }

    #[test]
    fn saved_github_token_builds_redacted_credentials() {
        let settings = ServiceSettings::new(
            "https://github.com/example/plainfeed-data.git".to_owned(),
            Some("plainfeed-test-secret".to_owned()),
        );
        let credentials = credentials_from_configuration(Some(&settings)).unwrap();
        let debug = format!("{credentials:?}");

        assert!(!debug.contains("plainfeed-test-secret"));
    }
}
