use std::ffi::c_void;

use windows::Win32::Storage::ProjectedFileSystem::{
    PrjAllocateAlignedBuffer,
    PrjFreeAlignedBuffer,
    PRJ_NAMESPACE_VIRTUALIZATION_CONTEXT,
};

pub struct PrjAlignedBuffer {
    length: usize,
    raw_buffer: *mut c_void,
}

impl PrjAlignedBuffer {
    pub fn allocate(context: PRJ_NAMESPACE_VIRTUALIZATION_CONTEXT, length: usize) -> Option<Self> {
        let raw_buffer = unsafe { PrjAllocateAlignedBuffer(context, length) };
        if raw_buffer.is_null() {
            None
        } else {
            Some(Self { length, raw_buffer })
        }
    }

    pub fn buffer(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.raw_buffer as *mut u8, self.length) }
    }
}

impl Drop for PrjAlignedBuffer {
    fn drop(&mut self) {
        unsafe { PrjFreeAlignedBuffer(self.raw_buffer) };
    }
}
