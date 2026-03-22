pub type SysResult<T> = Result<T, SysError>;

pub struct SysError {
	#[cfg(feature = "std")]
	error: std::io::Error,
	#[cfg(not(feature = "std"))]
	code: i64,
}

#[cfg(feature = "std")]
mod with_std {
	use core::error::Error;
	use core::fmt;
	use std::io;

	use super::SysError;

	impl fmt::Display for SysError {
		fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { fmt::Display::fmt(&self.error, f) }
	}

	impl fmt::Debug for SysError {
		fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { fmt::Debug::fmt(&self.error, f) }
	}

	impl Error for SysError {
		fn source(&self) -> Option<&(dyn Error + 'static)> { self.error.source() }
	}

	impl From<io::Error> for SysError {
		fn from(error: io::Error) -> Self { Self { error } }
	}

	impl From<SysError> for io::Error {
		fn from(value: SysError) -> Self { value.error }
	}

	impl SysError {
		pub(crate) fn from_syscall_error(ret: i64) -> Self {
			Self {
				error: ret
					.checked_neg()
					.and_then(|x| i32::try_from(x).ok())
					.map(io::Error::from_raw_os_error)
					.unwrap_or_else(|| io::Error::other(format!("unrecognised syscall error: {ret}"))),
			}
		}

		pub(crate) fn from_errno(errno: i32) -> Self {
			Self {
				error: io::Error::from_raw_os_error(errno),
			}
		}

		pub(crate) fn last_os_error() -> Self {
			Self {
				error: io::Error::last_os_error(),
			}
		}
	}
}
