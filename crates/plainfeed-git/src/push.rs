use std::{collections::HashSet, io};

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

use crate::{Error, FetchLimits, Remote, http::Transport};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PushOutcome {
    pub previous_remote: String,
    pub pushed_commit: String,
    pub remote_ref: String,
    pub pack_bytes: usize,
}

pub async fn push_one_commit(
    remote: Remote,
    repository_path: impl AsRef<std::path::Path>,
    local_ref: &str,
    remote_ref: &str,
    limits: FetchLimits,
) -> Result<PushOutcome, Error> {
    validate_local_ref(local_ref)?;
    validate_remote_ref(remote_ref)?;
    let repository = gix::open(repository_path.as_ref()).map_err(git_error)?;
    if repository.object_hash() != gix::hash::Kind::Sha1 {
        return Err(Error::Git("only SHA-1 pushes are supported".to_owned()));
    }
    limits.check_repository(repository.git_dir())?;
    let local = repository
        .find_reference(local_ref)
        .map_err(git_error)?
        .into_fully_peeled_id()
        .map_err(git_error)?
        .detach();
    let mut transport = Transport::for_push(remote, limits)?;

    let (advertised_remote, report_status_v2, object_format) = {
        let handshake = transport
            .handshake(Service::ReceivePack, &[])
            .await
            .map_err(git_error)?;
        if handshake.actual_protocol != Protocol::V1 {
            return Err(Error::Git(format!(
                "receive-pack returned unsupported protocol {:?}",
                handshake.actual_protocol
            )));
        }
        if !handshake.capabilities.contains("report-status")
            && !handshake.capabilities.contains("report-status-v2")
        {
            return Err(Error::Git(
                "remote does not advertise receive-pack status".to_owned(),
            ));
        }
        let report_status_v2 = handshake.capabilities.contains("report-status-v2");
        let object_format = handshake.capabilities.contains("object-format");
        let mut advertised = handshake
            .refs
            .ok_or_else(|| Error::Git("receive-pack did not advertise refs".to_owned()))?;
        let (refs, _) = from_v1_refs_received_as_part_of_handshake_and_capabilities(
            advertised.as_mut(),
            handshake.capabilities.iter(),
        )
        .await
        .map_err(git_error)?;
        let advertised_remote = refs
            .iter()
            .find_map(|candidate| {
                let (name, target, _) = candidate.unpack();
                (name == remote_ref.as_bytes().as_bstr()).then(|| target.map(ToOwned::to_owned))
            })
            .flatten()
            .ok_or_else(|| Error::Git(format!("remote ref {remote_ref:?} was not advertised")))?;
        (advertised_remote, report_status_v2, object_format)
    };

    let commit = repository.find_commit(local).map_err(git_error)?;
    let parents: Vec<_> = commit.parent_ids().map(|id| id.detach()).collect();
    if parents.as_slice() != [advertised_remote] {
        return Err(Error::StaleRemote {
            parent: parents
                .first()
                .map(ToString::to_string)
                .unwrap_or_else(|| "missing".to_owned()),
            remote: advertised_remote.to_string(),
        });
    }

    let tree = commit.tree_id().map_err(git_error)?.detach();
    let mut seen = HashSet::new();
    let mut objects = Vec::new();
    collect_object(&repository, local, &mut seen, &mut objects)?;
    collect_tree(&repository, tree, &mut seen, &mut objects)?;
    limits.check_object_count(objects.len())?;
    for object in &objects {
        limits.check_object_size(object.data.len())?;
    }
    let pack = create_pack(objects)?;
    if pack.len() > limits.max_response_bytes {
        return Err(Error::PushTooLarge {
            bytes: pack.len(),
            limit: limits.max_response_bytes,
        });
    }

    let status_capability = if report_status_v2 {
        "report-status-v2"
    } else {
        "report-status"
    };
    let mut capabilities = vec![status_capability, "agent=plainfeed/0.1"];
    if object_format {
        capabilities.push("object-format=sha1");
    }
    let command = format!(
        "{advertised_remote} {local} {remote_ref}\0{}",
        capabilities.join(" ")
    );
    let mut request = transport
        .request(WriteMode::Binary, MessageKind::Flush, false)
        .map_err(git_error)?;
    request
        .write_all(command.as_bytes())
        .await
        .map_err(git_error)?;
    request
        .write_message(MessageKind::Flush)
        .await
        .map_err(git_error)?;
    let (mut request_body, response) = request.into_parts();
    request_body.write_all(&pack).await.map_err(git_error)?;
    request_body.flush().await.map_err(git_error)?;
    request_body.close().await.map_err(git_error)?;
    drop(request_body);

    let mut lines = response.lines();
    let mut unpack_ok = false;
    let mut ref_ok = false;
    let expected_ok = format!("ok {remote_ref}");
    while let Some(line) = lines.next().await {
        let line = line.map_err(git_error)?;
        let line = line.trim_end();
        if line == "unpack ok" {
            unpack_ok = true;
        } else if line == expected_ok {
            ref_ok = true;
        } else if line.starts_with("ng ") {
            return Err(Error::StaleRemote {
                parent: advertised_remote.to_string(),
                remote: "advanced after receive-pack advertisement".to_owned(),
            });
        } else if line.starts_with("unpack ") {
            return Err(Error::Git(format!("remote rejected push: {line}")));
        }
    }
    if !unpack_ok || !ref_ok {
        return Err(Error::Git(format!(
            "incomplete receive-pack status: unpack_ok={unpack_ok}, ref_ok={ref_ok}"
        )));
    }

    Ok(PushOutcome {
        previous_remote: advertised_remote.to_string(),
        pushed_commit: local.to_string(),
        remote_ref: remote_ref.to_owned(),
        pack_bytes: pack.len(),
    })
}

