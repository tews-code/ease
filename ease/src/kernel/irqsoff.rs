//! Interrupts-off tracer (`irqsoff` feature)
//!
//! An interrupt that arrives while `mstatus.MIE` is clear waits until the
//! hart re-enables interrupts, so the longest interrupts-off *section* on a
//! hart is a hard floor on wake and input latency. This module measures those
//! sections per hart, in the manner of Linux's `irqsoff` tracer.
//!
//! A section opens when MIE goes from set to clear and closes when it goes
//! back. Every software transition passes through one of three doors in
//! [`crate::arch::interrupts`] — `disable`, `restore` and `enable` — which
//! already see the previous `mstatus`, so they know when they are the
//! outermost. Nested sections do nothing. The hardware door (trap entry
//! clears MIE) opens a section too, via [`trap_entry`], tagged with the trap
//! cause so the report can tell a timer tick from an IPI or an ecall.
//!
//! Attribution uses `#[track_caller]` on the doors and on the lock and
//! critical-section entry points that call them. The attribute lives on the
//! callee and makes the compiler pass the caller's file and line as a hidden
//! argument, so every existing and future lock site is attributed without
//! opting in.
//!
//! Some closes are invisible: `mret` re-enables without software seeing it,
//! and a return to U-mode always re-enables because M-mode interrupts cannot
//! be masked there. The bookkeeping rule that makes this harmless is that an
//! open always overwrites, a close consumes, and a close with nothing open is
//! ignored. A trap that exits by plain `mret` leaves a dangling open that the
//! thread's next `disable` (or the next trap) overwrites. Those trap-only
//! sections are short and the `profile` feature already covers them.
//!
//! The idle thread sleeps in `wfi` inside a critical section. `wfi` returns
//! once an enabled interrupt is pending even with MIE clear, so that time is
//! idle, not latency: `wait_for_interrupt` closes a section going in and opens
//! a fresh one coming out ([`Site::Wfi`]), leaving only the wake-to-enable tail.
//!
//! A section can close on a different thread from the one that opened it: a
//! timer trap opens, the trampoline runs `schedule`, `switch_to` lands in
//! another thread, and that thread's guard drop closes. The accounting is per
//! hart, so this is exactly right — it is the hart, not the thread, that has
//! interrupts off — and the report shows both ends, with the thread index on
//! each side so a switch (or the absence of one) is visible.
//!
//! Each section also records its `mtime` start and the `cycle` counter delta.
//! The start lets the two harts' longest sections be compared: the same
//! length at the same instant on both harts means the virtual machine
//! stopped, not that the kernel ran for that long. (Under `-icount` the
//! `cycle` delta would count instructions and say the same thing directly;
//! without it QEMU reports host time there, so it merely mirrors `mtime`.)
//!
//! `IrqSpinLock` also reports how long each acquisition spun on its ticket
//! and how long the guard was then held, keyed by lock site. A long section
//! that turns out to be a long *wait* points at the other hart, and the hold
//! table then says what that hart was doing under the same lock.
//!
//! Writers: only the owning hart writes its record, always with interrupts
//! disabled, so plain `UnsafeCell`s suffice. Readers: the report reads the
//! other hart's record racily; totals go through the seqlocked `CounterU64`,
//! the per-site table may tear. It is a diagnostic printed when the system is
//! quiet, so this is accepted.

use core::cell::UnsafeCell;
use core::fmt::{self, Write};
use core::panic::Location;
use core::sync::atomic::{AtomicU32, Ordering};

use crate::arch::csr::mcause::{self, Trap, exception, interrupt};
use crate::arch::csr::rdcycles;
use crate::arch::hart_id;
use crate::kernel::percpu;
use crate::kernel::sync::CounterU64;
use crate::kernel::timer::{self, CYCLES_PER_MS, CYCLES_PER_US};

/// What kind of trap opened a section, decoded from `mcause` at entry
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum TrapKind {
    Timer,
    Software,
    External,
    EcallFromUser,
    Interrupt(usize),
    Exception(usize),
}

