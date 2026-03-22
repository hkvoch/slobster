use std::mem::forget;
use std::num::NonZeroUsize;
use std::os::fd::RawFd;
use std::ptr::{NonNull, addr_of_mut, null_mut};
use std::sync::atomic::{AtomicUsize, Ordering};

use bitflags::bitflags;

use crate::sys::{SysError, SysResult};

static PAGE_SIZE: AtomicUsize = AtomicUsize::new(0);

pub(crate) fn get_page_size() -> NonZeroUsize {
	if let Some(size) = NonZeroUsize::new(PAGE_SIZE.load(Ordering::Relaxed)) {
		return size;
	}

	let size = unsafe { libc::sysconf(libc::_SC_PAGE_SIZE) };

	if size < 0 {
		#[cfg(target_os = "linux")]
		let errno = unsafe { *libc::__errno_location() };
		#[cfg(target_os = "macos")]
		let errno = unsafe { *libc::__error() };
		panic!("failed to get page size with error code: {errno}");
	}

	let size = size
		.try_into()
		.ok()
		.and_then(NonZeroUsize::new)
		.expect("invalid page size");

	PAGE_SIZE.store(size.get(), Ordering::Relaxed);

	size
}

pub(crate) struct Mmap {
	mapping: NonNull<[u8]>,
}

bitflags! {
	#[derive(Debug, Default)]
	pub(crate) struct MmapFlags: u8 {
		const SHARED = 1 << 0;
		const POPULATE = 1 << 1;
	}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum MmapProtect {
	#[default]
	None,
	Read,
	ReadWrite,
	ReadWriteExec,
}

impl MmapProtect {
	fn into_flags(self) -> libc::c_int {
		match self {
			| MmapProtect::None => libc::PROT_NONE,
			| MmapProtect::Read => libc::PROT_READ,
			| MmapProtect::ReadWrite => libc::PROT_READ | libc::PROT_WRITE,
			| MmapProtect::ReadWriteExec => libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC,
		}
	}
}

impl Mmap {
	pub(crate) fn new_options(
		size: usize,
		protect: MmapProtect,
		flags: impl Into<Option<MmapFlags>>,
		file: impl Into<Option<(RawFd, libc::off_t)>>,
	) -> SysResult<Self> {
		let flags = flags.into().unwrap_or_default();
		let file = file.into();
		let (fd, offset) = file.unwrap_or((-1, 0));
		let mut c_flags: libc::c_int = 0;

		if flags.contains(MmapFlags::SHARED) {
			#[cfg(target_os = "linux")]
			{
				c_flags |= libc::MAP_SHARED_VALIDATE;
			}
			#[cfg(not(target_os = "linux"))]
			{
				c_flags |= libc::MAP_SHARED;
			}
		} else {
			c_flags |= libc::MAP_PRIVATE;
		}

		if flags.contains(MmapFlags::POPULATE) {
			c_flags |= libc::MAP_POPULATE;
		}

		if file.is_none() {
			c_flags |= libc::MAP_ANONYMOUS;
		}

		let mapping = unsafe { libc::mmap(null_mut(), size, protect.into_flags(), c_flags, fd, offset) };

		if mapping == libc::MAP_FAILED {
			return Err(SysError::last_os_error());
		}

		let mapping = match NonNull::new(mapping) {
			| Some(x) => x,
			| None => {
				unsafe { libc::munmap(mapping, size) };
				return Err(SysError::from_errno(libc::ERANGE));
			}
		};

		let mapping = NonNull::slice_from_raw_parts(mapping.cast(), size);

		Ok(Self { mapping })
	}

	pub(crate) fn new(size: usize) -> SysResult<Self> { Self::new_options(size, MmapProtect::ReadWrite, None, None) }

	pub(crate) const fn into_non_null(self) -> NonNull<[u8]> {
		let mapping = self.mapping;
		forget(self);
		mapping
	}

	pub(crate) const unsafe fn from_non_null(mapping: NonNull<[u8]>) -> Self { Self { mapping } }

	pub(crate) const fn as_non_null(&self) -> NonNull<[u8]> { self.mapping }

	pub(crate) const fn as_non_null_ptr(&self) -> NonNull<u8> {
		unsafe { NonNull::new_unchecked(addr_of_mut!((*self.as_non_null().as_ptr())[0])) }
	}

	pub(crate) const fn len(&self) -> usize { self.mapping.len() }
}

impl Drop for Mmap {
	fn drop(&mut self) {
		unsafe { libc::munmap(addr_of_mut!((*self.mapping.as_ptr())[0]).cast(), self.mapping.len()) };
	}
}
