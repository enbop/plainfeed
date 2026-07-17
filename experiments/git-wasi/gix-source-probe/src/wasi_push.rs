use std::{collections::HashSet, error::Error, io};

use gix::{
    bstr::ByteSlice,
    protocol::{
        futures_lite::{AsyncBufReadExt, AsyncWriteExt, StreamExt},
        handshake::refs::from_v1_refs_received_as_part_of_handshake_and_capabilities,
        transport::{
            Protocol, Service,
            client::{MessageKind, WriteMode, async_io::Transport as _},
        },
    },
};

use crate::wasi_http;

pub async fn push(
    url: String,
    path: String,
    local_ref: String,
    remote_ref: String,
) -> Result<(), Box<dyn Error>> {
    validate_ref(&local_ref)?;
    validate_ref(&remote_ref)?;

    let repository = gix::open(path)?;
    if repository.object_hash() != gix::hash::Kind::Sha1 {
        return Err("the push probe supports SHA-1 repositories only".into());
    }
    let local = repository
        .find_reference(local_ref.as_str())?
        .into_fully_peeled_id()?
        .detach();

    let has_token = std::env::var_os("PLAINFEED_GITHUB_TOKEN").is_some();
    if has_token && !url.starts_with("https://") {
        return Err("refusing to send PLAINFEED_GITHUB_TOKEN over non-HTTPS transport".into());
    }
    let mut transport = wasi_http::Transport::new(url, Protocol::V1)?;
    if let Ok(token) = std::env::var("PLAINFEED_GITHUB_TOKEN") {
        if token.is_empty() {
            return Err("PLAINFEED_GITHUB_TOKEN is empty".into());
        }
        transport = transport.with_basic_auth("x-access-token".into(), token);
    }

    let (remote, report_status_v2, object_format) = {
        let handshake = transport.handshake(Service::ReceivePack, &[]).await?;
        if handshake.actual_protocol != Protocol::V1 {
            return Err(format!(
                "receive-pack returned unsupported protocol {:?}",
                handshake.actual_protocol
            )
            .into());
        }
        if !handshake.capabilities.contains("report-status")
            && !handshake.capabilities.contains("report-status-v2")
        {
            return Err("remote does not advertise report-status".into());
        }
        let report_status_v2 = handshake.capabilities.contains("report-status-v2");
        let object_format = handshake.capabilities.contains("object-format");
        let mut advertised = handshake
            .refs
            .ok_or("receive-pack did not advertise refs")?;
        let (refs, _) = from_v1_refs_received_as_part_of_handshake_and_capabilities(
            advertised.as_mut(),
            handshake.capabilities.iter(),
        )
        .await?;
        let remote = refs
            .iter()
            .find_map(|candidate| {
                let (name, target, _) = candidate.unpack();
                (name == remote_ref.as_bytes().as_bstr()).then(|| target.map(ToOwned::to_owned))
            })
            .flatten()
            .ok_or_else(|| format!("remote ref {remote_ref:?} was not advertised"))?;
        (remote, report_status_v2, object_format)
    };

    let commit = repository.find_commit(local)?;
    let parents: Vec<_> = commit.parent_ids().map(|id| id.detach()).collect();
    if parents.as_slice() != [remote] {
        return Err(format!(
            "probe requires exactly one new commit whose sole parent is remote {remote}; parents={parents:?}"
        )
        .into());
    }

    let tree = commit.tree_id()?.detach();
    let mut seen = HashSet::new();
    let mut objects = Vec::new();
    collect_object(&repository, local, &mut seen, &mut objects)?;
    collect_tree(&repository, tree, &mut seen, &mut objects)?;
    let pack = create_pack(objects)?;

    let status_capability = if report_status_v2 {
        "report-status-v2"
    } else {
        "report-status"
    };
    let mut capabilities = vec![status_capability, "agent=plainfeed-gix-wasip2-probe/0.0.0"];
    if object_format {
        capabilities.push("object-format=sha1");
    }
    let command = format!("{remote} {local} {remote_ref}\0{}", capabilities.join(" "));

    let mut request = transport.request(WriteMode::Binary, MessageKind::Flush, false)?;
    request.write_all(command.as_bytes()).await?;
    request.write_message(MessageKind::Flush).await?;
    let (mut request_body, response) = request.into_parts();
    request_body.write_all(&pack).await?;
    request_body.flush().await?;
    request_body.close().await?;
    drop(request_body);

    let mut lines = response.lines();
    let mut unpack_ok = false;
    let mut ref_ok = false;
    let expected_ok = format!("ok {remote_ref}");
    while let Some(line) = lines.next().await {
        let line = line?;
        let line = line.trim_end();
        println!("remote_status={line}");
        if line == "unpack ok" {
            unpack_ok = true;
        } else if line == expected_ok {
            ref_ok = true;
        } else if line.starts_with("unpack ") || line.starts_with("ng ") {
            return Err(format!("remote rejected push: {line}").into());
        }
    }
    if !unpack_ok || !ref_ok {
        return Err(format!(
            "incomplete receive-pack status: unpack_ok={unpack_ok}, ref_ok={ref_ok}"
        )
        .into());
    }

    println!("implementation=gix-smart-http-push");
    println!("old={remote}");
    println!("new={local}");
    println!("ref={remote_ref}");
    println!("pack_bytes={}", pack.len());
    println!("status=ok");
    Ok(())
}

