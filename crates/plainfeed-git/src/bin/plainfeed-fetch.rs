use std::{env, error::Error};

use plainfeed_git::{Credentials, FetchLimits, FetchRequest, Remote, fetch};

fn main() -> Result<(), Box<dyn Error>> {
    rustls_rustcrypto::provider()
        .install_default()
        .map_err(|_| "failed to install the RustCrypto TLS provider")?;

    let mut arguments = env::args().skip(1);
    let url = arguments
        .next()
        .ok_or("usage: plainfeed-fetch REMOTE_URL REPOSITORY")?;
    let repository = arguments
        .next()
        .ok_or("usage: plainfeed-fetch REMOTE_URL REPOSITORY")?;
    if arguments.next().is_some() {
        return Err("usage: plainfeed-fetch REMOTE_URL REPOSITORY".into());
    }

    let credentials = env::var("PLAINFEED_GITHUB_TOKEN")
        .ok()
        .map(|token| Credentials::basic("x-access-token", token));
    let remote = Remote::new(url, credentials)?;
    let mut request = FetchRequest::main(repository, remote);
    request.limits = FetchLimits {
        max_response_bytes: usize_from_env(
            "PLAINFEED_MAX_RESPONSE_BYTES",
            request.limits.max_response_bytes,
        )?,
        max_repository_bytes: u64_from_env(
            "PLAINFEED_MAX_REPOSITORY_BYTES",
            request.limits.max_repository_bytes,
        )?,
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let outcome = runtime.block_on(fetch(request))?;

    println!("remote_tip={}", outcome.remote_tip);
    println!("state_tree={}", outcome.state_tree.as_deref().unwrap_or(""));
    println!("remote_refs={}", outcome.remote_refs);
    println!("repository_bytes={}", outcome.repository_bytes);
    Ok(())
}

fn usize_from_env(name: &str, default: usize) -> Result<usize, Box<dyn Error>> {
    match env::var(name) {
        Ok(value) => value
            .parse()
            .map_err(|_| format!("{name} must be a positive integer").into()),
        Err(env::VarError::NotPresent) => Ok(default),
        Err(error) => Err(error.into()),
    }
}

fn u64_from_env(name: &str, default: u64) -> Result<u64, Box<dyn Error>> {
    match env::var(name) {
        Ok(value) => value
            .parse()
            .map_err(|_| format!("{name} must be a positive integer").into()),
        Err(env::VarError::NotPresent) => Ok(default),
        Err(error) => Err(error.into()),
    }
}
