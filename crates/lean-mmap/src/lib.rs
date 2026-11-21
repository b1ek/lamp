#![no_std]

use core::ffi::c_void;
use core::ptr;

use rustix::fd::BorrowedFd;
use rustix::io::Errno;
use rustix::mm::{madvise, mmap, munmap, Advice, MapFlags, ProtFlags};

pub struct MmapMut {
    ptr: *mut u8,
    len: usize,
}

impl MmapMut {
    pub fn map_mut(fd: BorrowedFd<'_>, len: usize) -> Result<Self, Errno> {
        let mapped = unsafe {
            mmap(
                ptr::null_mut(),
                len,
                ProtFlags::READ | ProtFlags::WRITE,
                MapFlags::SHARED,
                fd,
                0,
            )
        }?;

        Ok(Self {
            ptr: mapped.cast::<u8>(),
            len,
        })
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn as_ptr(&self) -> *const u8 {
        self.ptr
    }

    pub fn as_mut_ptr(&mut self) -> *mut u8 {
        self.ptr
    }

    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        unsafe { core::slice::from_raw_parts_mut(self.ptr, self.len) }
    }

    pub fn drop_resident_pages(&self) -> Result<(), Errno> {
        unsafe { madvise(self.ptr.cast::<c_void>(), self.len, Advice::LinuxDontNeed) }
    }
}

impl Drop for MmapMut {
    fn drop(&mut self) {
        unsafe {
            let _ = munmap(self.ptr.cast::<c_void>(), self.len);
        }
    }
}
