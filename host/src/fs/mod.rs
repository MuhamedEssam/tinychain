use std::fmt;
use std::fs::Metadata;
use std::io;
use std::path::PathBuf;

use futures::TryFutureExt;
use tokio::fs;

use error::*;
use generic::PathSegment;

mod cache;
mod dir;
mod file;

pub use cache::*;
pub use dir::*;
pub use file::*;

type DirContents = Vec<(fs::DirEntry, Metadata)>;

async fn dir_contents(dir_path: &PathBuf) -> TCResult<Vec<(fs::DirEntry, Metadata)>> {
    let mut contents = vec![];
    let mut handles = fs::read_dir(dir_path)
        .map_err(|e| io_err(e, dir_path))
        .await?;

    while let Some(handle) = handles
        .next_entry()
        .map_err(|e| io_err(e, dir_path))
        .await?
    {
        let meta = handle
            .metadata()
            .map_err(|e| io_err(e, handle.path()))
            .await?;

        contents.push((handle, meta));
    }

    Ok(contents)
}

fn file_name(handle: &fs::DirEntry) -> TCResult<PathSegment> {
    if let Some(name) = handle.file_name().to_str() {
        name.parse()
    } else {
        Err(TCError::internal("Cannot load file with no name!"))
    }
}

fn fs_path(mount_point: &PathBuf, name: &PathSegment) -> PathBuf {
    let mut path = mount_point.clone();
    path.push(name.to_string());
    path
}

fn io_err<I: fmt::Debug>(err: io::Error, info: I) -> TCError {
    match err.kind() {
        io::ErrorKind::NotFound => {
            TCError::unsupported(format!("There is no directory at {:?}", info))
        }
        io::ErrorKind::PermissionDenied => TCError::internal(format!(
            "Tinychain does not have permission to access the host filesystem: {:?}",
            info
        )),
        other => TCError::internal(format!("host filesystem error: {:?}", other)),
    }
}