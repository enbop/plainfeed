use std::{env, error::Error};

#[cfg(feature = "https-async-reqwest-rustls")]
mod wasi_http;
#[cfg(feature = "smart-http-push")]
mod wasi_push;

fn main() -> Result<(), Box<dyn Error>> {
    #[cfg(feature = "https-async-reqwest-rustls")]
    rustls_rustcrypto::provider()
        .install_default()
        .map_err(|_| "failed to install the RustCrypto TLS provider")?;

    let mut args = env::args().skip(1);
    let operation = args.next().unwrap_or_else(|| "inspect".into());

    if operation == "https-get" {
        let url = args.next().ok_or("https-get requires a URL")?;
        return https_get(url);
    }

    if operation == "async-fetch" {
        let url = args.next().ok_or("async-fetch requires a URL")?;
        let path = args.next().ok_or("async-fetch requires a destination")?;
        return async_fetch_repository(url, path);
    }

    if operation == "create-push-commit" {
        let path = args
            .next()
            .ok_or("create-push-commit requires a repository")?;
        let base_ref = args
            .next()
            .ok_or("create-push-commit requires a base ref")?;
        let local_ref = args
            .next()
            .ok_or("create-push-commit requires a local ref")?;
        let file = args
            .next()
            .ok_or("create-push-commit requires a file path")?;
        let content = args
            .next()
            .ok_or("create-push-commit requires file content")?;
        return create_push_commit(path, base_ref, local_ref, file, content);
    }

    if operation == "async-push" {
        let url = args.next().ok_or("async-push requires a URL")?;
        let path = args.next().ok_or("async-push requires a repository")?;
        let local_ref = args.next().ok_or("async-push requires a local ref")?;
        let remote_ref = args.next().ok_or("async-push requires a remote ref")?;
        return async_push_repository(url, path, local_ref, remote_ref);
    }

    let path = args.next().unwrap_or_else(|| ".".into());

    if operation == "init-commit" {
        return init_and_commit(path);
    }

    let repository = gix::open(path)?;

    println!("implementation=gix-source");
    println!("git_dir={}", repository.git_dir().display());
    Ok(())
}

fn init_and_commit(path: String) -> Result<(), Box<dyn Error>> {
    use gix::{bstr::ByteSlice, objs::tree::EntryKind};

    let repository = gix::init(path)?;
    let blob = repository.write_blob(b"written by gix inside Wasmtime\n")?;
    let mut editor = repository.edit_tree(repository.empty_tree().id)?;
    let tree = editor.upsert("probe.txt", EntryKind::Blob, blob)?.write()?;
    let identity = gix::actor::SignatureRef {
        name: b"Plainfeed WASI Probe".as_bstr(),
        email: b"probe@plainfeed.invalid".as_bstr(),
        time: "1784160000 +0900",
    };
    let commit = repository.commit_as(
        identity,
        identity,
        "HEAD",
        "initial commit from Wasmtime",
        tree,
        gix::commit::NO_PARENT_IDS,
    )?;

    println!("implementation=gix-source");
    println!("commit={commit}");
    println!("tree={tree}");
    Ok(())
}

#[cfg(feature = "smart-http-push")]
fn create_push_commit(
    path: String,
    base_ref: String,
    local_ref: String,
    file: String,
    content: String,
) -> Result<(), Box<dyn Error>> {
    use gix::{bstr::ByteSlice, objs::tree::EntryKind};

    let repository = gix::open(path)?;
    let base = repository
        .find_reference(base_ref.as_str())?
        .into_fully_peeled_id()?;
    let base_commit = repository.find_commit(base)?;
    let mut editor = repository.edit_tree(base_commit.tree_id()?)?;
    let blob = repository.write_blob(content.as_bytes())?;
    let tree = editor.upsert(file, EntryKind::Blob, blob)?.write()?;
    let identity = gix::actor::SignatureRef {
        name: b"Plainfeed WASI Push Probe".as_bstr(),
        email: b"push-probe@plainfeed.invalid".as_bstr(),
        time: "1784160000 +0900",
    };
    let commit = repository.commit_as(
        identity,
        identity,
        local_ref,
        "commit created for the WASI push probe",
        tree,
        [base.detach()],
    )?;

    println!("implementation=gix-smart-http-push");
    println!("base={base}");
    println!("commit={commit}");
    println!("tree={tree}");
    Ok(())
}

