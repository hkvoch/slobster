mod slab;
mod slabbed;
mod slot;

use core::alloc::Layout;
use core::cell::Cell;
use core::error::Error;
use core::fmt;
use core::mem::MaybeUninit;
use core::num::NonZeroUsize;
use core::ptr::NonNull;

use likely_stable::unlikely;

use crate::sys::{get_page_size, mmap, munmap};
use crate::utils::debug_unwrap;
use slab::*;
pub use slabbed::*;
use slot::*;

#[derive(Clone, Copy, PartialEq)]
pub struct SlabError;

impl fmt::Debug for SlabError {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { write!(f, "slab allocation failed") }
}

impl fmt::Display for SlabError {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { write!(f, "slab allocation failed") }
}

impl Error for SlabError {}

#[derive(Debug)]
pub struct SlabAllocatorOptions {
	pub pages_per_slab: NonZeroUsize,
}

impl SlabAllocatorOptions {
	pub const DEFAULT: Self = Self {
		pages_per_slab: NonZeroUsize::new(4).unwrap(),
	};
}

/// SlabAllocator
pub struct SlabAllocator<T> {
	free: Cell<Option<PSlot<T>>>,
	full: Cell<Option<PSlab<T>>>,
	slab_capacity: NonZeroUsize,
	slab_len: NonZeroUsize,
	slab_mask: SlabMask,
	slab_alloc: NonZeroUsize,
}

impl<T> fmt::Debug for SlabAllocator<T> {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("SlabAllocator")
			.field("free", &self.free.get())
			.field("full", &self.full.get())
			.field("slab_capacity", &self.slab_capacity)
			.field("slab_len", &self.slab_len)
			.field("slab_mask", &self.slab_mask)
			.field("slab_alloc", &self.slab_alloc)
			.finish()
	}
}

impl<T> Drop for SlabAllocator<T> {
	fn drop(&mut self) {
		#[cfg(miri)]
		let mut to_drop = Vec::new();

		let mut maybe_slab = self.free.take().map(|x| self.slab_of(x)).or_else(|| self.full.take());
		while let Some(slab) = maybe_slab {
			match slab.next() {
				| Some(next) => maybe_slab = Some(next),
				| None => {
					maybe_slab = self.full.take();
				}
			}

			#[cfg(not(miri))]
			unsafe {
				munmap(slab.ptr(), self.slab_len, Some(self.slab_len));
			}

			#[cfg(miri)]
			if slab.ptr().addr().get().is_multiple_of(self.slab_alloc.get()) {
				to_drop.push(slab);
			}
		}

		#[cfg(miri)]
		for slab in to_drop {
			unsafe {
				munmap(slab.ptr(), self.slab_alloc, self.align_hint());
			}
		}
	}
}

impl<T> SlabAllocator<T> {
	#[inline]
	pub fn new<O>(options: O) -> Result<Self, SlabError>
	where O: Into<Option<SlabAllocatorOptions>> {
		let options = match options.into() {
			| Some(x) => x,
			| None => SlabAllocatorOptions::DEFAULT,
		};
		if !options.pages_per_slab.get().is_multiple_of(2) {
			return Err(SlabError);
		}

		let page_size = get_page_size().get();

		let slab_len = page_size
			.checked_mul(options.pages_per_slab.get())
			.and_then(NonZeroUsize::new)
			.ok_or(SlabError)?;

		let header_layout = PSlab::<T>::header_layout()?;
		if !page_size.is_multiple_of(header_layout.align()) {
			return Err(SlabError);
		}

		let slab_capacity = slab_len
			.get()
			.checked_sub(header_layout.size())
			.and_then(|x| x.checked_div(PSlot::<T>::size().get()))
			.and_then(NonZeroUsize::new)
			.ok_or(SlabError)?;

		let slab_mask = SlabMask::from_len(slab_len)?;

		let slab_alloc = slab_len
			.get()
			.checked_mul(2)
			.and_then(NonZeroUsize::new)
			.ok_or(SlabError)?;

		Ok(Self {
			free: Cell::new(None),
			full: Cell::new(None),
			slab_capacity,
			slab_len,
			slab_alloc,
			slab_mask,
		})
	}

	#[inline]
	pub fn emplace<U>(&self, value: U) -> Slabbed<'_, T>
	where U: Into<T> {
		Self::must_alloc(self.try_emplace(value))
	}

	#[inline]
	pub fn try_emplace<U>(&self, value: U) -> Result<Slabbed<'_, T>, SlabError>
	where U: Into<T> {
		self.try_init(|slot| slot.write(value.into()))
	}

