//! Schedulers

#![allow(unused_imports)]

mod roundrobin;

#[cfg(feature = "sched-rr")]
pub use roundrobin::{
    PRIORITY_DEFAULT,
    StackClass,
    bootstrap,
    get_current_cycles,
    idle_thread,
    preempt_into,
    sleep,
    sleep_until,
    spawn,
    // stack_ok_panic,
    yield_now,
};