#[cfg(not(feature = "smart-http-push"))]
fn create_push_commit(
    _path: String,
    _base_ref: String,
    _local_ref: String,
    _file: String,
    _content: String,
) -> Result<(), Box<dyn Error>> {
    Err("create-push-commit requires the smart-http-push feature".into())
}

#[cfg(feature = "https-async-reqwest-rustls")]
fn https_get(url: String) -> Result<(), Box<dyn Error>> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async move {
        let response = wasi_http::client()?
            .get(url)
            .send()
            .await?
            .error_for_status()?;
        println!("status={}", response.status());
        println!(
            "content_type={:?}",
            response.headers().get(reqwest::header::CONTENT_TYPE)
        );
        println!("body_bytes={}", response.bytes().await?.len());
        Ok(())
    })
}

#[cfg(not(feature = "https-async-reqwest-rustls"))]
fn https_get(_url: String) -> Result<(), Box<dyn Error>> {
    Err("https-get requires the https-async-reqwest-rustls feature".into())
}

#[cfg(feature = "https-async-reqwest-rustls")]
fn async_fetch_repository(url: String, path: String) -> Result<(), Box<dyn Error>> {
    use std::sync::atomic::AtomicBool;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async move {
        let mut repository = match gix::open(&path) {
            Ok(repository) => repository,
            Err(_) => gix::init(&path)?,
        };
        repository.committer_or_set_generic_fallback()?;
        let remote = repository.remote_at(url.as_str())?.with_refspecs(
            Some("+refs/heads/*:refs/remotes/origin/*"),
            gix::remote::Direction::Fetch,
        )?;
        let transport = wasi_http::Transport::new(url, gix::protocol::transport::Protocol::V2)?;
        let connection = remote.to_connection_with_transport(transport);
        let prepared = connection
            .prepare_fetch(gix::progress::Discard, Default::default())
            .await?;
        let outcome = prepared
            .receive(gix::progress::Discard, &AtomicBool::new(false))
            .await?;
        let reopened = gix::open(repository.git_dir())?;

        println!("implementation=gix-async-reqwest");
        println!("git_dir={}", repository.git_dir().display());
        println!("remote_refs={}", outcome.ref_map.remote_refs.len());
        for remote_ref in &outcome.ref_map.remote_refs {
            let id = match remote_ref {
                gix::protocol::handshake::Ref::Peeled { tag, .. } => Some(tag),
                gix::protocol::handshake::Ref::Direct { object, .. }
                | gix::protocol::handshake::Ref::Symbolic { object, .. } => Some(object),
                gix::protocol::handshake::Ref::Unborn { .. } => None,
            };
            if let Some(id) = id {
                println!("object_after_reopen[{id}]={}", reopened.has_object(id));
            }
        }
        println!("status={:?}", outcome.status);
        Ok(())
    })
}

#[cfg(not(feature = "https-async-reqwest-rustls"))]
fn async_fetch_repository(_url: String, _path: String) -> Result<(), Box<dyn Error>> {
    Err("async-fetch requires the https-async-reqwest-rustls feature".into())
}

#[cfg(feature = "smart-http-push")]
fn async_push_repository(
    url: String,
    path: String,
    local_ref: String,
    remote_ref: String,
) -> Result<(), Box<dyn Error>> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(wasi_push::push(url, path, local_ref, remote_ref))
}

#[cfg(not(feature = "smart-http-push"))]
fn async_push_repository(
    _url: String,
    _path: String,
    _local_ref: String,
    _remote_ref: String,
) -> Result<(), Box<dyn Error>> {
    Err("async-push requires the smart-http-push feature".into())
}
