//! Read-only access to the saved files, shared by terminal and future GUI adapters.
use crate::Paths;
use anyhow::{bail, Result};
use std::{path::PathBuf, time::SystemTime};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Folder {
    Notes,
    Audio,
}
impl Folder {
    pub fn name(self) -> &'static str {
        match self {
            Self::Notes => "notes",
            Self::Audio => "audio",
        }
    }
    fn extension(self) -> &'static str {
        match self {
            Self::Notes => "md",
            Self::Audio => "wav",
        }
    }
}

#[derive(Clone, Debug)]
pub struct SavedFile {
    pub path: PathBuf,
    pub name: String,
    pub bytes: u64,
    pub modified: SystemTime,
}

/// Completed files only; hidden lock files and in-progress writes are excluded.
pub fn list(paths: &Paths, folder: Folder) -> Result<Vec<SavedFile>> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(paths.root.join(folder.name()))? {
        let entry = entry?;
        let path = entry.path();
        if !entry.file_type()?.is_file()
            || path.extension().and_then(|value| value.to_str()) != Some(folder.extension())
        {
            continue;
        }
        // Recording may remove an audio clip between listing and stat.
        let metadata = match entry.metadata() {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        files.push(SavedFile {
            name: entry.file_name().to_string_lossy().into_owned(),
            path,
            bytes: metadata.len(),
            modified: metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
        });
    }
    files.sort_by(|a, b| {
        b.modified
            .cmp(&a.modified)
            .then_with(|| a.name.cmp(&b.name))
    });
    Ok(files)
}

pub fn read_note(file: &SavedFile) -> Result<String> {
    if file.path.extension().and_then(|value| value.to_str()) != Some("md") {
        bail!("This file is not a Markdown note");
    }
    Ok(std::fs::read_to_string(&file.path)?)
}
