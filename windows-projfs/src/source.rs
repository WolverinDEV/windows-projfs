use std::{
    io::Read,
    path::Path,
};

#[derive(Debug, Clone)]
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

#[derive(Debug, Clone, Default)]
pub struct FileInfo {
    pub file_name: String,
    pub file_size: i64,
    pub file_attributes: u32,

    pub creation_time: i64,
    pub last_access_time: i64,
    pub last_write_time: i64,
    pub change_time: i64,
}

#[derive(Debug, Clone, Default)]
pub struct DirectoryInfo {
    pub name: String,
}

pub trait ProjectedFileSystemSource {
    fn list_directory(&self, path: &Path) -> Vec<DirectoryEntry>;
    fn get_directory_entry(&self, path: &Path) -> Option<DirectoryEntry>;

    fn stream_file_content(
        &self,
        path: &Path,
        byte_offset: usize,
        length: usize,
    ) -> std::io::Result<Box<dyn Read>>;
}
