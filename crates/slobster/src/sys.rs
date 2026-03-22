#[cfg(miri)]
mod miri;

#[cfg(all(unix, not(miri)))]
mod unix;

#[cfg(miri)]
pub(crate) use miri::*;
#[cfg(all(unix, not(miri)))]
pub(crate) use unix::*;
