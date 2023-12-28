use std::{
    cell::RefCell,
    collections::{
        btree_map::Entry,
        BTreeMap,
    },
    ffi::c_void,
    path::{
        Path,
        PathBuf,
    },
    pin::Pin,
    rc::Rc,
};

use windows::{
    core::{
        GUID,
        PCWSTR,
    },
    Win32::Storage::ProjectedFileSystem::{
        PrjFileNameCompare,
        PrjStartVirtualizing,
        PrjStopVirtualizing,
        PRJ_CALLBACKS,
        PRJ_NAMESPACE_VIRTUALIZATION_CONTEXT,
    },
};

use crate::{
    DirectoryEntry,
    Error,
    ProjectedFileSystemSource,
    Result,
};

#[derive(Default)]
struct FileNameU16Cache {
    cache: BTreeMap<String, Vec<u16>>,
}

impl FileNameU16Cache {
    pub fn get_or_cache(&mut self, name: String) -> &[u16] {
        match self.cache.entry(name) {
            Entry::Occupied(entry) => entry.into_mut().as_slice(),
            Entry::Vacant(entry) => {
                let mut name = entry.key().encode_utf16().collect::<Vec<_>>();
                name.push(0);

                entry.insert(name).as_slice()
            }
        }
    }
}

struct DirectoryIteration {
    id: u128,

    entries: Vec<DirectoryEntry>,
    current_entry: usize,

    name_cache: Rc<RefCell<FileNameU16Cache>>,
}

impl DirectoryIteration {
    pub fn from_unsorted(id: u128, mut entries: Vec<DirectoryEntry>) -> Self {
        let name_cache: Rc<RefCell<FileNameU16Cache>> = Default::default();
        entries.sort_unstable_by({
            let name_cache = name_cache.clone();
            move |a, b| {
                let mut name_cache = name_cache.borrow_mut();
                let name_a = name_cache.get_or_cache(a.name().to_string()).as_ptr();
                let name_b = name_cache.get_or_cache(b.name().to_string()).as_ptr();

                let result = unsafe { PrjFileNameCompare(PCWSTR(name_a), PCWSTR(name_b)) };
                result.cmp(&0)
            }
        });

        Self {
            id,

            entries,
            current_entry: 0,

            name_cache,
        }
    }

    pub fn peek_entry(&mut self) -> Option<&DirectoryEntry> {
        let index = self.current_entry;
        if index < self.entries.len() {
            Some(&self.entries[index])
        } else {
            None
        }
    }

    pub fn consume_entry(&mut self) {
        self.current_entry += 1;
    }

    pub fn reset_enumeration(&mut self) {
        self.current_entry = 0;
    }
}

struct ProjectionContext {
    instance_id: GUID,
    source: Box<dyn ProjectedFileSystemSource>,
    virtualization_context: PRJ_NAMESPACE_VIRTUALIZATION_CONTEXT,

    directory_enumerations: BTreeMap<u128, DirectoryIteration>,
}

impl ProjectionContext {
    pub fn register_enumeration(&mut self, target: PathBuf, id: u128) {
        let old_enumeration = self.directory_enumerations.insert(
            id,
            DirectoryIteration::from_unsorted(id, self.source.list_directory(&target)),
        );

        if let Some(enumeration) = old_enumeration {
            log::warn!("Duplicate enumeration id {:X}", enumeration.id);
        }
    }

    pub fn finish_enumeration(&mut self, id: u128) -> bool {
        self.directory_enumerations.remove(&id).is_some()
    }
}

impl Drop for ProjectionContext {
    fn drop(&mut self) {
        if self.virtualization_context.is_invalid() {
            /* context never started */
            return;
        }

        log::debug!("Stopping projection for {:X}", self.instance_id.to_u128());
        unsafe { PrjStopVirtualizing(self.virtualization_context) };
    }
}

pub struct ProjectedFileSystem {
    _context: Pin<Box<ProjectionContext>>,
}

