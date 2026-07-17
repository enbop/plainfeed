use std::{env, error::Error};

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = env::args().skip(1);
    let operation = args.next().unwrap_or_else(|| "inspect".into());
    let path = args.next().unwrap_or_else(|| ".".into());

    if operation == "init-commit" {
        return init_and_commit(path);
    }

    if operation != "inspect" {
        return Err(format!("unknown operation: {operation}").into());
    }

    let repository = gix::open(path)?;
    let git_dir = repository.git_dir().display();

    let mut changes = 0_u64;
    let status = repository.status(gix::progress::Discard)?;
    for item in status.into_iter(std::iter::empty())? {
        item?;
        changes += 1;
    }

    println!("implementation=gix");
    println!("git_dir={git_dir}");
    println!("changes={changes}");
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

    println!("implementation=gix");
    println!("commit={commit}");
    println!("tree={tree}");
    Ok(())
}
