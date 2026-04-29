//! Schedulers

mod coop_rr;

#[cfg(feature = "sched-coop-rr")]
pub use coop_rr::{bootstrap, spawn, yield_now};
