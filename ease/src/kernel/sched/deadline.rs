//! Thread sleep deadline
//!
//! Thread can never sleep past its deadline, but it can coalesce to match earlier wakeups within a leeway

use core::fmt::Debug;

use crate::kernel::timer;

use super::types::Qos;

const LEEWAY_DEFAULT_US: u64 = 100;
const LEEWAY_DEFAULT_CYCLES: u64 = LEEWAY_DEFAULT_US * timer::CYCLES_PER_US;
const LEEWAY_MAX_US: u64 = 1_000_000; // Maximum slack period - used to cap the maximum requested leeway to sensible values
const LEEWAY_MAX_CYCLES: u64 = LEEWAY_MAX_US * timer::CYCLES_PER_US;

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct Deadline {
    pub(super) at_cycles: u64,
    pub(super) fixed_leeway: Option<u64>, // None - use system default leeway
}

impl Deadline {
    // Helper function - returns the leeway in timer cycles
    pub(super) fn leeway(&self, qos: &Qos) -> u64 {
        if let Some(leeway) = self.fixed_leeway {
            leeway
        } else {
            let now = crate::kernel::timer::elapsed();
            let remaining = self.at_cycles.saturating_sub(now);
            match qos {
                Qos::High => (remaining >> 16).min(LEEWAY_DEFAULT_CYCLES),
                Qos::Low => (remaining >> 3).min(LEEWAY_MAX_CYCLES),
            }
        }
    }
}
