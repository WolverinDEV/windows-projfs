use std::{
    self,
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
    rc::Rc,
};

use parking_lot::Mutex;
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
        PRJ_NOTIFICATION_MAPPING,
        PRJ_NOTIFY_FILE_HANDLE_CLOSED_FILE_DELETED,
        PRJ_NOTIFY_FILE_HANDLE_CLOSED_FILE_MODIFIED,
        PRJ_NOTIFY_FILE_HANDLE_CLOSED_NO_MODIFICATION,
        PRJ_NOTIFY_FILE_OPENED,
        PRJ_NOTIFY_FILE_OVERWRITTEN,
        PRJ_NOTIFY_FILE_PRE_CONVERT_TO_FULL,
        PRJ_NOTIFY_FILE_RENAMED,
        PRJ_NOTIFY_HARDLINK_CREATED,
        PRJ_NOTIFY_NEW_FILE_CREATED,
        PRJ_NOTIFY_PRE_DELETE,
        PRJ_NOTIFY_PRE_RENAME,
        PRJ_NOTIFY_PRE_SET_HARDLINK,
        PRJ_NOTIFY_TYPES,
        PRJ_STARTVIRTUALIZING_OPTIONS,
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

pub(crate) type RawProjectionContext = Mutex<ProjectionContext>;
pub(crate) struct ProjectionContext {
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

pub struct ProjectedFileSystem {
    instance_id: GUID,
    raw_context: *mut Mutex<ProjectionContext>,
    virtualization_context: PRJ_NAMESPACE_VIRTUALIZATION_CONTEXT,
}

static EMPTY_U16_STRING: &'static [u16] = &[0];

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

        let context = Box::new(Mutex::new(ProjectionContext {
            source: Box::new(source),
            virtualization_context: PRJ_NAMESPACE_VIRTUALIZATION_CONTEXT::default(),
            directory_enumerations: Default::default(),
        }));

        let callbacks = Box::new(PRJ_CALLBACKS {
            StartDirectoryEnumerationCallback: Some(native::start_directory_enumeration_callback),
            GetDirectoryEnumerationCallback: Some(native::get_directory_enumeration_callback),
            EndDirectoryEnumerationCallback: Some(native::end_directory_enumeration_callback),

            GetPlaceholderInfoCallback: Some(native::get_placeholder_information_callback),
            GetFileDataCallback: Some(native::get_file_data_callback),

            NotificationCallback: Some(native::notification_callback),
            ..Default::default()
        });

        let raw_context = Box::into_raw(context);
        let virtualization_context = unsafe {
            let context = &mut *raw_context;
            let mut context = context.lock();

            let notification_mask = 0
                | PRJ_NOTIFY_FILE_HANDLE_CLOSED_FILE_DELETED.0
                | PRJ_NOTIFY_FILE_HANDLE_CLOSED_FILE_MODIFIED.0
                | PRJ_NOTIFY_FILE_HANDLE_CLOSED_NO_MODIFICATION.0
                | PRJ_NOTIFY_FILE_OPENED.0
                | PRJ_NOTIFY_FILE_OVERWRITTEN.0
                | PRJ_NOTIFY_FILE_PRE_CONVERT_TO_FULL.0
                | PRJ_NOTIFY_FILE_RENAMED.0
                | PRJ_NOTIFY_HARDLINK_CREATED.0
                | PRJ_NOTIFY_NEW_FILE_CREATED.0
                | PRJ_NOTIFY_PRE_DELETE.0
                | PRJ_NOTIFY_PRE_RENAME.0
                | PRJ_NOTIFY_PRE_SET_HARDLINK.0;

            let mut notification_mapping = PRJ_NOTIFICATION_MAPPING {
                NotificationBitMask: PRJ_NOTIFY_TYPES(notification_mask),
                NotificationRoot: PCWSTR(EMPTY_U16_STRING.as_ptr()),
            };

            let mut options = PRJ_STARTVIRTUALIZING_OPTIONS::default();
            options.NotificationMappings = &mut notification_mapping;
            options.NotificationMappingsCount = 1;

            context.virtualization_context = PrjStartVirtualizing(
                PCWSTR(root_encoded.as_ptr()),
                &*callbacks,
                Some(raw_context as *const c_void),
                Some(&options),
            )
            .map_err(Error::StartProjection)?;

            context.virtualization_context
        };

