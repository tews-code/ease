//! Schedulers

#![allow(unused_imports)]

mod coop_rr;

#[cfg(feature = "sched-coop-rr")]
pub use coop_rr::{bootstrap, preempt_into, sleep, sleep_until, spawn, yield_now};
