//! Detailed profiling
//
// Profiling is per HART static buffer

#![allow(dead_code)]

use core::cell::UnsafeCell;
use core::mem::MaybeUninit;
use core::sync::atomic::{AtomicU16, AtomicUsize, Ordering};

use crate::arch::hart_id;
use crate::arch::regs::sp;

const RECORDS_MAX: usize = 1024; // Note that less is useable. We silently drop any others.
// Using a 16 bit counter
const _: () = assert!(RECORDS_MAX <= u16::MAX as usize);

#[repr(u8)]
#[derive(Clone, Copy, Debug)]
pub(crate) enum ProfileKind {
    Entry = 0,
    Exit = 1,
}

/// Magic value used as the validity sentinel on `ProfileRecord`. A fully
/// written slot has `valid == VALID_MAGIC`; a slot being mid-written by
/// `push` has `valid == 0`. `drain` skips records where `valid` is anything
/// other than `VALID_MAGIC` — this catches torn reads (drain racing with
/// push on the same slot) and uninitialised slots in a single check.
const VALID_MAGIC: u32 = 0xCAFE_BABE;

// Each record is roughly 24-32 bytes (after the `valid` field is added).
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct ProfileRecord {
    // `valid` is FIRST so it sits at a known offset (offset 0) for cheap
    // single-word access during the invalidate/validate dance in `push`.
    valid: u32,
    // Hart id is taken from the buffer in which it is stored
    name: &'static str,
    kind: ProfileKind,
    cycle_stamp: u64,
    sp: usize,
}

impl ProfileRecord {
    fn build(name: &'static str, kind: ProfileKind) -> Self {
        Self {
            valid: VALID_MAGIC,
            name,
            kind,
            // Use the CLINT mtime (via `timer::elapsed()`) rather than the
            // `mcycle` CSR. mtime ticks at `TIMER_FREQ_HZ`, matching
            // `CYCLES_PER_US` used in dump formatting — so the µs deltas
            // shown by the dump reflect actual wall time. On QEMU `mcycle`
            // counts emulated instructions and runs at a different (much
            // higher) rate, which would inflate reported µs ~125x.
            cycle_stamp: crate::kernel::timer::elapsed(),
            sp: sp(),
        }
    }
}

struct ProfileBuf<const N: usize> {
    buf: [UnsafeCell<MaybeUninit<ProfileRecord>>; N],
    counter: AtomicU16,
    // Deepest sp ever pushed to this buffer, segregated by stack region.
    // The IRQ stack and thread stack(s) live at different absolute addresses,
    // so a global lowest-sp would always favor the lower-address stack and
    // hide the other. Tracking each separately answers "which function went
    // deepest on the IRQ stack?" and "which function went deepest on a
    // thread stack?" independently.
    deepest_irq_sp: AtomicUsize,
    deepest_irq_name: UnsafeCell<&'static str>,
    deepest_thread_sp: AtomicUsize,
    deepest_thread_name: UnsafeCell<&'static str>,
    // IRQ stack bounds for this HART, populated by `init()`. A push whose
    // sp falls inside `[irq_lo, irq_hi)` updates the irq tracker; otherwise
    // the thread tracker.
    irq_stack_lo: AtomicUsize,
    irq_stack_hi: AtomicUsize,
}

// Safety: each HART pushes to its own buffer (single producer per buffer),
// and the dump consumer reads via raw pointers. The atomic counter
// serialises the push side; torn reads during drain are tolerated as a
// known profiler limitation.
unsafe impl<const N: usize> Sync for ProfileBuf<N> {}

impl<const N: usize> ProfileBuf<N> {
    const fn new() -> Self {
        Self {
            buf: [const { UnsafeCell::new(MaybeUninit::uninit()) }; N],
            counter: AtomicU16::new(0),
            deepest_irq_sp: AtomicUsize::new(usize::MAX),
            deepest_irq_name: UnsafeCell::new("(none)"),
            deepest_thread_sp: AtomicUsize::new(usize::MAX),
            deepest_thread_name: UnsafeCell::new("(none)"),
            irq_stack_lo: AtomicUsize::new(0),
            irq_stack_hi: AtomicUsize::new(0),
        }
    }

