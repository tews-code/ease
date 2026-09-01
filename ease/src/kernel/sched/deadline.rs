//! Thread sleep deadline
//!
//! Thread minimum sleep is its deadline, but the scheduler actively looks to extend the sleep by the leeway
//! unless it can coalesce to match another wake up between the deadline and the leeway
//!
//! Leeway can be fixed or system calculated. System calculated leeways are set according to the thread's QoS

use core::fmt::Debug;

use crate::kernel::timer;

use super::Qos;

const LEEWAY_DEFAULT_US: u64 = 100;
const LEEWAY_DEFAULT_CYCLES: u64 = LEEWAY_DEFAULT_US * timer::CYCLES_PER_US;
const LEEWAY_MAX_US: u64 = 1_000_000; // Maximum slack period - used to cap the maximum requested leeway to sensible values
const LEEWAY_MAX_CYCLES: u64 = LEEWAY_MAX_US * timer::CYCLES_PER_US;

/// The deadline leeway
///
/// - `None` - no leeway, wakes at deadline
/// - `Fixed` - a fixed leeway after the deadline in cycles
/// - `System` - system-derived leeway. Note EASE uses this aggressively for low QoS threads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Leeway {
    None,
    Fixed(u64), // Cycles
    System,
}

impl Leeway {
    /// A fixed leeway from a duration in milliseconds — the one boundary
    /// where ms enter a `Leeway`; the stored value is always cycles.
    pub(crate) fn fixed_ms(duration_ms: u64) -> Self {
        Self::Fixed(duration_ms.saturating_mul(timer::CYCLES_PER_MS))
    }
}

/// Absolute deadline with leeway, measured in cycles.
///
/// Holds the absolute deadline value in cycles as well
/// as a `Leeway` (also in cycles where applicable).
/// The leeway is the *latest* time after a deadline that is acceptable for wake up.
/// Set the Leeway to
/// - `None` if no leeway is allowed,
/// - `Fixed(u64)` if a fixed leeway is decided on by the caller, or
/// - `System` if the leeway can be chosen by the system.
///
/// Note that
/// - deadlines can be missed by milliseconds due to scheduler activity.
/// - a thread may still not be immediately scheduled when woken if the HART is busy.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct Deadline {
    at: u64,
    leeway: Leeway,
}

impl Deadline {
    /// New deadline from millisecond values and given `Leeway`.
    pub(crate) fn from_ms(deadline_ms: u64, leeway: Leeway) -> Self {
        Self {
            at: deadline_ms.saturating_mul(timer::CYCLES_PER_MS),
            leeway,
        }
    }
    /// New deadline after duration millisecond values.
    pub(crate) fn after_ms(duration_ms: u64, leeway: Leeway) -> Self {
        Self {
            at: (duration_ms.saturating_add(timer::elapsed_ms()))
                .saturating_mul(timer::CYCLES_PER_MS),
            leeway,
        }
    }
    /// Set the deadline and leeway in cycles.
    pub(super) fn set(&mut self, at_cycles: u64, leeway: Leeway) {
        self.at = at_cycles;
        self.leeway = leeway;
    }
    /// Returns the absolute deadline in cycles. Ignores leeway.
    pub(super) fn at(&self) -> u64 {
        self.at
    }
    /// Returns the deadline's `Leeway`
    pub(super) fn leeway(&self) -> Leeway {
        self.leeway
    }
    /// Returns the absolute deadline and full leeway in cycles
    pub(super) fn latest(&self, qos: &Qos) -> u64 {
        self.at.saturating_add(self.leeway_cycles(qos))
    }
    /// Checks if the deadline is in the past. Ignores leeway.
    pub(crate) fn has_passed(&self) -> bool {
        self.at < timer::elapsed()
    }
    /// Checks if the given time is within the deadline and leeway
    ///
    /// Takes the thread's QoS as this drives the system leeway
    pub(super) fn contains(&self, absolute_cycles: u64, qos: &Qos) -> bool {
        (self.at..=self.latest(qos)).contains(&absolute_cycles)
    }
    /// Returns the leeway value in timer cycles
    ///
    /// `System` leeway is derived from the thread's `Qos`
    fn leeway_cycles(&self, qos: &Qos) -> u64 {
        match self.leeway {
            Leeway::None => 0,
            Leeway::Fixed(leeway_cycles) => leeway_cycles,
            Leeway::System => {
                let now = timer::elapsed();
                let remaining = self.at.saturating_sub(now);
                match qos {
                    Qos::High => (remaining >> 16).min(LEEWAY_DEFAULT_CYCLES),
                    Qos::Low => (remaining >> 3).min(LEEWAY_MAX_CYCLES),
                }
            }
        }
    }
}
