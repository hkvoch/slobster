#[cfg(target_os = "linux")]
pub mod linux;

mod error;
#[cfg(unix)]
mod unix;

pub use error::*;
#[cfg(target_os = "linux")]
pub use linux::*;
#[cfg(unix)]
pub use unix::*;