	#[inline]
	pub fn init<F>(&self, ctor: F) -> Slabbed<'_, T>
	where F: FnOnce(&mut MaybeUninit<T>) -> &mut T {
		Self::must_alloc(self.try_init(ctor))
	}

	#[inline]
	pub fn try_init<F>(&self, ctor: F) -> Result<Slabbed<'_, T>, SlabError>
	where F: FnOnce(&mut MaybeUninit<T>) -> &mut T {
		let ptr = self.try_alloc()?;
		ctor(unsafe { ptr.cast().as_mut() });
		Ok(unsafe { Slabbed::from_non_null(ptr, self) })
	}

	#[inline]
	pub fn alloc(&self) -> NonNull<T> { Self::must_alloc(self.try_alloc()) }

	#[inline]
	pub fn try_alloc(&self) -> Result<NonNull<T>, SlabError> {
		let Some(free) = self.free.get() else {
			return self.alloc_slow();
		};

		let next_free = free.vacant();
		self.free.set(next_free);
		if unlikely(next_free.is_none()) {
			self.shift_freelist(free);
		}

		Ok(free.object())
	}

	fn shift_freelist(&self, last: PSlot<T>) {
		let slab = self.slab_of(last);
		let next_free_slab = slab.next();

		slab.set_free(None);
		slab.set_next(self.full.get());
		self.full.set(Some(slab));

		if let Some(next_free_slab) = next_free_slab {
			self.free.set(next_free_slab.free());
		}
	}

	#[cold]
	fn alloc_slow(&self) -> Result<NonNull<T>, SlabError> {
		let slab = match self.reuse_slab() {
			| Some(slab) => slab,
			| None => self.add_slab()?,
		};
		let alloc = debug_unwrap!(slab.free());
		self.free.set(alloc.vacant());

		Ok(alloc.object())
	}

	#[inline]
	pub unsafe fn free_unchecked(&self, slot: NonNull<T>) {
		let slot = unsafe { PSlot::new(slot.cast()) };

		let Some(free) = self.free.get() else {
			self.free.set(Some(slot));
			return;
		};

		if unlikely(!self.slab_of(slot).is_same(self.slab_of(free))) {
			return self.free_slow(slot);
		}

		slot.vacate(free);
		self.free.set(Some(slot));
	}

	#[cold]
	fn free_slow(&self, slot: PSlot<T>) {
		let slab = self.slab_of(slot);
		let prev_free = slab.free();
		slab.set_free(slot);
		slot.vacate(prev_free);
	}

	fn reuse_slab(&self) -> Option<PSlab<T>> {
		let mut maybe_slab = self.full.get();
		let mut prev_slab: Option<PSlab<T>> = None;

		while let Some(slab) = maybe_slab {
			if slab.free().is_some() {
				let next_slab = slab.take_next();

				self.free.set(slab.free());

				if let Some(prev_slab) = prev_slab {
					prev_slab.set_next(next_slab);
				} else {
					self.full.set(next_slab);
				}

				return Some(slab);
			}

			prev_slab = Some(slab);
			maybe_slab = slab.next();
		}

		None
	}

	fn add_slab(&self) -> Result<PSlab<T>, SlabError> { self.add_slab_impl().map(|x| x.1) }

	fn add_slab_impl(&self) -> Result<(Option<PSlab<T>>, PSlab<T>), SlabError> {
		let mapping = mmap(self.slab_alloc, self.align_hint()).ok_or(SlabError)?;
		debug_assert!(
			mapping.addr().get().is_multiple_of(get_page_size().get()),
			"slab allocator assumes wrong page size or the pages are misaligned",
		);

		let slab_len = self.slab_len.get();
		let aligned_offset = NonZeroUsize::new(mapping.align_offset(slab_len));

		match aligned_offset {
			| None => {
				let fst = self.map_slab(mapping);
				let snd = self.map_slab(debug_unwrap!(NonNull::new(mapping.as_ptr().wrapping_add(slab_len))));
				Ok((Some(fst), snd))
			}
			| Some(aligned_offset) => {
				let slab = self.map_slab(debug_unwrap!(NonNull::new(
					mapping.as_ptr().wrapping_add(aligned_offset.get())
				)));
				unsafe {
					munmap(mapping, aligned_offset, None);
				}
				if let Some(end_len) = slab_len.checked_sub(aligned_offset.get()).and_then(NonZeroUsize::new) {
					unsafe {
						munmap(
							debug_unwrap!(NonNull::new(
								mapping
									.as_ptr()
									.wrapping_add(aligned_offset.get())
									.wrapping_add(slab_len),
							)),
							end_len,
							None,
						);
					}
				}
				Ok((None, slab))
			}
		}
	}