impl ProjectedFileSystem {
    pub fn new(root: &Path, source: impl ProjectedFileSystemSource + 'static) -> Result<Self> {
        let instance_id = GUID::new()?;
        let mut root_encoded = root.to_string_lossy().encode_utf16().collect::<Vec<_>>();
        root_encoded.push(0);

        unsafe {
            use windows::Win32::Storage::ProjectedFileSystem::PrjMarkDirectoryAsPlaceholder;

            PrjMarkDirectoryAsPlaceholder(
                PCWSTR(root_encoded.as_ptr()),
                PCWSTR::null(),
                None,
                &instance_id,
            )
        }
        .map_err(Error::MarkProjectionRoot)?;

        let mut context = Box::pin(ProjectionContext {
            instance_id,
            source: Box::new(source),
            virtualization_context: PRJ_NAMESPACE_VIRTUALIZATION_CONTEXT::default(),
            directory_enumerations: Default::default(),
        });

        let callbacks = Box::new(PRJ_CALLBACKS {
            StartDirectoryEnumerationCallback: Some(native::start_directory_enumeration_callback),
            GetDirectoryEnumerationCallback: Some(native::get_directory_enumeration_callback),
            EndDirectoryEnumerationCallback: Some(native::end_directory_enumeration_callback),

            GetPlaceholderInfoCallback: Some(native::get_placeholder_information_callback),
            GetFileDataCallback: Some(native::get_file_data_callback),

            ..Default::default()
        });

        context.virtualization_context = unsafe {
            PrjStartVirtualizing(
                PCWSTR(root_encoded.as_ptr()),
                &*callbacks,
                Some(&*context as *const _ as *const c_void),
                None,
            )
        }
        .map_err(Error::StartProjection)?;

        log::debug!(
            "Started projection {:X} at {}",
            context.instance_id.to_u128(),
            root.to_string_lossy()
        );
        Ok(Self { _context: context })
    }
}

mod native {
    use std::{
        ffi::{
            c_void,
            OsString,
        },
        mem,
        os::windows::ffi::OsStringExt,
        path::PathBuf,
    };

    use windows::{
        core::{
            GUID,
            HRESULT,
            PCWSTR,
        },
        Win32::{
            Foundation::{
                BOOLEAN,
                ERROR_FILE_NOT_FOUND,
                ERROR_INSUFFICIENT_BUFFER,
                ERROR_IO_INCOMPLETE,
                ERROR_OUTOFMEMORY,
                STATUS_SUCCESS,
            },
            Storage::ProjectedFileSystem::{
                PrjFillDirEntryBuffer2,
                PrjWriteFileData,
                PrjWritePlaceholderInfo,
                PrjWritePlaceholderInfo2,
                PRJ_CALLBACK_DATA,
                PRJ_CB_DATA_FLAG_ENUM_RESTART_SCAN,
                PRJ_CB_DATA_FLAG_ENUM_RETURN_SINGLE_ENTRY,
                PRJ_DIR_ENTRY_BUFFER_HANDLE,
                PRJ_EXTENDED_INFO,
                PRJ_FILE_BASIC_INFO,
                PRJ_PLACEHOLDER_INFO,
            },
        },
    };

    use super::FileNameU16Cache;
    use crate::{
        aligned_buffer::PrjAlignedBuffer,
        fs::ProjectionContext,
        utils::io_result_to_hresult,
        DirectoryEntry,
    };

    impl DirectoryEntry {
        fn get_basic_info(&self) -> PRJ_FILE_BASIC_INFO {
            let mut basic_info = PRJ_FILE_BASIC_INFO::default();

            match self {
                Self::Directory(_) => {
                    basic_info.IsDirectory = BOOLEAN::from(true);
                }
                Self::File(file) => {
                    basic_info.IsDirectory = BOOLEAN::from(false);

                    basic_info.FileSize = file.file_size as i64;
                    basic_info.FileAttributes = file.file_attributes;

                    basic_info.CreationTime = file.creation_time as i64;
                    basic_info.LastAccessTime = file.last_access_time as i64;
                    basic_info.LastWriteTime = file.last_write_time as i64;

                    /*
                     * ChangeTime includes metadata changes, but we merge these together.
                     * Source: https://web.archive.org/web/20230404085857/https://devblogs.microsoft.com/oldnewthing/20100709-00/?p=13463
                     */
                    basic_info.ChangeTime = file.last_write_time as i64;
                }
            };

            basic_info
        }

