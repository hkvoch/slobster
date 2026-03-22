use core::borrow::{Borrow, BorrowMut};
use core::cmp::Ordering;
use core::hash::{Hash, Hasher};
use core::ops::{Deref, DerefMut};
use core::ptr::{NonNull, drop_in_place};
use core::{fmt, mem};

use crate::slab::SlabAllocator;

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

	pub const fn as_ptr(this: &Self) -> *const T { this.ptr.as_ptr() }

	pub const fn as_mut_ptr(this: &mut Self) -> *mut T { this.ptr.as_ptr() }

	pub const fn as_non_null(this: &Self) -> NonNull<T> { this.ptr }
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

impl<'alloc, T> PartialEq for Slabbed<'alloc, T>
where T: PartialEq
{
	fn eq(&self, other: &Self) -> bool { Self::inner(self).eq(other) }
}

impl<'alloc, T> Eq for Slabbed<'alloc, T> where T: Eq {}

impl<'alloc, T> PartialOrd for Slabbed<'alloc, T>
where T: PartialOrd
{
	fn partial_cmp(&self, other: &Self) -> Option<Ordering> { Self::inner(self).partial_cmp(other) }
}

impl<'alloc, T> Ord for Slabbed<'alloc, T>
where T: Ord
{
	fn cmp(&self, other: &Self) -> Ordering { Self::inner(self).cmp(other) }
}

impl<'alloc, T> Hash for Slabbed<'alloc, T>
where T: Hash
{
	fn hash<H: Hasher>(&self, state: &mut H) { Self::inner(self).hash(state); }
}

impl<'alloc, T> Iterator for Slabbed<'alloc, T>
where T: Iterator
{
	type Item = T::Item;

	fn next(&mut self) -> Option<Self::Item> { Self::inner_mut(self).next() }

	fn size_hint(&self) -> (usize, Option<usize>) { Self::inner(self).size_hint() }
}

impl<'alloc, T> DoubleEndedIterator for Slabbed<'alloc, T>
where T: DoubleEndedIterator
{
	fn next_back(&mut self) -> Option<Self::Item> { Self::inner_mut(self).next_back() }
}

impl<'alloc, T> ExactSizeIterator for Slabbed<'alloc, T>
where T: ExactSizeIterator
{
	fn len(&self) -> usize { Self::inner(self).len() }
}

impl<'alloc, T> fmt::Pointer for Slabbed<'alloc, T> {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { write!(f, "{:p}", self.ptr) }
}

impl<'alloc, T> AsRef<T> for Slabbed<'alloc, T> {
	fn as_ref(&self) -> &T { Self::inner(self) }
}

impl<'alloc, T> AsMut<T> for Slabbed<'alloc, T> {
	fn as_mut(&mut self) -> &mut T { Self::inner_mut(self) }
}

impl<'alloc, T> Borrow<T> for Slabbed<'alloc, T> {
	fn borrow(&self) -> &T { Self::inner(self) }
}

impl<'alloc, T> BorrowMut<T> for Slabbed<'alloc, T> {
	fn borrow_mut(&mut self) -> &mut T { Self::inner_mut(self) }
}

impl<'alloc, T> Unpin for Slabbed<'alloc, T> {}
