use core::ptr::{NonNull, addr_of_mut};

#[derive(Debug)]
pub(crate) struct PtrIterMut<T> {
	ptr: NonNull<[T]>,
}

impl<T> PtrIterMut<T> {
	pub(crate) const fn new(ptr: NonNull<[T]>) -> Self { Self { ptr } }

	pub(crate) const fn new_ptr(ptr: *mut [T]) -> Option<Self> {
		match NonNull::new(ptr) {
			| Some(x) => Some(Self::new(x)),
			| None => None,
		}
	}

	fn ptr_first(&self) -> Option<NonNull<T>> {
		if self.ptr.is_empty() {
			return None;
		}

		let ptr = unsafe { addr_of_mut!((*self.ptr.as_ptr())[0]) };
		let ptr = unsafe { NonNull::new_unchecked(ptr) };
		Some(ptr)
	}

	fn move_front(&mut self) {
		debug_assert!(!self.ptr.is_empty());

		self.ptr = unsafe {
			NonNull::new_unchecked(core::ptr::slice_from_raw_parts_mut(
				addr_of_mut!((*self.ptr.as_ptr())[0]).wrapping_add(1),
				self.ptr.len() - 1,
			))
		};
	}

	fn ptr_last(&self) -> Option<NonNull<T>> {
		if self.ptr.is_empty() {
			return None;
		}

		let ptr = unsafe { addr_of_mut!((*self.ptr.as_ptr())[self.ptr.len() - 1]) };
		let ptr = unsafe { NonNull::new_unchecked(ptr) };

		Some(ptr)
	}

	fn move_back(&mut self) {
		debug_assert!(!self.ptr.is_empty());

		self.ptr = unsafe {
			NonNull::new_unchecked(core::ptr::slice_from_raw_parts_mut(
				addr_of_mut!((*self.ptr.as_ptr())[0]),
				self.ptr.len() - 1,
			))
		};
	}
}

impl<T> Iterator for PtrIterMut<T> {
	type Item = NonNull<T>;

	fn next(&mut self) -> Option<Self::Item> {
		let ptr = self.ptr_first()?;
		self.move_front();
		Some(ptr)
	}

	fn size_hint(&self) -> (usize, Option<usize>) { (self.ptr.len(), Some(self.ptr.len())) }
}

impl<T> ExactSizeIterator for PtrIterMut<T> {
	fn len(&self) -> usize { self.ptr.len() }
}

impl<T> DoubleEndedIterator for PtrIterMut<T> {
	fn next_back(&mut self) -> Option<Self::Item> {
		let ptr = self.ptr_last()?;
		self.move_back();
		Some(ptr)
	}
}
