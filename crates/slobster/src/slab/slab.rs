use core::alloc::Layout;
use core::cell::Cell;
use core::fmt;
use core::num::NonZeroUsize;
use core::ptr::{NonNull, addr_of_mut};

use crate::slab::slot::PSlot;
use crate::slab::{SlabError, Slots};
use crate::utils::debug_unwrap;

#[repr(C)]
struct Slab<T> {
	header: SlabHeader<T>,
	slots: Slots<T>,
}

struct SlabHeader<T> {
	next: Cell<Option<PSlab<T>>>,
	free: Cell<Option<PSlot<T>>>,
}

pub(super) struct PSlab<T> {
	ptr: NonNull<SlabHeader<T>>,
}

impl<T> fmt::Debug for PSlab<T> {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { fmt::Debug::fmt(&self.ptr, f) }
}

impl<T> Copy for PSlab<T> {}

impl<T> Clone for PSlab<T> {
	fn clone(&self) -> Self { *self }
}

impl<T> PSlab<T> {
	pub(super) const fn header_layout() -> Result<Layout, SlabError> {
		match Layout::new::<SlabHeader<T>>().align_to(PSlot::<T>::layout().align()) {
			| Ok(layout) => Ok(layout.pad_to_align()),
			| Err(_) => Err(SlabError),
		}
	}

	pub(super) unsafe fn new(ptr: NonNull<()>) -> Self { Self { ptr: ptr.cast() } }

	pub(super) unsafe fn init(ptr: NonNull<()>, capacity: NonZeroUsize, next: impl Into<Option<PSlab<T>>>) -> Self {
		let p_slab_h = ptr.cast::<SlabHeader<T>>();
		let p_slab = debug_unwrap!(NonNull::new(
			core::ptr::slice_from_raw_parts_mut(p_slab_h.as_ptr(), capacity.get()) as *mut Slab<T>
		));
		let last = Slots::init(debug_unwrap!(NonNull::new(unsafe {
			addr_of_mut!((*p_slab.as_ptr()).slots)
		})));

		let header = SlabHeader::<T> {
			free: Cell::new(last),
			next: Cell::new(next.into()),
		};

		unsafe {
			addr_of_mut!((*p_slab.as_ptr()).header).write(header);
		}

		unsafe { PSlab::new(ptr) }
	}

	fn header(&self) -> &SlabHeader<T> { unsafe { self.ptr.as_ref() } }

	pub(super) fn is_same(self, other: Self) -> bool { self.ptr == other.ptr }

	pub(super) fn next(self) -> Option<PSlab<T>> { self.header().next.get() }

	pub(super) fn take_next(self) -> Option<PSlab<T>> { self.header().next.take() }

	pub(super) fn set_next(self, next: impl Into<Option<PSlab<T>>>) { self.header().next.set(next.into()); }

	pub(super) fn free(self) -> Option<PSlot<T>> { self.header().free.get() }

	pub(super) fn set_free(self, free: impl Into<Option<PSlot<T>>>) { self.header().free.set(free.into()); }

	pub(super) fn ptr(self) -> NonNull<u8> { self.ptr.cast() }
}

#[derive(Debug, Clone, Copy)]
pub(super) struct SlabMask(NonZeroUsize);

impl SlabMask {
	pub(super) const fn from_len(len: NonZeroUsize) -> Result<Self, SlabError> {
		let len = len.get();
		if !len.is_power_of_two() || len == 0 {
			return Err(SlabError);
		}

		let Some(len) = len.checked_sub(1) else {
			return Err(SlabError);
		};
		match NonZeroUsize::new(!len) {
			| Some(mask) => Ok(Self(mask)),
			| None => Err(SlabError),
		}
	}

	pub(super) const fn to_usize(self) -> usize { self.0.get() }

	pub(super) const fn apply(self, addr: NonZeroUsize) -> NonZeroUsize {
		debug_unwrap!(NonZeroUsize::new(addr.get() & self.to_usize()))
	}
}
