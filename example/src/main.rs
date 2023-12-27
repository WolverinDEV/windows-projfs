use std::{
    ffi::OsStr,
    fs,
    io::{
        self,
        Cursor,
        Read,
    },
    path::{
        Path,
        PathBuf,
    },
};

use clap::Parser;
use windows_projfs::{
    DirectoryEntry,
    DirectoryInfo,
    FileInfo,
    ProjectedFileSystem,
    ProjectedFileSystemSource,
};

struct PFSBackend {}
impl PFSBackend {
    pub fn new() -> Self {
        Self {}
    }
}
impl ProjectedFileSystemSource for PFSBackend {
    fn list_directory(&self, path: &std::path::Path) -> Vec<windows_projfs::DirectoryEntry> {
        if path.display().to_string().is_empty() {
            vec![
                DirectoryEntry::Directory(DirectoryInfo {
                    name: format!("test-dir"),
                    ..Default::default()
                }),
                DirectoryEntry::File(FileInfo {
                    file_name: "test.txt".to_string(),
                    file_size: 12,

                    ..Default::default()
                }),
            ]
        } else {
            vec![]
        }
    }

    fn get_directory_entry(&self, path: &std::path::Path) -> Option<DirectoryEntry> {
        let directory = path.parent().map(Path::to_path_buf).unwrap_or_default();
        let file_name = path.file_name().map(OsStr::to_string_lossy)?;

        self.list_directory(&directory)
            .into_iter()
            .find(|entry| entry.name() == file_name)
    }

    fn stream_file_content(
        &self,
        _path: &std::path::Path,
        _byte_offset: usize,
        _length: usize,
    ) -> std::io::Result<Box<dyn Read>> {
        let buffer = "Hello World\n".to_owned().into_bytes();

        Ok(Box::new(Cursor::new(buffer)))
    }
}

#[derive(clap::Parser)]
struct Args {
    #[clap(short, long)]
    root: PathBuf,
}

fn pause() {
    log::info!("Press any key to continue...");
    let mut stdin = io::stdin();
    let _ = stdin.read(&mut [0u8]).unwrap();
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    env_logger::init();

    if args.root.exists() {
        log::error!("Target path does already exists.");
        log::error!("The root path should not be existent.");
        return Ok(());
    } else {
        log::debug!("Creating {}", args.root.display());
        fs::create_dir_all(&args.root)?;
    }

    log::info!("Starting projected file system ({})", args.root.display());
    {
        let backend = Box::new(PFSBackend::new());
        let _pfs = ProjectedFileSystem::new(args.root.clone(), backend)?;
        pause();
    }
    log::info!("Stopped projected file system. Cleaning up root.");
    fs::remove_dir_all(&args.root)?;
    log::info!("Root path cleaned.");
    Ok(())
}
