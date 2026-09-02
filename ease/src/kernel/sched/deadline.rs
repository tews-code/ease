//! Deadline with leeway
//!
//! A deadline is an absolute cycle value. The leeway is the number of cycles after
//! the deadline that are allowed.
//!
//! The leeway can be set by the caller or calculated by the system. System-calculated
//! leeways are dependent on the QoS.

use super::Qos;
use crate::kernel::timer;
use crate::percpu;

const LEEWAY_DEFAULT_US: u64 = 100;
const LEEWAY_DEFAULT_CYCLES: u64 = LEEWAY_DEFAULT_US * timer::CYCLES_PER_US;
const LEEWAY_MAX_US: u64 = 1_000_000; // Maximum slack period - used to cap the maximum requested leeway to sensible values
const LEEWAY_MAX_CYCLES: u64 = LEEWAY_MAX_US * timer::CYCLES_PER_US;

/// Absolute deadline with leeway, measured in cycles.
///
/// Holds the absolute deadline value in cycles as well
/// as a leeway in cycles. The leeway is the *latest* time
/// after a deadline that is acceptable for wake up.
///
/// The leeway can be set by the caller or calculated by the system
/// based on the thread's QoS.
/// - [Deadline::from_ms(deadline_ms, leeway_ms)]
/// - [Deadline::from_ms_with_system_leeway(deadline_ms)]
///
/// Note that
/// - deadlines can be missed by milliseconds due to scheduler activity.
/// - a thread may still not be immediately scheduled when woken if the HART is busy.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct Deadline {
    at: u64,
    leeway: u64,
}

impl Deadline {
    /// New deadline with leeway from millisecond values
    pub(crate) fn from_ms(deadline_ms: u64, leeway_ms: u64) -> Self {
        Self {
            at: deadline_ms.saturating_mul(timer::CYCLES_PER_MS),
            leeway: leeway_ms.saturating_mul(timer::CYCLES_PER_MS),
        }
    }
    /// New absolute deadline millisecond value using system leeway.
    pub(crate) fn from_ms_with_system_leeway(deadline_ms: u64) -> Self {
        let at = deadline_ms.saturating_mul(timer::CYCLES_PER_MS);
        Self {
            at,
            leeway: Self::system_leeway_cycles(at, percpu::current_qos()),
        }
    }
    /// New deadline after duration millisecond values.
    pub(crate) fn after_ms(duration_ms: u64, leeway_ms: u64) -> Self {
        Self {
            at: (duration_ms.saturating_add(timer::elapsed_ms()))
                .saturating_mul(timer::CYCLES_PER_MS),
            leeway: leeway_ms.saturating_mul(timer::CYCLES_PER_MS),
        }
    }
    /// New deadline after duration millisecond values using system leeway.
    pub(crate) fn after_ms_with_system_leeway(duration_ms: u64) -> Self {
        let at =
            (duration_ms.saturating_add(timer::elapsed_ms())).saturating_mul(timer::CYCLES_PER_MS);
        Self {
            at,
            leeway: Self::system_leeway_cycles(at, percpu::current_qos()),
        }
    }
    /// Coalesce to a given time in cycles. If there is a leeway this
    /// is reduced to keep the latest wakeup the same.
    ///
    /// # Panics #
    /// Panics if the given time is not within the deadline to deadline plus leeway window.
    pub(super) fn coalesce_to(&mut self, coalesce_cycles: u64) {
        assert!(
            self.contains(coalesce_cycles),
            "coalesce point is not within the windows"
        );
        let leeway = self.latest() - coalesce_cycles;
        self.at = coalesce_cycles;
        self.leeway = leeway;
    }
    /// Returns the absolute deadline in cycles. Ignores leeway.
    pub(super) fn at(&self) -> u64 {
        self.at
    }
    /// Returns the deadline's leeway in cycles.
    pub(super) fn leeway(&self) -> u64 {
        self.leeway
    }
    /// Returns the absolute deadline and full leeway in cycles
    pub(super) fn latest(&self) -> u64 {
        self.at.saturating_add(self.leeway)
    }
    /// Checks if the deadline is in the past. Ignores leeway.
    pub(crate) fn has_passed(&self) -> bool {
        self.at < timer::elapsed()
    }
    /// Checks if the given time in cycles is between the deadline and leeway
    pub(super) fn contains(&self, absolute_cycles: u64) -> bool {
        (self.at..=self.latest()).contains(&absolute_cycles)
    }
    /// Calculates the system leeway value in timer cycles
    fn system_leeway_cycles(at: u64, qos: Qos) -> u64 {
        let now = timer::elapsed();
        let remaining = at.saturating_sub(now);
        match qos {
            Qos::High => (remaining >> 16).min(LEEWAY_DEFAULT_CYCLES),
            Qos::Low => (remaining >> 3).min(LEEWAY_MAX_CYCLES),
        }
    }
}
