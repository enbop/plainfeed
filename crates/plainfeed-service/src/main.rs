use std::env;
use std::error::Error;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{Method, StatusCode, Uri, header};
use axum::response::Response;
use axum::routing::any;
use plainfeed_git::{Credentials, Remote};
use plainfeed_sync::{
    PublishOutcome, SyncCommand, publish_state, run_pull_cycle, state_publication_is_due,
};
use plainfeed_sync_core::{DirtyJournal, SyncState};
use time::OffsetDateTime;

const DEFAULT_ADDRESS: &str = "127.0.0.1:8080";
const MAX_REQUEST_BODY: usize = 64 * 1024;
const DEFAULT_SYNC_TICK: Duration = Duration::from_secs(30);

#[derive(Clone)]
struct AppState {
    data_root: Arc<PathBuf>,
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
    let state = AppState {
        data_root: Arc::new(data_root),
    };
    let sync_root = Arc::clone(&state.data_root);
    let sync_tick = sync_tick_duration();
    tokio::task::spawn_local(async move {
        synchronization_loop(sync_root, sync_tick).await;
    });

    let application = Router::new()
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
    let suppress_body = method == Method::HEAD;
    let route_method = if suppress_body {
        "GET"
    } else {
        method.as_str()
    };
    let response = plainfeed_server::handle_request(
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
        .header(header::CACHE_CONTROL, "no-store")
        .body(response_body)
        .unwrap_or_else(|_| Response::new(Body::from("invalid response\n")))
}

async fn synchronization_loop(data_root: Arc<PathBuf>, tick: Duration) {
    run_synchronization(SyncCommand::Force, &data_root).await;
    let mut interval = tokio::time::interval(tick);
    interval.tick().await;
    loop {
        interval.tick().await;
        run_synchronization(SyncCommand::Tick, &data_root).await;
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
    let Some(remote_url) = env::var("PLAINFEED_REMOTE_URL")
        .ok()
        .or_else(|| state.and_then(|state| state.remote_url))
    else {
        return Ok(None);
    };
    Remote::new(remote_url, credentials_from_environment())
        .map(Some)
        .map_err(|error| error.to_string())
}

fn credentials_from_environment() -> Option<Credentials> {
    if let Ok(password) = env::var("PLAINFEED_GIT_PASSWORD") {
        let username = env::var("PLAINFEED_GIT_USERNAME").unwrap_or_else(|_| "git".to_owned());
        return Some(Credentials::basic(username, password));
    }
    env::var("PLAINFEED_GITHUB_TOKEN")
        .ok()
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
