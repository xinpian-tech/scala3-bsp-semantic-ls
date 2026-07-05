//! Dispatch-generation bookkeeping. PC requests serialize on a single loaned
//! dispatch thread (the `InProcessPcWorker` semantics). When a generation
//! wedges non-cooperatively, its thread is abandoned (parked, stuck in the
//! wedged op) and a fresh generation is spawned. Abandoned generations are
//! bounded: past the cap the island is unrecoverable and the process exits in
//! an orderly way (crash-safe on-disk state; the editor restarts us).

/// The outcome of abandoning the current generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Advance {
    /// A fresh generation was spawned with this number.
    Spawned(u32),
    /// The abandoned-generation cap was exceeded; the island is fatal.
    Fatal,
}

/// Tracks the active dispatch generation and how many have been abandoned.
#[derive(Debug)]
pub struct GenerationState {
    current: u32,
    abandoned: u32,
    max_abandoned: u32,
}

impl GenerationState {
    /// A fresh state at generation 0 with the given abandoned-generation cap.
    pub fn new(max_abandoned: u32) -> GenerationState {
        GenerationState {
            current: 0,
            abandoned: 0,
            max_abandoned,
        }
    }

    /// The active generation number.
    pub fn current(&self) -> u32 {
        self.current
    }

    /// How many generations have been abandoned so far.
    pub fn abandoned(&self) -> u32 {
        self.abandoned
    }

    /// Abandon the current generation and advance to the next, unless doing so
    /// would exceed the cap (→ [`Advance::Fatal`]).
    pub fn advance(&mut self) -> Advance {
        if self.abandoned >= self.max_abandoned {
            return Advance::Fatal;
        }
        self.abandoned += 1;
        self.current += 1;
        Advance::Spawned(self.current)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advances_until_the_cap_then_goes_fatal() {
        let mut gen = GenerationState::new(2);
        assert_eq!(gen.current(), 0);
        assert_eq!(gen.advance(), Advance::Spawned(1));
        assert_eq!(gen.advance(), Advance::Spawned(2));
        assert_eq!(gen.abandoned(), 2);
        // The third abandonment exceeds the cap of 2.
        assert_eq!(gen.advance(), Advance::Fatal);
        // Still fatal afterwards; the generation does not advance past the cap.
        assert_eq!(gen.advance(), Advance::Fatal);
        assert_eq!(gen.current(), 2);
    }

    #[test]
    fn a_zero_cap_is_fatal_on_the_first_abandonment() {
        let mut gen = GenerationState::new(0);
        assert_eq!(gen.advance(), Advance::Fatal);
        assert_eq!(gen.current(), 0);
    }
}