    fn push(&self, mut record: ProfileRecord) {
        let idx = self.counter.fetch_add(1, Ordering::Relaxed) as usize % N;
        let slot = self.buf[idx].get();
        let r = slot.cast::<ProfileRecord>();

        // Bulk-write the record with `valid = 0` so that even if the
        // underlying memcpy writes the valid field first, a concurrent
        // drain sees "invalid" and skips this slot. We use
        // `core::ptr::write` directly — a single raw memcpy without
        // the `MaybeUninit::new` + `assume_init_mut` ceremony that
        // `MaybeUninit::write` adds in debug builds.
        record.valid = 0;
        unsafe { core::ptr::write(r, record) };

        // Release fence ensures the bulk write lands before the
        // validity sentinel becomes visible to a reader. The final
        // `write_volatile` is the only one we still need — it's a
        // single-word store with a defined visibility point.
        core::sync::atomic::fence(Ordering::Release);
        unsafe { core::ptr::write_volatile(&raw mut (*r).valid, VALID_MAGIC) };

        // Update deepest-sp tracker. Lower sp = deeper. `fetch_min` returns
        // the previous value; if our sp was strictly lower, we won the race
        // to lower the bound and should record our name alongside it.
        //
        // There IS a benign race: another push on the same HART could win
        // a deeper sp between our `fetch_min` and our `deepest_name` write,
        // and then we'd overwrite their name with ours. For a single-producer
        // buffer that only happens in trap-during-trap scenarios (which we
        // don't have), so in practice this is consistent. Even if a torn
        // read occurred, the worst case is a slightly wrong name on the
        // deepest record — acceptable for a diagnostic.
        // Classify the push by stack region and update the appropriate
        // deepest-tracker. IRQ-stack bounds are loaded once at init.
        let irq_lo = self.irq_stack_lo.load(Ordering::Relaxed);
        let irq_hi = self.irq_stack_hi.load(Ordering::Relaxed);
        let in_irq = record.sp >= irq_lo && record.sp < irq_hi;
        let (sp_field, name_cell) = if in_irq {
            (&self.deepest_irq_sp, &self.deepest_irq_name)
        } else {
            (&self.deepest_thread_sp, &self.deepest_thread_name)
        };
        let prev = sp_field.fetch_min(record.sp, Ordering::Relaxed);
        if record.sp < prev {
            // Safety: writes happen only on the producing HART; the name
            // pointer is one word so a torn read by the dump consumer can
            // produce at worst a slightly wrong name on the deepest record.
            unsafe { *name_cell.get() = record.name };
        }
    }

    fn drain(&self, mut f: impl FnMut(&ProfileRecord)) {
        let head = self.counter.load(Ordering::Relaxed) as usize;
        let start = head.saturating_sub(N);
        for i in start..head {
            let slot = self.buf[i % N].get();
            let r = slot.cast::<ProfileRecord>();

            // Read the validity sentinel first via volatile. If it isn't
            // the magic value, the slot is either:
            //   - currently being written (push set valid=0 in step 1)
            //   - never been written (uninitialised garbage memory)
            // Either way, skip it. The Acquire fence pairs with push's
            // Release fence to ensure that if we see VALID_MAGIC, all the
            // field writes that preceded it are also visible.
            let valid = unsafe { core::ptr::read_volatile(&raw const (*r).valid) };
            if valid != VALID_MAGIC {
                continue;
            }
            core::sync::atomic::fence(Ordering::Acquire);

            // Now safe to read as a fully-initialised record.
            let record = unsafe { &*r };
            f(record);
        }
    }

    /// Returns the deepest IRQ-stack push as (sp, name), or None if no
    /// IRQ-context push has happened yet.
    fn deepest_irq(&self) -> Option<(usize, &'static str)> {
        let sp = self.deepest_irq_sp.load(Ordering::Relaxed);
        if sp == usize::MAX {
            None
        } else {
            let name = unsafe { *self.deepest_irq_name.get() };
            Some((sp, name))
        }
    }

