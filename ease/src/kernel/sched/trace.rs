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

use core::cell::UnsafeCell;
use core::fmt::{self, Write};
use core::sync::atomic::{AtomicUsize, Ordering};

use super::ExitReason;
use super::deadline::Deadline;
use super::stride::{PRIORITY_MIN, SchedInner};
use super::threads::{PostSwitch, ThreadControlBlock};
use crate::arch::hart_id;
use crate::board::HARTS_MAX;
use crate::kernel::percpu;
use crate::kernel::sched::{Qos, SCHEDULER, State, THREADS_MAX};
use crate::kernel::timer;

// 512 fits the PSRAM `.psram_buf` slot once per-thread records grew
// (ready_since, etc.); still ample history since dumps read the tail.
const TRACE_BUFFER_SIZE: usize = 512;

static TRACE_SEQ: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug)]
struct PerCpuTracePoint {
    idle_thread_idx: usize,
    current_thread_idx: usize,
    switching_from_thread_idx: Option<usize>,
    needs_reschedule: bool,
}

impl PerCpuTracePoint {
    const fn new() -> Self {
        Self {
            idle_thread_idx: 0,
            current_thread_idx: 0,
            switching_from_thread_idx: None,
            needs_reschedule: false,
        }
    }
}

#[derive(Debug)]
#[allow(dead_code)]
struct ThreadControlBlockTracePoint {
    state: State,
    id: u16,
    qos: Qos,
    priority: u8,
    pass: u64,
    last_started_cycles: u64,
    next_waiter: Option<usize>,
    affinity: Option<u8>,
    user_thread: bool,
    needs_user_exit: bool,
    ready_since: u64,
}

/// Why a Ready thread wasn't given a CPU, computed at a `ready-stall`
/// snapshot from the live state. `blocker_*` name the thread it out-ranks on
/// an eligible hart (the smoking gun for a missed preemption); zero when none.
#[derive(Debug)]
#[allow(dead_code)]
pub(crate) struct PickMiss {
    starved_id: u16,
    ready_ms: u64,
    blocker_id: u16,
    blocker_hart: u8,
    // True if the out-ranked runner is on the hart that DETECTED the stall —
    // that hart should have preempted locally (a local pick/reschedule bug);
    // false means only the remote hart's runner is out-ranked (an IPI/kick
    // gap — the remote hart was never told to reschedule).
    local: bool,
    reason: &'static str,
}

#[derive(Debug)]
struct TracePoint {
    time_stamp: u64,
    label: &'static str,
    per_cpu: [PerCpuTracePoint; HARTS_MAX],
    threads: [Option<ThreadControlBlockTracePoint>; THREADS_MAX],
    wake_overshoot: [u64; THREADS_MAX],
    pick_miss: Option<PickMiss>,
}

impl TracePoint {
    const fn new() -> Self {
        Self {
            time_stamp: 0,
            label: "",
            per_cpu: [const { PerCpuTracePoint::new() }; HARTS_MAX],
            threads: [const { None }; THREADS_MAX],
            wake_overshoot: [0; THREADS_MAX],
            pick_miss: None,
        }
    }
}

// Store data in a static ring buffer that overwrites - no locking, lightweight, fixed size
struct TraceBuf([UnsafeCell<TracePoint>; TRACE_BUFFER_SIZE]);

// Safety: All writes are disjoint
unsafe impl Sync for TraceBuf {}

// Safety: The caller must provide a valid pointer to a TracePoint
unsafe fn stash_percpu(tp: *mut TracePoint) {
    // Safety: Caller has provided valid pointer to a TracePoint
    unsafe {
        for hart in 0..HARTS_MAX {
            if hart == hart_id() {
                (*tp).per_cpu[hart].idle_thread_idx = percpu::idle_thread_idx();
                (*tp).per_cpu[hart].current_thread_idx = percpu::current_thread_idx();
                (*tp).per_cpu[hart].switching_from_thread_idx = percpu::switching_from_thread_idx();
                (*tp).per_cpu[hart].needs_reschedule = percpu::needs_reschedule();
            } else {
                // Use cross-Hart methods
                (*tp).per_cpu[hart].idle_thread_idx = percpu::other_idle_thread_idx();
                (*tp).per_cpu[hart].current_thread_idx = percpu::other_current_thread_idx();
                (*tp).per_cpu[hart].switching_from_thread_idx =
                    percpu::other_switching_from_thread_idx();
                (*tp).per_cpu[hart].needs_reschedule = percpu::other_needs_reschedule();
            }
        }
    }
}

