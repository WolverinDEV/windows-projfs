#![deny(clippy::all)]
#![allow(clippy::blocks_in_conditions)]

mod error;
pub use error::*;

mod source;
pub use source::*;

mod fs;
pub use fs::*;

mod callback_data;
pub(crate) use callback_data::*;

pub(crate) mod aligned_buffer;
pub(crate) mod utils;
