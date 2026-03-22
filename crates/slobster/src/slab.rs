use core::alloc::Layout;
use core::cell::Cell;
use core::error::Error;
use core::marker::PhantomPinned;
use core::mem::{ManuallyDrop, MaybeUninit};
use core::num::NonZeroUsize;
use core::ops::{Deref, DerefMut};
use core::ptr::{NonNull, addr_of_mut, drop_in_place};
use core::{fmt, mem};

use likely_stable::unlikely;

use crate::pointer::PtrIterMut;
use crate::sys::{get_page_size, mmap, munmap};
use crate::utils::debug_unwrap;

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

pub struct SlabAllocator<T> {
	free: Cell<Option<PSlot<T>>>,
	full: Cell<Option<PSlab<T>>>,
	slab_capacity: NonZeroUsize,
	slab_len: NonZeroUsize,
	slab_mask: NonZeroUsize,
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
			match slab.next.get() {
				| Some(next) => maybe_slab = Some(next),
				| None => {
					maybe_slab = self.full.take();
				}
			}

			#[cfg(not(miri))]
			unsafe {
				munmap(slab.ptr.cast(), self.slab_len, Some(self.slab_len));
			}

			#[cfg(miri)]
			if slab.ptr.addr().get().is_multiple_of(self.slab_alloc.get()) {
				to_drop.push(slab);
			}
		}

		#[cfg(miri)]
		for slab in to_drop {
			unsafe {
				munmap(slab.ptr.cast(), self.slab_alloc, self.align_hint());
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

		let slab_len = get_page_size()
			.get()
			.checked_mul(options.pages_per_slab.get())
			.and_then(NonZeroUsize::new)
			.ok_or(SlabError)?;

		let header_layout = Layout::new::<SlabHeader<T>>()
			.align_to(Layout::new::<[Slot<T>; 1]>().align())
			.or(Err(SlabError))?
			.pad_to_align();

		let slab_capacity = slab_len
			.get()
			.checked_sub(header_layout.size())
			.and_then(|x| x.checked_div(size_of::<Slot<T>>()))
			.and_then(NonZeroUsize::new)
			.ok_or(SlabError)?;

		let slab_mask = slab_len
			.get()
			.checked_sub(1)
			.map(|x| !x)
			.and_then(NonZeroUsize::new)
			.ok_or(SlabError)?;

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
		Ok(Slabbed { alloc: self, ptr })
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
		let next_free_slab = slab.next.get();

		slab.free.set(None);
		slab.next.set(self.full.get());
		self.full.set(Some(slab));

		if let Some(next_free_slab) = next_free_slab {
			self.free.set(next_free_slab.free.get());
		}
	}

	#[cold]
	fn alloc_slow(&self) -> Result<NonNull<T>, SlabError> {
		let slab = match self.reuse_slab() {
			| Some(slab) => slab,
			| None => self.add_slab()?,
		};
		let alloc = debug_unwrap!(slab.free.get());
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
		slot.vacate(slab.free.replace(Some(slot)));
	}

	fn reuse_slab(&self) -> Option<PSlab<T>> {
		let mut maybe_slab = self.full.get();
		let mut prev_slab: Option<PSlab<T>> = None;

		while let Some(slab) = maybe_slab {
			if slab.free.get().is_some() {
				let next_slab = slab.next.take();

				self.free.set(slab.free.get());

				if let Some(prev_slab) = prev_slab {
					prev_slab.next.set(next_slab);
				} else {
					self.full.set(next_slab);
				}

				return Some(slab);
			}

			prev_slab = Some(slab);
			maybe_slab = slab.next.get();
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
		let p_slab_h = slab.cast::<SlabHeader<T>>();
		let p_slab = self.cast_slab_ptr(p_slab_h);
		let p_slots = unsafe { addr_of_mut!((*p_slab.as_ptr()).slots) };

		let mut last = None;

		for slot in debug_unwrap!(PtrIterMut::new_ptr(p_slots)).rev() {
			let slot = unsafe {
				slot.write(Slot {
					vacant: ManuallyDrop::new(last),
				});
				PSlot::new(slot)
			};
			last = Some(slot);
		}

		let header = SlabHeader::<T> {
			free: Cell::new(last),
			next: Cell::new(self.free.get().map(|x| self.slab_of(x))),
		};

		unsafe {
			addr_of_mut!((*p_slab.as_ptr()).header).write(header);
		}

		self.free.set(last);

		unsafe { PSlab::new(p_slab_h) }
	}

	fn cast_slab_ptr(&self, slab: NonNull<SlabHeader<T>>) -> NonNull<Slab<T>> {
		debug_unwrap!(NonNull::new(
			core::ptr::slice_from_raw_parts_mut(slab.as_ptr(), self.slab_capacity.get()) as *mut Slab<T>
		))
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

pub struct Slabbed<'alloc, T> {
	ptr: NonNull<T>,
	alloc: &'alloc SlabAllocator<T>,
}

impl<'alloc, T> Slabbed<'alloc, T> {
	pub const fn inner(this: &Self) -> &T { unsafe { this.ptr.as_ref() } }

	pub const fn inner_mut(this: &mut Self) -> &mut T { unsafe { this.ptr.as_mut() } }

	pub const fn leak(this: Self) -> &'static mut T { unsafe { Self::into_non_null(this).as_mut() } }

	pub const fn into_raw(this: Self) -> *mut T { Self::into_non_null(this).as_ptr() }

	pub const fn into_non_null(this: Self) -> NonNull<T> {
		let ptr = this.ptr;
		mem::forget(this);
		ptr
	}

	pub const unsafe fn from_non_null(ptr: NonNull<T>, alloc: &'alloc SlabAllocator<T>) -> Self { Self { ptr, alloc } }

	pub const unsafe fn from_raw(ptr: *mut T, alloc: &'alloc SlabAllocator<T>) -> Option<Self> {
		match NonNull::new(ptr) {
			| Some(ptr) => Some(unsafe { Self::from_non_null(ptr, alloc) }),
			| None => None,
		}
	}
}

impl<'alloc, T> fmt::Debug for Slabbed<'alloc, T>
where T: fmt::Debug
{
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { <T as fmt::Debug>::fmt(Self::inner(self), f) }
}

impl<'alloc, T> Drop for Slabbed<'alloc, T> {
	fn drop(&mut self) {
		unsafe {
			drop_in_place(self.ptr.as_ptr());
			self.alloc.free_unchecked(self.ptr);
		}
	}
}

impl<'alloc, T> Deref for Slabbed<'alloc, T> {
	type Target = T;

	fn deref(&self) -> &Self::Target { Self::inner(self) }
}

impl<'alloc, T> DerefMut for Slabbed<'alloc, T> {
	fn deref_mut(&mut self) -> &mut Self::Target { Self::inner_mut(self) }
}

#[repr(C)]
struct Slab<T> {
	_unpin: PhantomPinned,
	header: SlabHeader<T>,
	slots: [Slot<T>],
}

struct SlabHeader<T> {
	next: Cell<Option<PSlab<T>>>,
	free: Cell<Option<PSlot<T>>>,
}

union Slot<T> {
	_unpin: PhantomPinned,
	vacant: ManuallyDrop<Option<PSlot<T>>>,
	_occupied: ManuallyDrop<MaybeUninit<T>>,
}

struct PSlab<T> {
	ptr: NonNull<SlabHeader<T>>,
}

impl<T> fmt::Debug for PSlab<T> {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { fmt::Debug::fmt(&self.ptr, f) }
}

impl<T> Copy for PSlab<T> {}

impl<T> Clone for PSlab<T> {
	fn clone(&self) -> Self { *self }
}

impl<T> Deref for PSlab<T> {
	type Target = SlabHeader<T>;

	fn deref(&self) -> &Self::Target { unsafe { self.ptr.as_ref() } }
}

impl<T> PSlab<T> {
	unsafe fn new(ptr: NonNull<SlabHeader<T>>) -> Self { Self { ptr } }

	fn is_same(self, other: Self) -> bool { self.ptr == other.ptr }
}

struct PSlot<T> {
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
	unsafe fn new(ptr: NonNull<Slot<T>>) -> Self { Self { ptr } }

	fn header(self, mask: impl Into<usize>) -> PSlab<T> {
		let slab = self
			.ptr
			.map_addr(|slot| debug_unwrap!(NonZeroUsize::new(slot.get() & mask.into())))
			.cast();
		unsafe { PSlab::new(slab) }
	}

	fn vacate(self, next: impl Into<Option<PSlot<T>>>) {
		unsafe {
			self.ptr.write(Slot {
				vacant: ManuallyDrop::new(next.into()),
			});
		}
	}

	fn vacant(self) -> Option<PSlot<T>> {
		// SAFETY: self.ptr is guaranteed to point to a valid Slot<T>
		unsafe { *self.ptr.as_ref().vacant }
	}

	fn object(self) -> NonNull<T> { self.ptr.cast() }
}

#[cfg(test)]
mod test {
	use cfg_if::cfg_if;
	use rstest::rstest;

	use crate::slab::PSlot;

	use super::{SlabAllocator, SlabHeader, Slot};

	cfg_if! {
		if #[cfg(target_pointer_width = "64")] {
			#[rstest]
			#[case(0_u8, 8190)]
			#[case(0_u16, 8190)]
			#[case(0_u32, 8190)]
			#[case(0_u64, 8190)]
			#[case(0_u128, 4095)]
			fn derived_values<T>(#[case] x: T, #[case] expected_cap: usize) {
				derived_values_impl(x, expected_cap);
			}
		} else {
			compile_error!("unsupported target pointer width");
		}
	}

	fn derived_values_impl<T>(_x: T, expected_cap: usize) {
		let alloc = SlabAllocator::<T>::new(None).unwrap();

		eprintln!("slot size: {}", size_of::<Slot<T>>());
		eprintln!("header size: {}", size_of::<SlabHeader<T>>());

		assert_eq!(alloc.slab_capacity.get(), expected_cap);
	}

	#[rstest]
	fn new_slab_correct_freelist() {
		let alloc = SlabAllocator::<i32>::new(None).unwrap();
		alloc.add_slab().unwrap();

		let free_slot = alloc.slab_of(alloc.free.get().unwrap());
		let slab = unsafe { alloc.cast_slab_ptr(free_slot.ptr).as_ref() };
		let mut count = 0_usize;
		let mut free_it = slab.header.free.get();
		while let Some(free) = free_it {
			assert!(core::ptr::addr_eq(free.object().as_ptr(), &slab.slots[count]));
			count += 1;
			free_it = free.vacant();
		}

		cfg_if! {
			if #[cfg(target_pointer_width = "64")] {
				assert_eq!(count, 8190);
			} else {
				compile_error!("unsupported pointer width");
			}
		}
	}

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

		assert_eq!(s3.ptr, alloc.slab_of(alloc.free.get().unwrap()).ptr);

		let f1 = alloc.slab_of(alloc.free.get().unwrap());
		assert_eq!(s3.ptr, f1.ptr);

		let f2 = f1.next.get().unwrap();
		assert_eq!(s2.ptr, f2.ptr);

		let f3 = f2.next.get().unwrap();
		assert_eq!(s1.ptr, f3.ptr);
	}

	#[rstest]
	fn simple_alloc() {
		let mut alloc = SlabAllocator::<i32>::new(None).unwrap();
		let (_, s1) = alloc.add_slab_impl().unwrap();
		let slot = unsafe { PSlot::new(alloc.alloc().cast()) };
		assert_eq!(alloc.slab_of(slot).ptr, s1.ptr);
		unsafe {
			alloc.free_unchecked(slot.object());
			assert_eq!(s1.free.get().unwrap().ptr, slot.ptr);
		}
	}
}
