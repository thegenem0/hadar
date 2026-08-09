//! Hadar server

mod config;
mod server;

#[doc(inline)]
pub use crate::config::{Config, Paths};

#[doc(inline)]
pub use crate::server::run;
