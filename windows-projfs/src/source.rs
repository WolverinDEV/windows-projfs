use std::{
    self,
    ffi::OsStr,
    fs::DirEntry,
    io::{
        self,
        Read,
    },
    path::Path,
};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum DirectoryEntry {
    Directory(DirectoryInfo),
    File(FileInfo),
}

impl DirectoryEntry {
    pub fn name(&self) -> &str {
        match self {
            Self::Directory(dir) => &dir.name,
            Self::File(file) => &file.file_name,
        }
    }
}

impl From<FileInfo> for DirectoryEntry {
    fn from(value: FileInfo) -> Self {
        Self::File(value)
    }
}

impl From<DirectoryInfo> for DirectoryEntry {
    fn from(value: DirectoryInfo) -> Self {
        Self::Directory(value)
    }
}

impl TryFrom<DirEntry> for DirectoryEntry {
    type Error = std::io::Error;

    fn try_from(value: DirEntry) -> Result<Self, Self::Error> {
        let file_name = value.file_name().to_string_lossy().to_string();
        let file_type = value.file_type()?;
        if file_type.is_dir() {
            Ok(DirectoryInfo { name: file_name }.into())
        } else if file_type.is_file() {
            use std::os::windows::fs::MetadataExt;

            let metadata = value.metadata()?;
            Ok(FileInfo {
                file_name,
                file_size: metadata.len(),
                file_attributes: metadata.file_attributes(),

                creation_time: metadata.creation_time(),
                last_access_time: metadata.last_access_time(),
                last_write_time: metadata.last_write_time(),
            }
            .into())
        } else {
            Err(io::Error::other("file type is not supported"))
        }
    }
}

#[derive(Default, Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct FileInfo {
    pub file_name: String,
    pub file_size: u64,
    pub file_attributes: u32,

    pub creation_time: u64,
    pub last_access_time: u64,
    pub last_write_time: u64,
}

#[derive(Default, Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct DirectoryInfo {
    pub name: String,
}

pub trait ProjectedFileSystemSource {
    fn list_directory(&self, path: &Path) -> Vec<DirectoryEntry>;
    fn get_directory_entry(&self, path: &Path) -> Option<DirectoryEntry> {
        let directory = path.parent().map(Path::to_path_buf).unwrap_or_default();
        let file_name = path.file_name().map(OsStr::to_string_lossy)?;

        self.list_directory(&directory)
            .into_iter()
            .find(|entry| entry.name() == file_name)
    }

    fn stream_file_content(
        &self,
        path: &Path,
        byte_offset: usize,
        length: usize,
    ) -> std::io::Result<Box<dyn Read>>;
}