        fn get_extended_info(&self) -> Option<PRJ_EXTENDED_INFO> {
            /* TODO: Symlinks */
            None
        }
    }

    pub unsafe extern "system" fn start_directory_enumeration_callback(
        callback_data: *const PRJ_CALLBACK_DATA,
        enumeration_id: *const GUID,
    ) -> HRESULT {
        let callback_data = &*callback_data;
        let enumeration_id = &*enumeration_id;

        let context = &mut *(callback_data.InstanceContext as *mut ProjectionContext);
        let path = PathBuf::from(OsString::from_wide(callback_data.FilePathName.as_wide()));

        context.register_enumeration(path, enumeration_id.to_u128());
        STATUS_SUCCESS.to_hresult()
    }

    pub unsafe extern "system" fn end_directory_enumeration_callback(
        callback_data: *const PRJ_CALLBACK_DATA,
        enumeration_id: *const GUID,
    ) -> HRESULT {
        let callback_data = &*callback_data;
        let enumeration_id = *enumeration_id;

        let context = &mut *(callback_data.InstanceContext as *mut ProjectionContext);

        if context.finish_enumeration(enumeration_id.to_u128()) {
            STATUS_SUCCESS.to_hresult()
        } else {
            log::warn!(
                "Tried to end an non existing enumeration with id {:X}",
                enumeration_id.to_u128()
            );
            STATUS_SUCCESS.to_hresult()
        }
    }

    pub unsafe extern "system" fn get_directory_enumeration_callback(
        callback_data: *const PRJ_CALLBACK_DATA,
        enumeration_id: *const GUID,
        _searchexpression: PCWSTR,
        dir_entry_buffer_handle: PRJ_DIR_ENTRY_BUFFER_HANDLE,
    ) -> HRESULT {
        let callback_data = &*callback_data;
        let enumeration_id = &*enumeration_id;

        let context = &mut *(callback_data.InstanceContext as *mut ProjectionContext);
        let enumeration = match context
            .directory_enumerations
            .get_mut(&enumeration_id.to_u128())
        {
            Some(enumeration) => enumeration,
            None => {
                /* Assume when the enumeration is unknown, we just finished */
                log::warn!(
                    "Tried to get a directory enumeration entry for an invalid enumeration {:X}",
                    enumeration_id.to_u128()
                );
                return STATUS_SUCCESS.to_hresult();
            }
        };

        if callback_data.Flags.0 & PRJ_CB_DATA_FLAG_ENUM_RESTART_SCAN.0 > 0 {
            enumeration.reset_enumeration();
        }

        let name_cache = enumeration.name_cache.clone();
        while let Some(entry) = enumeration.peek_entry() {
            let basic_info = entry.get_basic_info();
            let extended_info = entry.get_extended_info();

            /* TODO: Compare name with the search input! */

            let mut name_cache = name_cache.borrow_mut();
            let name = name_cache.get_or_cache(entry.name().to_string());

            let result = unsafe {
                PrjFillDirEntryBuffer2(
                    dir_entry_buffer_handle,
                    PCWSTR(name.as_ptr()),
                    Some(&basic_info),
                    extended_info.map(|v| &v as *const _),
                )
            };

            if let Err(err) = result {
                if err.code() == ERROR_INSUFFICIENT_BUFFER.to_hresult() {
                    /* buffer full */
                    break;
                }

                /* unexpected... */
                return err.code();
            }

            enumeration.consume_entry();

            if callback_data.Flags.0 & PRJ_CB_DATA_FLAG_ENUM_RETURN_SINGLE_ENTRY.0 > 0 {
                break;
            }
        }

        STATUS_SUCCESS.to_hresult()
    }