    /// Returns the deepest thread-stack push as (sp, name), or None if no
    /// thread-context push has happened yet.
    fn deepest_thread(&self) -> Option<(usize, &'static str)> {
        let sp = self.deepest_thread_sp.load(Ordering::Relaxed);
        if sp == usize::MAX {
            None
        } else {
            let name = unsafe { *self.deepest_thread_name.get() };
            Some((sp, name))
        }
    }
}

#[unsafe(link_section = ".psram_buf")]
static PROFILE_HART0: ProfileBuf<RECORDS_MAX> = ProfileBuf::new();

#[unsafe(link_section = ".psram_buf")]
static PROFILE_HART1: ProfileBuf<RECORDS_MAX> = ProfileBuf::new();

pub(crate) struct ProfileGuard {
    name: &'static str,
}

fn push_record(record: ProfileRecord) {
    match hart_id() {
        0 => PROFILE_HART0.push(record),
        1 => PROFILE_HART1.push(record),
        _ => panic!("Too many HARTs"),
    }
}

impl ProfileGuard {
    pub(crate) fn new(name: &'static str) -> Self {
        let record = ProfileRecord::build(name, ProfileKind::Entry);
        push_record(record);
        Self { name }
    }
}

impl Drop for ProfileGuard {
    fn drop(&mut self) {
        let record = ProfileRecord::build(self.name, ProfileKind::Exit);
        push_record(record);
    }
}

pub(crate) fn init() {
    // The statics live in `.psram_buf` which is NOLOAD, so const initialisers
    // are discarded. Set them explicitly here.
    unsafe extern "C" {
        static __hart0_irq_stack_start: u8;
        static __hart0_irq_stack_top: u8;
        static __hart1_irq_stack_start: u8;
        static __hart1_irq_stack_top: u8;
    }

    for buf in [&PROFILE_HART0, &PROFILE_HART1] {
        buf.counter.store(0, Ordering::Relaxed);
        buf.deepest_irq_sp.store(usize::MAX, Ordering::Relaxed);
        buf.deepest_thread_sp.store(usize::MAX, Ordering::Relaxed);
    }

    PROFILE_HART0.irq_stack_lo.store(
        &raw const __hart0_irq_stack_start as usize,
        Ordering::Relaxed,
    );
    PROFILE_HART0
        .irq_stack_hi
        .store(&raw const __hart0_irq_stack_top as usize, Ordering::Relaxed);
    PROFILE_HART1.irq_stack_lo.store(
        &raw const __hart1_irq_stack_start as usize,
        Ordering::Relaxed,
    );
    PROFILE_HART1
        .irq_stack_hi
        .store(&raw const __hart1_irq_stack_top as usize, Ordering::Relaxed);
}

// Display wrapper that inserts `_` every three digits.
// Builds the string into a stack buffer and uses `f.pad` so that width/align
// specifiers (e.g. `{:>14}`) are honored by the formatter.
struct U64Sep(u64);
impl core::fmt::Display for U64Sep {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        if self.0 == 0 {
            return f.pad("0");
        }
        // u64 max = 20 digits + 6 underscores.
        let mut buf = [0u8; 26];
        let mut len = 0;
        let mut n = self.0;
        let mut digits = 0;
        while n > 0 {
            if digits > 0 && digits % 3 == 0 {
                buf[len] = b'_';
                len += 1;
            }
            buf[len] = b'0' + (n % 10) as u8;
            len += 1;
            digits += 1;
            n /= 10;
        }
        // We built LSB-first; reverse in place to get MSB-first.
        buf[..len].reverse();
        // Bytes are ASCII digits and `_` only.
        let s = core::str::from_utf8(&buf[..len]).expect("ASCII only");
        f.pad(s)
    }
}

// Display wrapper for the per-record sp delta. Renders None as "    -",
// zero as "    =", non-zero as a signed 5-char field (e.g. "  -48", " +112").
struct SpDelta(Option<isize>);
impl core::fmt::Display for SpDelta {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        match self.0 {
            None => write!(f, "    -"),
            Some(0) => write!(f, "    ="),
            Some(d) => write!(f, "{:+5}", d),
        }
    }
}