// Safety: Caller must provide a valid pointer to a TracePoint
unsafe fn stash_tcbs(tp: *mut TracePoint, tcbs: &[Option<ThreadControlBlock>]) {
    unsafe {
        for (idx, tcb_array) in tcbs.iter().enumerate().take(THREADS_MAX) {
            if let Some(tcb) = tcb_array {
                let tcbtp = ThreadControlBlockTracePoint {
                    state: tcb.state,
                    id: tcb.id,
                    qos: tcb.qos,
                    priority: tcb.priority,
                    pass: tcb.pass,
                    last_started_cycles: tcb.last_started_cycles,
                    next_waiter: tcb.next_waiter.map(|handle| handle.idx),
                    affinity: tcb.affinity,
                    user_thread: tcb.user.is_some(),
                    needs_user_exit: SCHEDULER.needs_user_exit.get(idx),
                    ready_since: tcb.ready_since,
                };
                (*tp).threads[idx] = Some(tcbtp); // copy
            }
        }
    };
}

#[unsafe(link_section = ".psram_buf")]
static TRACE_BUF: TraceBuf =
    TraceBuf([const { UnsafeCell::new(TracePoint::new()) }; TRACE_BUFFER_SIZE]);

impl SchedInner {
    // Fill a trace point from the current state (lock already held).
    // Safety: caller provides a valid pointer to a TracePoint.
    unsafe fn write_snapshot(&self, tp: *mut TracePoint, label: &'static str) {
        unsafe {
            (*tp).label = label;
            (*tp).time_stamp = timer::elapsed();
            stash_percpu(tp);
            stash_tcbs(tp, &self.thread_blocks.0);
            (*tp).wake_overshoot = self.wake_overshoot;
            (*tp).pick_miss = None;
        }
    }

    // Take a snapshot with the scheduler lock already held.
    #[allow(dead_code)]
    pub(crate) fn snapshot_raw(&self, site_id: &'static str) {
        let idx = TRACE_SEQ.fetch_add(1, Ordering::Relaxed) % TRACE_BUFFER_SIZE;
        let tp = TRACE_BUF.0[idx].get();
        // Safety: distinct ring slot; dump runs single-threaded from panic.
        unsafe { self.write_snapshot(tp, site_id) };
    }

    // Snapshot annotated with WHY a stalled Ready thread isn't running.
    #[allow(dead_code)]
    pub(crate) fn snapshot_ready_stall(&self, stalled_idx: usize) {
        let idx = TRACE_SEQ.fetch_add(1, Ordering::Relaxed) % TRACE_BUFFER_SIZE;
        let tp = TRACE_BUF.0[idx].get();
        // Safety: distinct ring slot; dump runs single-threaded from panic.
        unsafe {
            self.write_snapshot(tp, "ready-stall");
            (*tp).pick_miss = Some(self.compute_pick_miss(stalled_idx));
        }
    }

