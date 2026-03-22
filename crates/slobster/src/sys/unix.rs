use core::num::NonZeroUsize;
use core::ptr;
use core::ptr::NonNull;
use core::sync::atomic::{AtomicUsize, Ordering};

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
	if !size.is_power_of_two() {
		panic!("unsupported page size: {size}");
	}

	PAGE_SIZE.store(size.get(), Ordering::Relaxed);

	size
}

pub(crate) fn mmap(len: NonZeroUsize, align_hint: Option<NonZeroUsize>) -> Option<NonNull<u8>> {
	let _align_hint = align_hint;
	debug_assert!(len.get().is_multiple_of(get_page_size().get()), "invalid mmap size");

	let ret = unsafe {
		libc::mmap(
			ptr::null_mut(),
			len.get(),
			libc::PROT_READ | libc::PROT_WRITE,
			libc::MAP_ANON | libc::MAP_PRIVATE,
			-1,
			0,
		)
	};

	if ret == libc::MAP_FAILED {
		return None;
	}

	NonNull::new(ret.cast::<u8>())
}

pub(crate) unsafe fn munmap(addr: NonNull<u8>, len: NonZeroUsize, align_hint: Option<NonZeroUsize>) {
	let _align_hint = align_hint;
	debug_assert!(
		addr.addr().get().is_multiple_of(get_page_size().get()),
		"invalid unmap address",
	);
	debug_assert!(len.get().is_multiple_of(get_page_size().get()), "invalid unmap length");

	let ret = unsafe { libc::munmap(addr.cast().as_ptr(), len.get()) };
	assert!(ret == 0, "failed to unmap memory region");
}
