macro_rules! tl_swap {
	($temp:expr => $tl:expr ; $code:block) => {{
		struct Restore<T>(
			std::mem::MaybeUninit<T>,
			&'static std::thread::LocalKey<std::cell::RefCell<T>>,
		)
		where T: 'static;

		impl<T> Drop for Restore<T>
		where T: 'static
		{
			fn drop(&mut self) {
				let prev = unsafe { std::mem::replace(&mut self.0, std::mem::MaybeUninit::uninit()).assume_init() };
				self.1.replace(prev);
			}
		}

		let tl = &$tl;
		let prev = tl.replace($temp);
		let restore = Restore(std::mem::MaybeUninit::new(prev), tl);

		let ret = $code;

		drop(restore);

		ret
	}};
	(@cell $temp:expr => $tl:expr ; $code:block) => {{
		struct Restore<T>(
			std::mem::MaybeUninit<T>,
			&'static std::thread::LocalKey<std::cell::Cell<T>>,
		)
		where T: 'static;

		impl<T> Drop for Restore<T>
		where T: 'static
		{
			fn drop(&mut self) {
				let prev = unsafe { std::mem::replace(&mut self.0, std::mem::MaybeUninit::uninit()).assume_init() };
				self.1.replace(prev);
			}
		}

		let tl = &$tl;
		let prev = tl.replace($temp);
		let restore = Restore(std::mem::MaybeUninit::new(prev), tl);

		let ret = $code;

		drop(restore);

		ret
	}};
}

macro_rules! size_of_field {
	($typ:ty, $field:ident) => {{
		let ptr = ::core::ptr::null::<$typ>();
		let ptr = unsafe { ::core::ptr::addr_of!((*ptr).$field) };
		$crate::utils::size_of_ptr(ptr)
	}};
}

macro_rules! for_each_field {
	($value:expr, $typ:tt, $($field:ident : $code:block $(,)?)* $(,)?) => {
		#[allow(unreachable_code)]
		#[allow(clippy::diverging_sub_expression)]
		fn _assert_all_fields() {
			let _assert_all_fields: $typ = $typ {
				$($field : todo!(),)*
			};
		}

		let _ty_guard: &$typ = &$value;
		$($code)*
	};
	(@cell_set $value:expr, $typ:tt, $($field:ident = $code:expr),* $(,)?) => {
		let value = $value;
		$crate::utils::for_each_field!(
			value,
			$typ,
			$(
				$field: { value.$field.set($code); }
			)*
		);
	};
}

pub(crate) use {for_each_field, size_of_field, tl_swap};

pub(crate) const fn size_of_ptr<T>(_: *const T) -> usize { size_of::<T>() }