impl TrapKind {
    fn from_mcause(cause: Trap) -> Self {
        match cause {
            Trap::Interrupt(interrupt::TIMER) => TrapKind::Timer,
            Trap::Interrupt(interrupt::SOFTWARE) => TrapKind::Software,
            Trap::Interrupt(interrupt::EXTERNAL) => TrapKind::External,
            Trap::Exception(exception::ECALL_FROM_U) => TrapKind::EcallFromUser,
            Trap::Interrupt(code) => TrapKind::Interrupt(code),
            Trap::Exception(code) => TrapKind::Exception(code),
        }
    }
}

impl fmt::Display for TrapKind {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            TrapKind::Timer => f.write_str("timer trap"),
            TrapKind::Software => f.write_str("ipi trap"),
            TrapKind::External => f.write_str("external trap"),
            TrapKind::EcallFromUser => f.write_str("ecall trap"),
            TrapKind::Interrupt(code) => write!(f, "interrupt {code} trap"),
            TrapKind::Exception(code) => write!(f, "exception {code} trap"),
        }
    }
}

/// Where a section was opened or closed
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Site {
    /// Hardware cleared MIE on trap entry
    Trap(TrapKind),
    /// Either side of a `wfi` sleep: a section closes going in and a new one
    /// opens coming out, so idle time is not counted as latency
    Wfi,
    /// Software cleared or set MIE at this call site
    At(&'static Location<'static>),
}

impl fmt::Display for Site {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Site::Trap(kind) => kind.fmt(f),
            Site::Wfi => f.write_str("wfi"),
            Site::At(loc) => write!(f, "{}:{}", loc.file(), loc.line()),
        }
    }
}

/// One closed section: how long, how many instructions, and both ends
#[derive(Clone, Copy)]
struct Section {
    /// `mtime` at open: lets the two harts' longest sections be compared for
    /// overlap — identical lengths at the same instant mean the machine
    /// stopped, not the kernel
    start: u64,
    cycles: u64,
    /// `cycle` CSR delta. NOTE: on QEMU without -icount this is host time,
    /// not instructions, so it only differs from `cycles` under -icount
    insns: u64,
    open: Site,
    open_thread: u8,
    close: Site,
    close_thread: u8,
}

impl Section {
    const NONE: Self = Self {
        start: 0,
        cycles: 0,
        insns: 0,
        open: Site::Wfi,
        open_thread: 0,
        close: Site::Wfi,
        close_thread: 0,
    };
}

impl fmt::Display for Section {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "{} us / {} kcyc at {} ms  {} -> {}  (thread {} -> {})",
            self.cycles / CYCLES_PER_US,
            self.insns / 1000,
            self.start / CYCLES_PER_MS,
            self.open,
            self.close,
            self.open_thread,
            self.close_thread
        )
    }
}

/// Distinct opening sites tracked per hart. A full test run touches ~35 lock
/// and critical-section sites plus the trap kinds and wfi; sections whose site
/// does not fit are counted in `overflow` and still contribute to the total.
const SITES_MAX: usize = 64;
const _: () = assert!(
    SITES_MAX <= u64::BITS as usize,
    "report ranks with a u64 printed-mask"
);

#[derive(Clone, Copy)]
struct SiteStats {
    site: Site,
    count: u32,
    total: u64,
    longest: Section,
}

impl SiteStats {
    const fn new(site: Site) -> Self {
        Self {
            site,
            count: 0,
            total: 0,
            longest: Section::NONE,
        }
    }
    fn record(&mut self, section: Section) {
        self.count += 1;
        self.total += section.cycles;
        if section.cycles > self.longest.cycles {
            self.longest = section;
        }
    }
}