    // Reconstruct the reason a Ready thread is being passed over, from the
    // live state: does it out-rank (lower pass) a thread currently running on
    // a hart it's allowed to use? If so that's a missed preemption.
    fn compute_pick_miss(&self, stalled_idx: usize) -> PickMiss {
        let stalled = &self.thread_blocks.0[stalled_idx]
            .as_ref()
            .expect("should be a valid thread");
        let now = timer::elapsed();
        let ready_ms = now.saturating_sub(stalled.ready_since) / timer::CYCLES_PER_MS;
        let this = hart_id();
        let mut pm = PickMiss {
            starved_id: stalled.id,
            ready_ms,
            blocker_id: 0,
            blocker_hart: 0,
            local: false,
            reason: "lower priority than every runner (waiting its turn)",
        };
        // Check the DETECTING hart first (could it have preempted its own
        // runner locally?), then the other hart (does it need a kick?). The
        // first out-ranked, eligible runner wins. (2-hart system: other = this ^ 1.)
        for &hart in &[this, this ^ 1] {
            let cur_idx = if hart == this {
                percpu::current_thread_idx()
            } else {
                percpu::other_current_thread_idx()
            };
            let runner = &self.thread_blocks.0[cur_idx]
                .as_ref()
                .expect("should be valid thread");
            let affinity_ok = stalled.affinity.is_none_or(|h| h as usize == hart);
            if affinity_ok && stalled.pass < runner.pass {
                pm.blocker_id = runner.id;
                pm.blocker_hart = hart as u8;
                pm.local = hart == this;
                pm.reason = if pm.local {
                    "detecting hart did NOT preempt its own runner (LOCAL pick/reschedule bug)"
                } else {
                    "out-ranks remote-hart runner; remote never kicked (IPI/notify gap)"
                };
                break;
            } else if !affinity_ok {
                pm.reason = "pinned to a hart busy with another thread";
            }
        }
        pm
    }
}

// Snapshot entry point for the `#[trace]` macro, called at function entry
// with the scheduler lock NOT held. Acquire it and delegate to
// `snapshot_raw` so this path captures exactly what the manual under-lock
// call sites do — percpu, the TCBs, AND `wake_overshoot` — in one consistent
// pass. (The old with_tcbs path could not reach `wake_overshoot`.)
pub(crate) fn take_snapshot(label: &'static str) {
    SCHEDULER.sched.lock().snapshot_raw(label);
}

/// Discard all buffered snapshots. The test runner calls this at each test
/// boundary so a panic dump shows only the failing test's history, not the
/// tail of whatever ran before it (e.g. T3's busy contenders bleeding into
/// T4). Resetting the sequence to 0 makes the next `count` snapshots the
/// only live entries.
#[allow(dead_code)]
pub(crate) fn reset() {
    TRACE_SEQ.store(0, Ordering::Relaxed);
}

/// Print every live (non-Avail, non-idle) thread with its state, plus counts.
/// The test runner calls this at each test boundary: a test that leaves
/// runnable threads behind shows up as an elevated `runnable` count entering
/// the *next* test, which pins the leaker.
#[allow(dead_code)]
pub(crate) fn report_live(label: &'static str) {
    // Snapshot the live threads under the scheduler lock, then DROP it before
    // printing. Two hazards this avoids:
    //  - The lock is an IrqSpinLock (interrupts disabled). The buffered UART
    //    writer spins waiting for the THRE drain interrupt once TX_BUF fills —
    //    which never fires with interrupts off, hanging the hart. So no I/O is
    //    done while the lock is held.
    //  - The line goes through the *buffered* writer (`with_uart_writer`), like
    //    the rest of the test output, so it stays FIFO-ordered instead of
    //    racing ahead via the direct writer (the old `dprint!` garble).
    // State is Copy, so we can snapshot (id, state) into a local array.
    let mut entries: [Option<(u16, State)>; THREADS_MAX] = [None; THREADS_MAX];
    let mut n = 0;
    let mut runnable = 0usize;
    let mut live = 0usize;
    {
        let sched = SCHEDULER.sched.lock();
        for tcb in sched.thread_blocks.0.iter().as_ref().iter().flatten() {
            if tcb.priority == PRIORITY_MIN {
                continue;
            }
            live += 1;
            if matches!(tcb.state, State::Ready | State::Running) {
                runnable += 1;
            }
            entries[n] = Some((tcb.id, tcb.state));
            n += 1;
        }
    }
    crate::drivers::uart::with_uart_writer(|w| {
        use core::fmt::Write;
        let _ = write!(w, "[live @ {label}]");
        for (id, state) in entries[..n].iter().flatten() {
            let mut cell = ColBuf::new();
            write_state(&mut cell, state);
            let _ = write!(w, " id{id}={}", cell.as_str());
        }
        let _ = writeln!(w, "  ({runnable} runnable, {live} live)");
    });
}