    pub unsafe extern "system" fn get_placeholder_information_callback(
        callback_data: *const PRJ_CALLBACK_DATA,
    ) -> HRESULT {
        let callback_data = &*callback_data;

        let context = &mut *(callback_data.InstanceContext as *mut ProjectionContext);
        let path = PathBuf::from(OsString::from_wide(callback_data.FilePathName.as_wide()));

        let entry = match context.source.get_directory_entry(&path) {
            Some(entry) => entry,
            None => return ERROR_FILE_NOT_FOUND.to_hresult(),
        };

        let mut name_cache = FileNameU16Cache::default();
        let name = name_cache.get_or_cache(path.display().to_string());

        let placeholder_info = PRJ_PLACEHOLDER_INFO {
            FileBasicInfo: entry.get_basic_info(),
            ..PRJ_PLACEHOLDER_INFO::default()
        };

        let result = if let Some(extended_info) = entry.get_extended_info() {
            unsafe {
                PrjWritePlaceholderInfo2(
                    context.virtualization_context,
                    PCWSTR(name.as_ptr()),
                    &placeholder_info,
                    mem::size_of_val(&placeholder_info) as u32,
                    Some(&extended_info),
                )
            }
        } else {
            unsafe {
                PrjWritePlaceholderInfo(
                    context.virtualization_context,
                    PCWSTR(name.as_ptr()),
                    &placeholder_info,
                    mem::size_of_val(&placeholder_info) as u32,
                )
            }
        };

        match result {
            Ok(_) => STATUS_SUCCESS.to_hresult(),
            Err(err) => err.code(),
        }
    }

    pub unsafe extern "system" fn get_file_data_callback(
        callback_data: *const PRJ_CALLBACK_DATA,
        byte_offset: u64,
        length: u32,
    ) -> HRESULT {
        let length = length as usize;
        let callback_data = &*callback_data;

        let context = &mut *(callback_data.InstanceContext as *mut ProjectionContext);
        let path = PathBuf::from(OsString::from_wide(callback_data.FilePathName.as_wide()));

        let mut source = match {
            context
                .source
                .stream_file_content(&path, byte_offset as usize, length)
        } {
            Ok(source) => source,
            Err(err) => {
                return HRESULT::from_win32(
                    err.raw_os_error().unwrap_or(ERROR_IO_INCOMPLETE.0 as i32) as u32,
                )
            }
        };

        let chunk_length = if length <= 1024 * 1024 {
            length
        } else {
            1024 * 1024
        };

        let mut buffer =
            match PrjAlignedBuffer::allocate(context.virtualization_context, chunk_length) {
                Some(buffer) => buffer,
                None => return ERROR_OUTOFMEMORY.to_hresult(),
            };
        let buffer = buffer.buffer();

        let mut bytes_written = 0;
        while bytes_written < length {
            let bytes_pending = length - bytes_written;
            let chunk_length = bytes_pending.min(buffer.len());

            if let Err(err) = source.read_exact(&mut buffer[0..chunk_length]) {
                log::debug!("IO error for reading {} bytes: {}", chunk_length, err);
                return io_result_to_hresult(err);
            }

            let write_result = unsafe {
                PrjWriteFileData(
                    context.virtualization_context,
                    &callback_data.DataStreamId,
                    buffer.as_ptr() as *const c_void,
                    byte_offset + bytes_written as u64,
                    chunk_length as u32,
                )
            };
            if let Err(err) = write_result {
                log::warn!(
                    "Failed to write projected file data for {}: {}",
                    path.display(),
                    err
                );
                return err.code();
            }

            bytes_written += chunk_length;
        }

        STATUS_SUCCESS.to_hresult()
    }
}