fn drain_and_print<const N: usize>(profile_buf: &ProfileBuf<N>, label: &str) -> usize {
    use crate::io::DirectWriter;
    use crate::kernel::timer::CYCLES_PER_US;
    use core::fmt::Write;

    // Combined width of indent + name field so the sp column lands at a fixed
    // position regardless of depth. Sized for full module paths like
    // `kernel::sched::stride::pick_next_if_fairer_mut` (~45 chars) after the
    // `ease::` prefix is stripped below.
    const NAME_FIELD: usize = 50;

    let mut count: usize = 0;
    let mut depth: usize = 0;
    let mut prev_cycles: Option<u64> = None;
    let mut prev_sp: Option<usize> = None;

    profile_buf.drain(|r| {
        // Microseconds since the previous record on this HART.
        // First record has no predecessor, so delta is 0.
        let delta_us = match prev_cycles {
            Some(prev) => r.cycle_stamp.saturating_sub(prev) / CYCLES_PER_US.max(1),
            None => 0,
        };
        prev_cycles = Some(r.cycle_stamp);

        // sp delta: positive = stack shrank (Exit), negative = grew deeper (Entry).
        let sp_delta = prev_sp.map(|p| r.sp as isize - p as isize);
        prev_sp = Some(r.sp);

        // For Exit, decrement BEFORE printing so the Exit lines up with
        // its matching Entry. saturating_sub guards against drift if any
        // records were dropped due to buffer wrap.
        if let ProfileKind::Exit = r.kind {
            depth = depth.saturating_sub(1);
        }

        let indent = depth * 2;
        let arrow = match r.kind {
            ProfileKind::Entry => '>',
            ProfileKind::Exit => '<',
        };

        // Shrink the name field so indent + name = NAME_FIELD. Keeps sp aligned.
        let name_w = NAME_FIELD.saturating_sub(indent);

        // Strip the crate name from `module_path!()`-prefixed names so the
        // output is `kernel::sched::mark_for_preempt` rather than
        // `ease::kernel::sched::mark_for_preempt`.
        let display_name = r.name.strip_prefix("ease::").unwrap_or(r.name);

        let _ = writeln!(
            DirectWriter,
            "[{}] {:>14}us {:indent$}{} {:<name_w$} sp={:08x}  Δsp={}",
            label,
            U64Sep(delta_us),
            "",
            arrow,
            display_name,
            r.sp,
            SpDelta(sp_delta),
            indent = indent,
            name_w = name_w,
        );

        // For Entry, increment AFTER printing so nested calls appear deeper.
        if let ProfileKind::Entry = r.kind {
            depth += 1;
        }

        count += 1;
    });

    count
}

use crate::io::DirectWriter;
use core::fmt::Write;

fn print_deepest<const N: usize>(buf: &ProfileBuf<N>, label: &str) {
    fn print_one(label: &str, kind: &str, entry: Option<(usize, &'static str)>) {
        match entry {
            None => {
                let _ = writeln!(DirectWriter, "[{}] deepest {}: (no records)", label, kind,);
            }
            Some((sp, name)) => {
                let display_name = name.strip_prefix("ease::").unwrap_or(name);
                let _ = writeln!(
                    DirectWriter,
                    "[{}] deepest {}: sp={:08x} in {}",
                    label, kind, sp, display_name,
                );
            }
        }
    }
    print_one(label, "irq   ", buf.deepest_irq());
    print_one(label, "thread", buf.deepest_thread());
}

pub(crate) fn dump() {
    let _ = writeln!(DirectWriter, "== profile dump ==");
    let total = drain_and_print(&PROFILE_HART0, "H0") + drain_and_print(&PROFILE_HART1, "H1");
    // Print the deepest summary AFTER the record stream so it's easy to
    // find — otherwise it scrolls off the top with hundreds of records.
    print_deepest(&PROFILE_HART0, "H0");
    print_deepest(&PROFILE_HART1, "H1");
    if total == 0 {
        let _ = writeln!(DirectWriter, "(no records)");
    }
    let _ = writeln!(DirectWriter, "== profile dump ==");
}