        log::debug!(
            "Started projection {:X} at {}",
            instance_id.to_u128(),
            root.to_string_lossy()
        );
        Ok(Self {
            instance_id,
            raw_context,
            virtualization_context,
        })
    }
}

impl Drop for ProjectedFileSystem {
    fn drop(&mut self) {
        log::trace!("Stopping projection for {:X}", self.instance_id.to_u128());

        /* Shutdown projection */
        unsafe { PrjStopVirtualizing(self.virtualization_context) };

        /*
         * Await every currently executing call, before deallocating the raw context.
         * No new calls for the callbacks should occurr, as the projection has been stopped.
         */
        let context = unsafe { Box::from_raw(self.raw_context) };
        let _context = context.lock();

        log::debug!("Stopped projection for {:X}", self.instance_id.to_u128());
    }
}

mod native {
    use std::{
        ffi::{
            c_void,
            OsString,
        },
        mem,
        ops::ControlFlow,
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
                ERROR_OUTOFMEMORY,
                STATUS_CANNOT_DELETE,
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
                PRJ_NOTIFICATION,
                PRJ_NOTIFICATION_FILE_HANDLE_CLOSED_FILE_DELETED,
                PRJ_NOTIFICATION_FILE_HANDLE_CLOSED_FILE_MODIFIED,
                PRJ_NOTIFICATION_FILE_HANDLE_CLOSED_NO_MODIFICATION,
                PRJ_NOTIFICATION_FILE_OPENED,
                PRJ_NOTIFICATION_FILE_OVERWRITTEN,
                PRJ_NOTIFICATION_FILE_PRE_CONVERT_TO_FULL,
                PRJ_NOTIFICATION_FILE_RENAMED,
                PRJ_NOTIFICATION_HARDLINK_CREATED,
                PRJ_NOTIFICATION_NEW_FILE_CREATED,
                PRJ_NOTIFICATION_PARAMETERS,
                PRJ_NOTIFICATION_PRE_DELETE,
                PRJ_NOTIFICATION_PRE_RENAME,
                PRJ_NOTIFICATION_PRE_SET_HARDLINK,
                PRJ_PLACEHOLDER_INFO,
            },
        },
    };

    use super::{
        FileNameU16Cache,
        RawProjectionContext,
    };
    use crate::{
        aligned_buffer::PrjAlignedBuffer,
        utils::io_result_to_hresult,
        DirectoryEntry,
        FileCloseAction,
        FileRenameInfo,
        Notification,
        ProjectedFile,
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

    type CallbackData = crate::CallbackData<'static, RawProjectionContext>;
    pub unsafe extern "system" fn start_directory_enumeration_callback(
        callback_data: *const PRJ_CALLBACK_DATA,
        enumeration_id: *const GUID,
    ) -> HRESULT {
        let enumeration_id = &*enumeration_id;
        let callback_data: CallbackData = callback_data.into();

        callback_data.execute(move |callback_data| {
            let target = callback_data.file_path.clone().unwrap_or_default();
            let mut context = callback_data.context.lock();
            context.register_enumeration(target, enumeration_id.to_u128());

            Ok(())
        })
    }

    pub unsafe extern "system" fn end_directory_enumeration_callback(
        callback_data: *const PRJ_CALLBACK_DATA,
        enumeration_id: *const GUID,
    ) -> HRESULT {
        let enumeration_id = &*enumeration_id;
        let callback_data: CallbackData = callback_data.into();

        callback_data.execute(move |callback_data| {
            let mut context = callback_data.context.lock();
            if !context.finish_enumeration(enumeration_id.to_u128()) {
                log::warn!(
                    "Tried to end an non existing enumeration with id {:X}",
                    enumeration_id.to_u128()
                );
            }

            Ok(())
        })
    }

    pub unsafe extern "system" fn get_directory_enumeration_callback(
        callback_data: *const PRJ_CALLBACK_DATA,
        enumeration_id: *const GUID,
        _searchexpression: PCWSTR,
        dir_entry_buffer_handle: PRJ_DIR_ENTRY_BUFFER_HANDLE,
    ) -> HRESULT {
        let enumeration_id = &*enumeration_id;
        let callback_data: CallbackData = callback_data.into();

        callback_data.execute(move |callback_data| {
            let mut context = callback_data.context.lock();
            let enumeration = context
                .directory_enumerations
                .get_mut(&enumeration_id.to_u128())
                /* Return STATUS_SUCCESS to indicate that the enumeration has ended (as it can not be found). */
                .ok_or(STATUS_SUCCESS.to_hresult())?;

            if callback_data.flags.0 & PRJ_CB_DATA_FLAG_ENUM_RESTART_SCAN.0 > 0 {
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
                    return Err(err.code());
                }

                enumeration.consume_entry();

                if callback_data.flags.0 & PRJ_CB_DATA_FLAG_ENUM_RETURN_SINGLE_ENTRY.0 > 0 {
                    break;
                }
            }

            Ok(())
        })
    }

    pub unsafe extern "system" fn get_placeholder_information_callback(
        callback_data: *const PRJ_CALLBACK_DATA,
    ) -> HRESULT {
        let callback_data: CallbackData = callback_data.into();

        callback_data.execute(move |callback_data| {
            let path = callback_data.file_path.clone().unwrap_or_default();

            let context = callback_data.context.lock();
            let entry = context
                .source
                .get_directory_entry(&path)
                .ok_or(ERROR_FILE_NOT_FOUND.to_hresult())?;

            let mut name_cache = FileNameU16Cache::default();
            let name = name_cache.get_or_cache(path.display().to_string());

            let placeholder_info = PRJ_PLACEHOLDER_INFO {
                FileBasicInfo: entry.get_basic_info(),
                ..PRJ_PLACEHOLDER_INFO::default()
            };

            if let Some(extended_info) = entry.get_extended_info() {
                unsafe {
                    PrjWritePlaceholderInfo2(
                        context.virtualization_context,
                        PCWSTR(name.as_ptr()),
                        &placeholder_info,
                        mem::size_of_val(&placeholder_info) as u32,
                        Some(&extended_info),
                    )
                    .map_err(|err| err.code())?;
                }
            } else {
                unsafe {
                    PrjWritePlaceholderInfo(
                        context.virtualization_context,
                        PCWSTR(name.as_ptr()),
                        &placeholder_info,
                        mem::size_of_val(&placeholder_info) as u32,
                    )
                    .map_err(|err| err.code())?;
                }
            };

            Ok(())
        })
    }

    pub unsafe extern "system" fn get_file_data_callback(
        callback_data: *const PRJ_CALLBACK_DATA,
        byte_offset: u64,
        length: u32,
    ) -> HRESULT {
        let length = length as usize;
        let callback_data: CallbackData = callback_data.into();

        callback_data.execute(move |callback_data| {
            let path = callback_data.file_path.clone().unwrap_or_default();

            let context = callback_data.context.lock();
            let mut source = context
                .source
                .stream_file_content(&path, byte_offset as usize, length)
                .map_err(io_result_to_hresult)?;

            let chunk_length = if length <= 1024 * 1024 {
                length
            } else {
                1024 * 1024
            };

            let mut buffer =
                PrjAlignedBuffer::allocate(context.virtualization_context, chunk_length)
                    .ok_or(ERROR_OUTOFMEMORY.to_hresult())?;
            let buffer = buffer.buffer();

            let mut bytes_written = 0;
            while bytes_written < length {
                let bytes_pending = length - bytes_written;
                let chunk_length = bytes_pending.min(buffer.len());

                source
                    .read_exact(&mut buffer[0..chunk_length])
                    .inspect_err(|err| {
                        log::debug!("IO error for reading {} bytes: {}", chunk_length, err)
                    })
                    .map_err(io_result_to_hresult)?;

                let write_result = unsafe {
                    PrjWriteFileData(
                        context.virtualization_context,
                        &callback_data.data_stream_id,
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
                    return Err(err.code());
                }

                bytes_written += chunk_length;
            }

            Ok(())
        })
    }

    pub unsafe extern "system" fn notification_callback(
        callback_data: *const PRJ_CALLBACK_DATA,
        is_directory: BOOLEAN,
        notification: PRJ_NOTIFICATION,
        destination_filename: PCWSTR,
        _operation_parameters: *mut PRJ_NOTIFICATION_PARAMETERS,
    ) -> HRESULT {
        let callback_data: CallbackData = callback_data.into();

        let destination_filename = if destination_filename.is_null() {
            None
        } else {
            Some(PathBuf::from(OsString::from_wide(
                destination_filename.as_wide(),
            )))
        };

        callback_data.execute(move |callback_data| {
            let target_file = ProjectedFile {
                file_id: callback_data.file_id.to_u128(),
                is_directory: is_directory.as_bool(),
                path: callback_data.file_path.clone().unwrap_or_default(),
            };

            let notification = match notification {
                PRJ_NOTIFICATION_NEW_FILE_CREATED => Notification::FileCreated(target_file),
                PRJ_NOTIFICATION_FILE_OPENED => Notification::FileOpened(target_file),
                PRJ_NOTIFICATION_FILE_HANDLE_CLOSED_FILE_DELETED => {
                    Notification::FileClosed(target_file, FileCloseAction::Deleted)
                }
                PRJ_NOTIFICATION_FILE_HANDLE_CLOSED_FILE_MODIFIED => {
                    Notification::FileClosed(target_file, FileCloseAction::Modified)
                }
                PRJ_NOTIFICATION_FILE_HANDLE_CLOSED_NO_MODIFICATION => {
                    Notification::FileClosed(target_file, FileCloseAction::NoModification)
                }
                PRJ_NOTIFICATION_FILE_OVERWRITTEN => Notification::FileOverwritten(target_file),

                PRJ_NOTIFICATION_PRE_RENAME => Notification::PreFileRename(FileRenameInfo {
                    source: callback_data.file_path.clone(),
                    destination: destination_filename,
                }),
                PRJ_NOTIFICATION_FILE_RENAMED => Notification::FileRenamed(FileRenameInfo {
                    source: callback_data.file_path.clone(),
                    destination: destination_filename,
                }),

                PRJ_NOTIFICATION_PRE_SET_HARDLINK => Notification::PreSetHardlink(target_file),
                PRJ_NOTIFICATION_HARDLINK_CREATED => Notification::HardlinkCreated(target_file),

                PRJ_NOTIFICATION_FILE_PRE_CONVERT_TO_FULL => {
                    Notification::FilePreConvertToFull(target_file)
                }
                PRJ_NOTIFICATION_PRE_DELETE => Notification::PreFileDelete(target_file),

                notification => {
                    log::warn!("Invalid notification {}", notification.0);
                    return Ok(());
                }
            };

            let context = callback_data.context.lock();
            let action = context.source.handle_notification(&notification);
            if matches!(action, ControlFlow::Break(_)) {
                if notification.is_cancelable() {
                    return Err(STATUS_CANNOT_DELETE.to_hresult());
                }

                log::warn!(
                    "Tried to cancel a non cancelable action: {:?}",
                    notification
                );
            }

            Ok(())
        })
    }
}