struct Hart {
    /// Time stamp, instruction stamp, site and thread of the open section
    open: UnsafeCell<Option<(u64, u64, Site, u8)>>,
    /// Cycles spent with interrupts off, all sections
    total: CounterU64,
    /// Closed sections
    count: AtomicU32,
    /// Closed sections whose opening site did not fit in `sites`
    overflow: AtomicU32,
    /// Longest section seen on this hart, any site
    longest: UnsafeCell<Section>,
    /// `mtime` at the last hook call, and the largest gap between consecutive
    /// hook calls with when it began. Hooks fire thousands of times a second on
    /// a busy hart, so this is a stall detector that works with interrupts
    /// enabled too: the same gap at the same instant on both harts means the
    /// virtual machine stopped.
    last_hook: UnsafeCell<u64>,
    max_gap: UnsafeCell<(u64, u64)>,
    sites: UnsafeCell<[Option<SiteStats>; SITES_MAX]>,
    /// Longest ticket-lock spin on this hart, and per-site lock hold times
    longest_wait: UnsafeCell<LockEvent>,
    longest_hold: UnsafeCell<LockEvent>,
    holds: UnsafeCell<[Option<LockStats>; SITES_MAX]>,
}

/// One lock wait or hold: how long, which lock site, which thread
#[derive(Clone, Copy)]
struct LockEvent {
    cycles: u64,
    site: &'static Location<'static>,
    thread: u8,
}

impl LockEvent {
    const NONE: Self = Self {
        cycles: 0,
        site: Location::caller(),
        thread: 0,
    };
}

impl fmt::Display for LockEvent {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "{} us  {}:{}  (thread {})",
            self.cycles / CYCLES_PER_US,
            self.site.file(),
            self.site.line(),
            self.thread
        )
    }
}

#[derive(Clone, Copy)]
struct LockStats {
    site: &'static Location<'static>,
    count: u32,
    total_hold: u64,
    max_hold: u64,
    max_wait: u64,
}

// Safety: each hart writes only its own record, with interrupts disabled.
// The report's cross-hart reads are racy by design (see module docs).
unsafe impl Sync for Hart {}

impl Hart {
    const fn new() -> Self {
        Self {
            open: UnsafeCell::new(None),
            total: CounterU64::new(0),
            count: AtomicU32::new(0),
            overflow: AtomicU32::new(0),
            longest: UnsafeCell::new(Section::NONE),
            last_hook: UnsafeCell::new(0),
            max_gap: UnsafeCell::new((0, 0)),
            sites: UnsafeCell::new([None; SITES_MAX]),
            longest_wait: UnsafeCell::new(LockEvent::NONE),
            longest_hold: UnsafeCell::new(LockEvent::NONE),
            holds: UnsafeCell::new([None; SITES_MAX]),
        }
    }
}

static HARTS: [Hart; 2] = [Hart::new(), Hart::new()];

fn this_hart() -> &'static Hart {
    &HARTS[hart_id()]
}

fn current_thread() -> u8 {
    percpu::current_thread_idx() as u8
}

/// Stamp a hook call and track the largest gap since the previous one.
/// Must be called with interrupts disabled. Returns `mtime` now.
fn hook_stamp(h: &Hart, count_gap: bool) -> u64 {
    let now = timer::elapsed();
    // Safety: own hart, interrupts disabled — no concurrent writer.
    let last = unsafe { &mut *h.last_hook.get() };
    let gap = now.saturating_sub(*last);
    let max_gap = unsafe { &mut *h.max_gap.get() };
    if count_gap && *last != 0 && gap > max_gap.0 {
        *max_gap = (gap, *last);
    }
    *last = now;
    now
}

/// Open a section: MIE has just gone from set to clear on this hart.
///
/// Must be called with interrupts disabled. Overwrites any dangling open.
pub(crate) fn open(site: Site) {
    let h = this_hart();
    // Safety: own hart, interrupts disabled — no concurrent writer.
    // The gap across a `wfi` sleep is idle time, not a stall
    let now = hook_stamp(h, site != Site::Wfi);
    unsafe { *h.open.get() = Some((now, rdcycles(), site, current_thread())) };
}

