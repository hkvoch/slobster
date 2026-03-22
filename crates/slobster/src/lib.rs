#![cfg_attr(all(not(feature = "std"), not(miri)), no_std)]

pub(crate) mod pointer;
pub mod slab;
mod sys;
pub(crate) mod utils;
