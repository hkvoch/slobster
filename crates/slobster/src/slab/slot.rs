use core::alloc::Layout;
use core::fmt;
use core::mem::{ManuallyDrop, MaybeUninit};
use core::num::NonZeroUsize;
use core::ptr::{NonNull, addr_of_mut};

use crate::pointer::PtrIterMut;
use crate::slab::SlabMask;
use crate::slab::slab::PSlab;
use crate::utils::debug_unwrap;

union Slot<T> {
	vacant: Option<PSlot<T>>,
	_occupied: ManuallyDrop<MaybeUninit<T>>,
}

#[repr(transparent)]
pub(super) struct Slots<T>([MaybeUninit<Slot<T>>]);

impl<T> Slots<T> {
	pub(super) fn init(this: NonNull<Slots<T>>) -> Option<PSlot<T>> {
		let p_slots = unsafe { addr_of_mut!((*this.as_ptr()).0) };
		let mut last = None;

		for slot in debug_unwrap!(PtrIterMut::new_ptr(p_slots)).rev() {
			let slot = unsafe { PSlot::<T>::init(slot.cast(), last) };
			last = Some(slot);
		}

		last
	}
}

pub(super) struct PSlot<T> {
	ptr: NonNull<Slot<T>>,
}

impl<T> fmt::Debug for PSlot<T> {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { fmt::Debug::fmt(&self.ptr, f) }
}

impl<T> Copy for PSlot<T> {}

impl<T> Clone for PSlot<T> {
	fn clone(&self) -> Self { *self }
}

impl<T> PSlot<T> {
	pub(super) const fn layout() -> Layout { Layout::new::<Slot<T>>() }

	pub(super) const fn size() -> NonZeroUsize { debug_unwrap!(NonZeroUsize::new(size_of::<Slot<T>>())) }

	pub(super) unsafe fn new(ptr: NonNull<()>) -> Self { Self { ptr: ptr.cast() } }

	pub(super) unsafe fn init(ptr: NonNull<()>, vacant: impl Into<Option<Self>>) -> Self {
		let ptr = ptr.cast::<Slot<T>>();
		unsafe {
			ptr.write(Slot { vacant: vacant.into() });
		}
		Self { ptr }
	}

	pub(super) fn header(self, mask: SlabMask) -> PSlab<T> {
		let slab = self.ptr.map_addr(|slot| mask.apply(slot)).cast();
		unsafe { PSlab::new(slab) }
	}

	pub(super) fn vacate(self, next: impl Into<Option<PSlot<T>>>) {
		unsafe {
			self.ptr.write(Slot { vacant: next.into() });
		}
	}

	pub(super) fn vacant(self) -> Option<PSlot<T>> {
		// SAFETY: self.ptr is guaranteed to point to a valid Slot<T>
		unsafe { self.ptr.as_ref().vacant }
	}

	pub(super) fn object(self) -> NonNull<T> { self.ptr.cast() }

	#[cfg(test)]
	pub(super) fn ptr(self) -> NonNull<()> { self.ptr.cast() }
}
