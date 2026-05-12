//! Schedulers

#![allow(unused_imports)]

mod roundrobin;

#[cfg(feature = "sched-rr")]
pub use roundrobin::{
    PRIORITY_DEFAULT,
    Qos,
    StackClass,
    bootstrap,
    get_current_cycles,
    idle_thread,
    // stack_ok_panic,
    post_switch_cleanup,
    preempt,
    sleep,
    sleep_until,
    sleep_with_leeway,
    spawn,
    yield_now,
};
