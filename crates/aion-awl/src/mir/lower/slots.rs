//! The reserved-slot pools (split from `forks` for the 500-line law):
//! `Slots` bundles the loop and fork pools threaded through region lowering;
//! `ForkSlots` mirrors `LoopSlots`' alignment discipline exactly.

use super::super::func::MirFn;
use super::super::ids::FnRef;
use super::driver::LowerError;
use super::loops::LoopSlots;

/// The reserved-slot pools consumed while regions lower: loop slots first
/// (appended after every chain fn), fork slots after every loop fn.
pub(super) struct Slots {
    pub(super) loops: LoopSlots,
    pub(super) forks: ForkSlots,
}

/// The reserved fork-function slots (skeleton-planned, pre-order) and the
/// bodies built while regions lower — the same alignment discipline as
/// `LoopSlots` (`FnRef(n)` is literally `functions[n]`).
pub(super) struct ForkSlots {
    refs: Vec<FnRef>,
    next: usize,
    built: Vec<Option<MirFn>>,
}

impl ForkSlots {
    pub(super) fn new(refs: Vec<FnRef>) -> Self {
        let built = refs.iter().map(|_| None).collect();
        Self {
            refs,
            next: 0,
            built,
        }
    }

    pub(super) fn take(&mut self) -> Result<(usize, FnRef), LowerError> {
        let ordinal = self.next;
        self.next += 1;
        let reference = *self.refs.get(ordinal).ok_or_else(|| LowerError::Planning {
            message: "fork encountered beyond the reserved inventory".to_owned(),
        })?;
        Ok((ordinal, reference))
    }

    /// Record the built function for a taken ordinal.
    pub(super) fn finish(&mut self, ordinal: usize, function: MirFn) {
        self.built[ordinal] = Some(function);
    }

    /// Append the built fork functions at their reserved indices. Every
    /// reserved slot must have been consumed — a hole would misalign every
    /// later `FnRef`.
    pub(super) fn append_into(self, functions: &mut Vec<MirFn>) -> Result<(), LowerError> {
        for (ordinal, slot) in self.built.into_iter().enumerate() {
            let function = slot.ok_or_else(|| LowerError::Planning {
                message: format!("reserved fork slot {ordinal} was never lowered"),
            })?;
            if self.refs[ordinal].0 as usize != functions.len() {
                return Err(LowerError::Planning {
                    message: format!("fork slot {ordinal} misaligned with its reserved ref"),
                });
            }
            functions.push(function);
        }
        Ok(())
    }
}