/// Close a section: MIE is about to go from clear to set on this hart.
///
/// Must be called with interrupts still disabled. Ignored if nothing is open.
pub(crate) fn close(close_site: Site) {
    let h = this_hart();
    // Safety: own hart, interrupts disabled — no concurrent writer.
    let now = hook_stamp(h, true);
    let Some((start, start_insns, open_site, open_thread)) = (unsafe { (*h.open.get()).take() })
    else {
        return;
    };
    let section = Section {
        start,
        cycles: now.saturating_sub(start),
        insns: rdcycles().saturating_sub(start_insns),
        open: open_site,
        open_thread,
        close: close_site,
        close_thread: current_thread(),
    };
    // Safety: single writer per hart (this hart, interrupts disabled).
    unsafe { h.total.add(section.cycles) };
    h.count.fetch_add(1, Ordering::Relaxed);
    // Safety: as above.
    let longest = unsafe { &mut *h.longest.get() };
    if section.cycles > longest.cycles {
        *longest = section;
    }
    // Safety: as above.
    let sites = unsafe { &mut *h.sites.get() };
    if let Some(stats) = sites.iter_mut().flatten().find(|s| s.site == open_site) {
        stats.record(section);
    } else if let Some(empty) = sites.iter_mut().find(|s| s.is_none()) {
        let mut stats = SiteStats::new(open_site);
        stats.record(section);
        *empty = Some(stats);
    } else {
        h.overflow.fetch_add(1, Ordering::Relaxed);
    }
}

/// The hardware door: trap entry cleared MIE. Tags the section with `mcause`.
pub(crate) fn trap_entry() {
    open(Site::Trap(TrapKind::from_mcause(mcause::read())));
}

/// An `IrqSpinLock` at `site` finished spinning on its ticket; the spin began
/// at `wait_start`. Returns the acquisition stamp for [`lock_released`].
///
/// Called with interrupts disabled (the lock disables them before spinning).
pub(crate) fn lock_acquired(site: &'static Location<'static>, wait_start: u64) -> u64 {
    let now = timer::elapsed();
    let waited = now.saturating_sub(wait_start);
    let h = this_hart();
    // Safety: own hart, interrupts disabled — no concurrent writer.
    let longest = unsafe { &mut *h.longest_wait.get() };
    if waited > longest.cycles {
        *longest = LockEvent {
            cycles: waited,
            site,
            thread: current_thread(),
        };
    }
    // Safety: as above.
    let holds = unsafe { &mut *h.holds.get() };
    if let Some(stats) = holds.iter_mut().flatten().find(|s| s.site == site) {
        stats.max_wait = stats.max_wait.max(waited);
    } else if let Some(empty) = holds.iter().position(|s| s.is_none()) {
        holds[empty] = Some(LockStats {
            site,
            count: 0,
            total_hold: 0,
            max_hold: 0,
            max_wait: waited,
        });
    }
    now
}

/// The guard for the `IrqSpinLock` taken at `site` is being dropped.
///
/// Called with interrupts still disabled (before the guard restores them).
pub(crate) fn lock_released(site: &'static Location<'static>, acquired: u64) {
    let held = timer::elapsed().saturating_sub(acquired);
    let h = this_hart();
    // Safety: own hart, interrupts disabled — no concurrent writer.
    let longest = unsafe { &mut *h.longest_hold.get() };
    if held > longest.cycles {
        *longest = LockEvent {
            cycles: held,
            site,
            thread: current_thread(),
        };
    }
    // Safety: as above.
    let holds = unsafe { &mut *h.holds.get() };
    if let Some(stats) = holds.iter_mut().flatten().find(|s| s.site == site) {
        stats.count += 1;
        stats.total_hold += held;
        stats.max_hold = stats.max_hold.max(held);
    } else if let Some(empty) = holds.iter().position(|s| s.is_none()) {
        holds[empty] = Some(LockStats {
            site,
            count: 1,
            total_hold: held,
            max_hold: held,
            max_wait: 0,
        });
    }
}