	fn map_slab(&self, slab: NonNull<u8>) -> PSlab<T> {
		let slab = unsafe {
			PSlab::init(
				slab.cast(),
				self.slab_capacity,
				self.free.get().map(|x| self.slab_of(x)),
			)
		};

		self.free.set(slab.free());
		slab
	}

	fn must_alloc<U>(result: Result<U, SlabError>) -> U {
		match result {
			| Ok(x) => x,
			#[cfg(feature = "std")]
			| Err(SlabError) => std::alloc::handle_alloc_error(Layout::new::<T>()),
			#[cfg(not(feature = "std"))]
			| Err(SlabError) => panic!("allocation failed"),
		}
	}

	fn slab_of(&self, slot: PSlot<T>) -> PSlab<T> { slot.header(self.slab_mask) }

	#[cfg(miri)]
	const fn align_hint(&self) -> Option<NonZeroUsize> { Some(self.slab_alloc) }

	#[cfg(not(miri))]
	const fn align_hint(&self) -> Option<NonZeroUsize> { Some(self.slab_len) }
}

#[cfg(test)]
mod test {
	use rstest::rstest;

	use crate::slab::PSlot;

	use super::SlabAllocator;

	// cfg_if! {
	// 	if #[cfg(target_pointer_width = "64")] {
	// 		#[rstest]
	// 		#[case(0_u8, 8190)]
	// 		#[case(0_u16, 8190)]
	// 		#[case(0_u32, 8190)]
	// 		#[case(0_u64, 8190)]
	// 		#[case(0_u128, 4095)]
	// 		fn derived_values<T>(#[case] x: T, #[case] expected_cap: usize) {
	// 			derived_values_impl(x, expected_cap);
	// 		}
	// 	} else {
	// 		compile_error!("unsupported target pointer width");
	// 	}
	// }

	// fn derived_values_impl<T>(_x: T, expected_cap: usize) {
	// 	let alloc = SlabAllocator::<T>::new(None).unwrap();
	// 	assert_eq!(alloc.slab_capacity.get(), expected_cap);
	// }

	// #[rstest]
	// fn new_slab_correct_freelist() {
	// 	let alloc = SlabAllocator::<i32>::new(None).unwrap();
	// 	alloc.add_slab().unwrap();

	// 	let free_slot = alloc.slab_of(alloc.free.get().unwrap());
	// 	let slab = unsafe { alloc.cast_slab_ptr(free_slot.ptr).as_ref() };
	// 	let mut count = 0_usize;
	// 	let mut free_it = slab.header.free.get();
	// 	while let Some(free) = free_it {
	// 		assert!(core::ptr::addr_eq(free.object().as_ptr(), &slab.slots[count]));
	// 		count += 1;
	// 		free_it = free.vacant();
	// 	}

	// 	cfg_if! {
	// 		if #[cfg(target_pointer_width = "64")] {
	// 			assert_eq!(count, 8190);
	// 		} else {
	// 			compile_error!("unsupported pointer width");
	// 		}
	// 	}
	// }

	#[rstest]
	fn add_slab_freelist() {
		let alloc = SlabAllocator::<i32>::new(None).unwrap();
		let add1 = alloc.add_slab_impl().unwrap();
		let add2 = alloc.add_slab_impl().unwrap();
		let add3 = alloc.add_slab_impl().unwrap();

		let (s1, s2, s3) = match (add1, add2, add3) {
			| (_, (_, s1), (Some(s2), s3)) => (s1, s2, s3),
			| (_, (Some(s1), s2), (None, s3)) => (s1, s2, s3),
			| ((_, s1), (None, s2), (None, s3)) => (s1, s2, s3),
		};

		eprintln!("s1 = {s1:x?}");
		eprintln!("s2 = {s2:x?}");
		eprintln!("s3 = {s3:x?}");

		assert_eq!(s3.ptr(), alloc.slab_of(alloc.free.get().unwrap()).ptr());

		let f1 = alloc.slab_of(alloc.free.get().unwrap());
		assert_eq!(s3.ptr(), f1.ptr());

		let f2 = f1.next().unwrap();
		assert_eq!(s2.ptr(), f2.ptr());

		let f3 = f2.next().unwrap();
		assert_eq!(s1.ptr(), f3.ptr());
	}

	#[rstest]
	fn simple_alloc() {
		let alloc = SlabAllocator::<i32>::new(None).unwrap();
		let (_, s1) = alloc.add_slab_impl().unwrap();
		let slot = unsafe { PSlot::new(alloc.alloc().cast()) };
		assert_eq!(alloc.slab_of(slot).ptr(), s1.ptr());
		unsafe {
			alloc.free_unchecked(slot.object());
			assert_eq!(s1.free().unwrap().ptr(), slot.ptr());
		}
	}
}
