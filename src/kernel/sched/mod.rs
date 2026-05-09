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
    // stack_ok_panic,
    post_switch_cleanup,
    preempt,
    sleep,
    sleep_until,
    spawn,
    yield_now,
};
