//! System Calls

pub const EXIT: usize = 0;
pub const PUT_CHAR: usize = 1;
pub const GET_CHAR: usize = 2;
/// Test-only: park the calling user thread in kernel space forever, so sched
/// tests can manufacture a Blocked user thread (no real syscall blocks yet).
#[cfg(all(test, feature = "test-sched"))]
pub const TEST_BLOCK: usize = 3;
