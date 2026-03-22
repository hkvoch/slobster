macro_rules! debug_unwrap {
	($opt_or_err:expr $(,)?) => {{
		let opt_or_err = $opt_or_err;

		#[cfg(debug_assertions)]
		let result = opt_or_err.unwrap();

		#[cfg(not(debug_assertions))]
		#[allow(unused_unsafe)]
		let result = unsafe { opt_or_err.unwrap_unchecked() };

		result
	}};
}

pub(crate) use debug_unwrap;