fn validate_ref(name: &str) -> Result<(), Box<dyn Error>> {
    if !name.starts_with("refs/heads/") {
        return Err(format!("probe only supports branch refs, got {name:?}").into());
    }
    gix::refs::FullName::try_from(name)?;
    Ok(())
}

fn collect_tree(
    repository: &gix::Repository,
    id: gix::hash::ObjectId,
    seen: &mut HashSet<gix::hash::ObjectId>,
    objects: &mut Vec<gix::ObjectDetached>,
) -> Result<(), Box<dyn Error>> {
    if !seen.insert(id) {
        return Ok(());
    }
    let object = repository.find_object(id)?;
    if object.kind != gix::objs::Kind::Tree {
        return Err(format!("expected tree {id}, found {:?}", object.kind).into());
    }
    let children = gix::objs::TreeRefIter::from_bytes(&object.data, id.kind())
        .map(|entry| entry.map(|entry| (entry.mode, entry.oid.to_owned())))
        .collect::<Result<Vec<_>, _>>()?;
    objects.push(object.detach());
    for (mode, child_id) in children {
        if mode.kind() == gix::objs::tree::EntryKind::Commit {
            continue;
        }
        if mode.is_tree() {
            collect_tree(repository, child_id, seen, objects)?;
        } else {
            collect_object(repository, child_id, seen, objects)?;
        }
    }
    Ok(())
}

fn collect_object(
    repository: &gix::Repository,
    id: gix::hash::ObjectId,
    seen: &mut HashSet<gix::hash::ObjectId>,
    objects: &mut Vec<gix::ObjectDetached>,
) -> Result<(), Box<dyn Error>> {
    if seen.insert(id) {
        objects.push(repository.find_object(id)?.detach());
    }
    Ok(())
}

fn create_pack(objects: Vec<gix::ObjectDetached>) -> Result<Vec<u8>, Box<dyn Error>> {
    let entries = objects
        .iter()
        .map(|object| {
            let count = gix_pack::data::output::Count::from_data(object.id, None);
            let data = gix::objs::Data::new(&object.data, object.kind, object.id.kind());
            gix_pack::data::output::Entry::from_data(
                &count,
                &data,
                gix_pack::data::output::entry::iter_from_counts::Options::default().compression,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    let count = u32::try_from(entries.len())?;
    let mut output = Vec::new();
    let input = std::iter::once(Ok::<_, io::Error>(entries));
    let mut writer = gix_pack::data::output::bytes::FromEntriesIter::new(
        input,
        &mut output,
        count,
        gix_pack::data::Version::V2,
        gix::hash::Kind::Sha1,
    );
    for result in &mut writer {
        result?;
    }
    Ok(output)
}