/// Write the per-hart report: share of wall time with interrupts off since
/// boot, section count, the longest section, and every opening site ranked by
/// its longest section.
pub(crate) fn write_report(w: &mut impl Write) -> fmt::Result {
    let elapsed = timer::elapsed();
    for (id, h) in HARTS.iter().enumerate() {
        let total = h.total.get();
        let permille = total.saturating_mul(1000).checked_div(elapsed).unwrap_or(0);
        writeln!(
            w,
            "irqsoff hart {id}: interrupts off {}.{}% of {} ms across {} sections ({} unattributed)",
            permille / 10,
            permille % 10,
            elapsed / CYCLES_PER_MS,
            h.count.load(Ordering::Relaxed),
            h.overflow.load(Ordering::Relaxed),
        )?;
        // Safety: racy read of the other hart's record, by design.
        writeln!(w, "  longest: {}", unsafe { *h.longest.get() })?;
        let (gap, at) = unsafe { *h.max_gap.get() };
        writeln!(
            w,
            "  largest gap between hook calls: {} us at {} ms",
            gap / CYCLES_PER_US,
            at / CYCLES_PER_MS
        )?;
        // Rank by longest section without copying the table: a panic in the
        // trap handler reports from the 1.5 KB IRQ stack.
        let sites = unsafe { &*h.sites.get() };
        writeln!(
            w,
            "     max us     max kcyc     total us      count  thr  opened at -> longest closed at"
        )?;
        let mut printed: u64 = 0;
        loop {
            let next = sites
                .iter()
                .enumerate()
                .filter(|(i, s)| printed & (1 << i) == 0 && s.is_some())
                .max_by_key(|(_, s)| s.map_or(0, |s| s.longest.cycles));
            let Some((i, Some(stats))) = next else { break };
            printed |= 1 << i;
            let l = &stats.longest;
            writeln!(
                w,
                "  {:>9}  {:>11}  {:>11}  {:>9}  {:>2}->{:<2}  {} -> {}",
                l.cycles / CYCLES_PER_US,
                l.insns / 1000,
                stats.total / CYCLES_PER_US,
                stats.count,
                l.open_thread,
                l.close_thread,
                stats.site,
                l.close,
            )?;
        }
        // Safety: racy read of the other hart's record, by design.
        writeln!(w, "  longest lock wait: {}", unsafe {
            *h.longest_wait.get()
        })?;
        writeln!(w, "  longest lock hold: {}", unsafe {
            *h.longest_hold.get()
        })?;
        let holds = unsafe { &*h.holds.get() };
        writeln!(
            w,
            "   max hold     max wait   total hold      count  lock site"
        )?;
        let mut printed: u64 = 0;
        loop {
            let next = holds
                .iter()
                .enumerate()
                .filter(|(i, s)| printed & (1 << i) == 0 && s.is_some())
                .max_by_key(|(_, s)| s.map_or(0, |s| s.max_hold));
            let Some((i, Some(stats))) = next else { break };
            printed |= 1 << i;
            writeln!(
                w,
                "  {:>9}  {:>11}  {:>11}  {:>9}  {}:{}",
                stats.max_hold / CYCLES_PER_US,
                stats.max_wait / CYCLES_PER_US,
                stats.total_hold / CYCLES_PER_US,
                stats.count,
                stats.site.file(),
                stats.site.line(),
            )?;
        }
    }
    Ok(())
}

/// Print the report through the queued console, so it does not interleave
/// with output already queued by `println!` (the panic path uses the direct
/// writer instead, since the queue may not drain there).
pub(crate) fn print_report() {
    struct QueuedConsole;
    impl Write for QueuedConsole {
        fn write_str(&mut self, s: &str) -> fmt::Result {
            // Same path as `print!`, without the macro's own trait import
            crate::drivers::uart::with_uart_writer(|w| w.write_str(s))
        }
    }
    let _ = write_report(&mut QueuedConsole);
}

/// Per-test watch: which test grew a hart's longest section.
///
/// Written only by the test runner thread (one thread, one call per boundary).
struct TestWatch {
    seen: UnsafeCell<[u64; 2]>,
    seen_gap: UnsafeCell<[u64; 2]>,
    seen_wait: UnsafeCell<[u64; 2]>,
    seen_hold: UnsafeCell<[u64; 2]>,
    prev_test: UnsafeCell<&'static str>,
}

// Safety: single writer — the test runner thread — see `test_boundary`.
unsafe impl Sync for TestWatch {}