// Width of the rendered state field. The longest compact state is
// `Sw>BlkU@<ms>`; six ms digits (~16 min of runtime) fits in 14.
const STATE_COL: usize = 14;

/// A small fixed-capacity sink so each trace cell can be rendered into a
/// bounded, column-aligned field without allocating. The dump runs from the
/// panic handler, where the global allocator may be poisoned, so `format!`
/// is off-limits; this writes into a stack buffer instead. Overflowing
/// writes past `CAP` are dropped rather than panicking.
struct ColBuf {
    buf: [u8; Self::CAP],
    len: usize,
}

impl ColBuf {
    const CAP: usize = 24;

    fn new() -> Self {
        Self {
            buf: [b' '; Self::CAP],
            len: 0,
        }
    }

    fn as_str(&self) -> &str {
        // We only ever write ASCII here and never past CAP.
        core::str::from_utf8(&self.buf[..self.len]).unwrap_or("?")
    }
}

impl Write for ColBuf {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for &b in s.as_bytes() {
            if self.len < Self::CAP {
                self.buf[self.len] = b;
                self.len += 1;
            }
        }
        Ok(())
    }
}

/// Render a deadline as `tag@<wake-ms>`, matching the timestamp column, or
/// `tag@MAX` for the park-forever sentinel (`u64::MAX`).
fn write_deadline(w: &mut impl Write, tag: &str, d: &Deadline) {
    if d.at_cycles == u64::MAX {
        let _ = write!(w, "{tag}@MAX");
    } else {
        let _ = write!(w, "{tag}@{}", d.at_cycles / timer::CYCLES_PER_MS);
    }
}

fn write_post_switch(w: &mut impl Write, ps: &PostSwitch) {
    match ps {
        PostSwitch::Blocked => drop(write!(w, "Blk")),
        PostSwitch::BlockedUntil(d) => write_deadline(w, "BlkU", d),
        PostSwitch::Ready => drop(write!(w, "Rdy")),
        PostSwitch::Sleeping(d) => write_deadline(w, "Slp", d),
        PostSwitch::Dead(ExitReason::Exit) => drop(write!(w, "Dead")),
        PostSwitch::Dead(ExitReason::Fault) => drop(write!(w, "Fault")),
    }
}

/// Compact, fixed-vocabulary rendering of a thread state, kept short so the
/// per-thread columns stay aligned. Sleep/block deadlines collapse to a wake
/// time in ms (see `write_deadline`).
fn write_state(w: &mut impl Write, s: &State) {
    match s {
        State::Blocked => drop(write!(w, "Blk")),
        State::BlockedUntil(d) => write_deadline(w, "BlkU", d),
        State::Ready => drop(write!(w, "Rdy")),
        State::Running => drop(write!(w, "Run")),
        State::Sleeping(d) => write_deadline(w, "Slp", d),
        State::Switching(ps) => {
            let _ = write!(w, "Sw>");
            write_post_switch(w, ps);
        }
    }
}

