//! Trace thread lifecycles

//      | 0                         | 1             | 2         |..         |30     |31                 |
//      +---------------------------+---------------+-----------+-----------+-------+-------------------+
// 1 ms |State::Running             | State: Ready  | State: Sleep(Deadline(50))    | State: Running    |
//      |Hart: 0                    | Hart: 0       | Hart: 0                       | Hart: 1           |
//      +-----------------------------------------------------------------------------------------------+
// 2 ms |
// 3 ms |
// ...
// 16 ms| State: Postswitch(Ready)  | State: Running | State: Sleep(Deadline(50))   |State: Running     |
// ...
// 50ms | State: Avail              | State: Running | State: Postswitch(Ready)     | State: Running    |

use crate::kernel::sched::State;
use crate::kernel::sched::THREADS_MAX;
use crate::kernel::timer;

const TRACE_BUFFER_SIZE: usize = 1024;

struct TracePoint {
    state: State,
    hart: u8,
}

struct ThreadsSnapshot {
    time_stamp: u64,
    threads: [TracePoint; THREADS_MAX],
}

impl ThreadsSnapshot {
    const fn new() -> Self {
        Self {
            time_stamp: 0,
            threads: [const {
                TracePoint {
                    state: State::Avail,
                    hart: 0,
                }
            }; THREADS_MAX],
        }
    }
}
// Store data in a static ring buffer that overwrites - no locking, lightweight, fixed size
static TRACE_BUF: [ThreadsSnapshot; TRACE_BUFFER_SIZE] =
    [const { ThreadsSnapshot::new() }; TRACE_BUFFER_SIZE];

fn take_snapshot() {
    // Get the time
}
