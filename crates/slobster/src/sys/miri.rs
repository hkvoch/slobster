use core::num::NonZeroUsize;
use core::ptr::NonNull;
use std::alloc::{self, Layout};

pub(crate) fn get_page_size() -> NonZeroUsize { NonZeroUsize::new(16384).unwrap() }

pub(crate) fn mmap(len: NonZeroUsize, align_hint: Option<NonZeroUsize>) -> Option<NonNull<u8>> {
	let align_hint = align_hint?.get();
	let layout = Layout::new::<u8>()
		.repeat(len.get())
		.ok()?
		.0
		.align_to(align_hint)
		.ok()?;
	NonNull::new(unsafe { alloc::alloc(layout) })
}

pub(crate) unsafe fn munmap(addr: NonNull<u8>, len: NonZeroUsize, align_hint: Option<NonZeroUsize>) {
	let align_hint = align_hint.unwrap().get();
	let layout = Layout::new::<u8>()
		.repeat(len.get())
		.unwrap()
		.0
		.align_to(align_hint)
		.unwrap();
	unsafe {
		alloc::dealloc(addr.as_ptr(), layout);
	}
}