static TEST_WATCH: TestWatch = TestWatch {
    seen: UnsafeCell::new([0; 2]),
    seen_gap: UnsafeCell::new([0; 2]),
    seen_wait: UnsafeCell::new([0; 2]),
    seen_hold: UnsafeCell::new([0; 2]),
    prev_test: UnsafeCell::new("(before first test)"),
};

/// Called by the test runner at every test boundary with the name of the
/// test about to run. Prints a line for each hart whose longest section grew
/// during the test that just finished, naming that test. Pass any label at
/// the end of the run so the last test is covered too.
pub(crate) fn test_boundary(next_test: &'static str) {
    // Safety: the test runner is the only caller and runs tests one at a time.
    let seen = unsafe { &mut *TEST_WATCH.seen.get() };
    let seen_gap = unsafe { &mut *TEST_WATCH.seen_gap.get() };
    let seen_wait = unsafe { &mut *TEST_WATCH.seen_wait.get() };
    let seen_hold = unsafe { &mut *TEST_WATCH.seen_hold.get() };
    let prev = unsafe { &mut *TEST_WATCH.prev_test.get() };
    for (id, h) in HARTS.iter().enumerate() {
        // Safety: racy read of a hart's record, by design.
        let longest = unsafe { *h.longest.get() };
        if longest.cycles > seen[id] {
            seen[id] = longest.cycles;
            crate::println!("irqsoff: hart {id} longest grew during {prev}: {longest}");
        }
        let (gap, at) = unsafe { *h.max_gap.get() };
        if gap > seen_gap[id] {
            seen_gap[id] = gap;
            crate::println!(
                "irqsoff: hart {id} largest hook gap grew during {prev}: {} us at {} ms",
                gap / CYCLES_PER_US,
                at / CYCLES_PER_MS
            );
        }
        let wait = unsafe { *h.longest_wait.get() };
        if wait.cycles > seen_wait[id] {
            seen_wait[id] = wait.cycles;
            crate::println!("irqsoff: hart {id} longest lock wait grew during {prev}: {wait}");
        }
        let hold = unsafe { *h.longest_hold.get() };
        if hold.cycles > seen_hold[id] {
            seen_hold[id] = hold.cycles;
            crate::println!("irqsoff: hart {id} longest lock hold grew during {prev}: {hold}");
        }
    }
    *prev = next_test;
}

/// Longest section and count for the opening site at `file:line`, summed
/// over both harts (a thread may migrate). Test support.
#[cfg(test)]
fn site_stats(file: &str, line: u32) -> Option<(u64, u32)> {
    let mut found = None;
    for h in HARTS.iter() {
        // Safety: test-only read; racy by design.
        let sites = unsafe { &*h.sites.get() };
        for stats in sites.iter().flatten() {
            if let Site::At(loc) = stats.site
                && loc.file() == file
                && loc.line() == line
            {
                let (max, count) = found.unwrap_or((0, 0));
                found = Some((max.max(stats.longest.cycles), count + stats.count));
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::sync::{IrqSpinLock, with_interrupts_disabled};

    /// The outermost lock site is attributed with a duration covering the
    /// work done under it; a critical section nested inside it opens no
    /// section of its own.
    #[test_case]
    fn outermost_site_attributed_and_nested_ignored() {
        static LOCK: IrqSpinLock<()> = IrqSpinLock::new(());
        const SPIN_CYCLES: u64 = 2_000;

        let outer_line = line!() + 1;
        let guard = LOCK.lock();
        let nested_line = line!() + 1;
        with_interrupts_disabled(|_cs| {
            let start = timer::elapsed();
            while timer::elapsed() - start < SPIN_CYCLES {
                core::hint::spin_loop();
            }
        });
        drop(guard);

        let (max, count) = site_stats(file!(), outer_line).expect("outer lock site recorded");
        assert!(
            max >= SPIN_CYCLES,
            "longest section at lock site {max} cycles < spin {SPIN_CYCLES}"
        );
        assert!(count >= 1);
        assert!(
            site_stats(file!(), nested_line).is_none(),
            "nested critical section must not open its own section"
        );
    }
}
