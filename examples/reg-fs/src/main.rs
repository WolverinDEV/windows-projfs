use std::{
    fs,
    io::{
        self,
        Cursor,
        Read,
    },
    path::PathBuf,
};

use clap::Parser;
use windows_projfs::{
    DirectoryEntry,
    DirectoryInfo,
    FileInfo,
    ProjectedFileSystem,
    ProjectedFileSystemSource,
};
use winreg::{
    enums::HKEY_LOCAL_MACHINE,
    RegKey,
};

struct RegistryProjectedSource {
    root_key: RegKey,
}

impl ProjectedFileSystemSource for RegistryProjectedSource {
    fn list_directory(&self, path: &std::path::Path) -> Vec<windows_projfs::DirectoryEntry> {
        let key = match self.root_key.open_subkey(path) {
            Ok(key) => key,
            Err(_) => return vec![],
        };

        let directories = key
            .enum_keys()
            .filter_map(|key| key.ok())
            .map(|name| DirectoryEntry::Directory(DirectoryInfo { name }));

        let files = key
            .enum_values()
            .filter_map(|value| value.ok())
            .map(|(name, key)| {
                DirectoryEntry::File(FileInfo {
                    file_name: name,
                    file_size: key.bytes.len() as u64,

                    ..Default::default()
                })
            });

        directories.chain(files).collect()
    }

    fn stream_file_content(
        &self,
        path: &std::path::Path,
        byte_offset: usize,
        length: usize,
    ) -> std::io::Result<Box<dyn Read>> {
        let file_name = path.file_name().ok_or(io::Error::new(
            io::ErrorKind::InvalidInput,
            "path is missing a file name",
        ))?;

        let value = if let Some(parent) = path.parent() {
            self.root_key
                .open_subkey(parent)?
                .get_raw_value(file_name)?
        } else {
            self.root_key.get_raw_value(file_name)?
        };

        if byte_offset + length > value.bytes.len() {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "invalid read operation",
            ));
        }

        Ok(Box::new(Cursor::new(
            value.bytes[byte_offset..(byte_offset + length)].to_owned(),
        )))
    }
}

#[derive(clap::Parser)]
struct Args {
    #[clap(short, long)]
    projection_root: PathBuf,

    #[clap(short, long)]
    registry_root: String,
}

fn pause() {
    log::info!("Press any key to continue...");
    let mut stdin = io::stdin();
    let _ = stdin.read(&mut [0u8]).unwrap();
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    env_logger::init();

    if args.projection_root.exists() {
        log::error!("Target path does already exists.");
        log::error!("The root path should not be existent.");
        return Ok(());
    } else {
        log::debug!("Creating {}", args.projection_root.display());
        fs::create_dir_all(&args.projection_root)?;
    }

    log::info!(
        "Starting projected registry at {}",
        args.projection_root.display()
    );
    {
        let root_key = RegKey::predef(HKEY_LOCAL_MACHINE);

        let _pfs =
            ProjectedFileSystem::new(&args.projection_root, RegistryProjectedSource { root_key })?;
        pause();
    }

    log::info!("Stopped projection. Cleaning up root.");
    fs::remove_dir_all(&args.projection_root)?;
    log::info!("Root path cleaned.");
    Ok(())
}