fn validate_local_ref(name: &str) -> Result<(), Error> {
    if !name.starts_with("refs/plainfeed/") {
        return Err(Error::Git(format!("unsupported local push ref {name:?}")));
    }
    gix::refs::FullName::try_from(name).map_err(git_error)?;
    Ok(())
}

fn validate_remote_ref(name: &str) -> Result<(), Error> {
    if !name.starts_with("refs/heads/") {
        return Err(Error::Git(format!("unsupported remote push ref {name:?}")));
    }
    gix::refs::FullName::try_from(name).map_err(git_error)?;
    Ok(())
}

fn collect_tree(
    repository: &gix::Repository,
    id: gix::hash::ObjectId,
    seen: &mut HashSet<gix::hash::ObjectId>,
    objects: &mut Vec<gix::ObjectDetached>,
) -> Result<(), Error> {
    if !seen.insert(id) {
        return Ok(());
    }
    let object = repository.find_object(id).map_err(git_error)?;
    if object.kind != gix::objs::Kind::Tree {
        return Err(Error::Git(format!(
            "expected tree {id}, found {:?}",
            object.kind
        )));
    }
    let children = gix::objs::TreeRefIter::from_bytes(&object.data, id.kind())
        .map(|entry| entry.map(|entry| (entry.mode, entry.oid.to_owned())))
        .collect::<Result<Vec<_>, _>>()
        .map_err(git_error)?;
    objects.push(object.detach());
    for (mode, child_id) in children {
        if mode.kind() == gix::objs::tree::EntryKind::Commit {
            return Err(Error::Git(
                "submodules are not supported in push trees".to_owned(),
            ));
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
) -> Result<(), Error> {
    if seen.insert(id) {
        objects.push(repository.find_object(id).map_err(git_error)?.detach());
    }
    Ok(())
}

fn create_pack(objects: Vec<gix::ObjectDetached>) -> Result<Vec<u8>, Error> {
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
        .collect::<Result<Vec<_>, _>>()
        .map_err(git_error)?;
    let count = u32::try_from(entries.len()).map_err(git_error)?;
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
        result.map_err(git_error)?;
    }
    Ok(output)
}

fn git_error(error: impl std::fmt::Display + std::fmt::Debug) -> Error {
    Error::Git(format!("{error:#?}"))
}
