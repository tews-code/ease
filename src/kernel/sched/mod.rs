//! Schedulers

#![allow(unused_imports)]

mod coop_rr;

#[cfg(feature = "sched-coop-rr")]
pub use coop_rr::{bootstrap, sleep, sleep_until, spawn, yield_now};