pub(crate) fn dump_trace() {
    dprintln!("==== THREAD TRACE START ====");
    dprintln!(
        "(cell = slot:idN state [+Nms] Hn; Avail omitted. \
         Run Rdy Blk Slp@ms BlkU@ms Sw>next Dead/Fault; @MAX = parked; \
         +Nms = last wake overshoot; wNms = time spent Ready (wait for CPU); \
         pN = pass above row floor, so p0 = tied at the floor; \
         H/L after id = Qos High/Low; ready-stall rows carry a pick-miss line)"
    );
    // Replay oldest -> newest. TRACE_SEQ is a monotonic count of snapshots
    // taken; the live entries are the last `count` of them. Before the ring
    // fills, the oldest is slot 0; after wrap, it's the next-to-overwrite slot.
    let total = TRACE_SEQ.load(Ordering::Relaxed);
    let count = total.min(TRACE_BUFFER_SIZE);
    let oldest = if total <= TRACE_BUFFER_SIZE {
        0
    } else {
        total % TRACE_BUFFER_SIZE
    };
    for k in 0..count {
        let p = (oldest + k) % TRACE_BUFFER_SIZE;
        let tp = &TRACE_BUF.0[p].get();
        // Safety: the dump runs single-threaded from the panic handler;
        // there are no concurrent writers to the trace buffer.
        let ms = unsafe { (**tp).time_stamp } / timer::CYCLES_PER_MS;
        let label = unsafe { (**tp).label };
        dprint!("{ms:>6} ms | {label:<13} |");
        // Row floor: the lowest pass among present non-idle threads, mirroring
        // the scheduler's pass_baseline (PRI_MIN/idle excluded). Each cell then
        // shows `pN` = how far that thread sits above the floor, so a tie at the
        // floor (p0) — which is what stops a woken thread from preempting — is
        // obvious at a glance.
        let mut floor = u64::MAX;
        for j in 0..THREADS_MAX {
            if let Some(tcb) = unsafe { &(**tp).threads[j] }
                && tcb.priority != PRIORITY_MIN
            {
                floor = floor.min(tcb.pass);
            }
        }
        let floor = if floor == u64::MAX { 0 } else { floor };
        for i in 0..THREADS_MAX {
            let tcb_tp = unsafe { &(**tp).threads[i] };
            if let Some(tcb) = tcb_tp {
                // Which hart, if any, has this slot as its current thread?
                let mut on_hart = "  ";
                for h in 0..HARTS_MAX {
                    if unsafe { (**tp).per_cpu[h].current_thread_idx } == i {
                        on_hart = match h {
                            0 => "H0",
                            1 => "H1",
                            _ => "H?",
                        };
                    }
                }
                let mut cell = ColBuf::new();
                write_state(&mut cell, &tcb.state);
                // Tag the cell with the most recent wake overshoot, but only
                // when it's worth noticing (> 1 ms) — on-time wakes leave a
                // sub-ms value that would just be noise. The tag refers to the
                // thread's last timer wake, which may predate its current
                // state, so it rides alongside rather than replacing it.
                let overshoot = unsafe { (**tp).wake_overshoot[i] };
                if overshoot > timer::CYCLES_PER_MS {
                    let _ = write!(cell, " +{}ms", overshoot / timer::CYCLES_PER_MS);
                }
                // For a Ready thread, show how long it's been waiting for a CPU
                // (`wNms`). A growing value across rows is the starvation signal.
                if matches!(tcb.state, State::Ready) {
                    let waited = unsafe { (**tp).time_stamp }.saturating_sub(tcb.ready_since)
                        / timer::CYCLES_PER_MS;
                    if waited >= 1 {
                        let _ = write!(cell, " w{waited}ms");
                    }
                }
                let delta = tcb.pass.saturating_sub(floor);
                // QoS class: H(igh) wants tight wakes, L(ow) tolerates leeway.
                // Lets us pick the T4 measurer (a High thread) out of the row.
                let qos = match tcb.qos {
                    Qos::High => 'H',
                    Qos::Low => 'L',
                };
                dprint!(
                    " {i:>2}:id{:<3}{qos} {:<width$} {on_hart} p{delta:<9} |",
                    tcb.id,
                    cell.as_str(),
                    width = STATE_COL
                );
            }
        }
        dprintln!("");
        // If this snapshot was a ready-stall, explain why the thread waited.
        if let Some(pm) = unsafe { &(**tp).pick_miss } {
            dprintln!(
                "         ^ pick-miss id{} ready {}ms: {} (blocker id{} H{}, local={})",
                pm.starved_id,
                pm.ready_ms,
                pm.reason,
                pm.blocker_id,
                pm.blocker_hart,
                pm.local
            );
        }
    }
    dprintln!("==== THREAD TRACE END ====");
}
