//! NIF registration surface.

use beamr::native::NativeFn;

/// Module/function/arity key for a native implemented function.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Mfa {
    /// BEAM module name that owns the native function import.
    pub module: String,
    /// BEAM function name imported from `module`.
    pub function: String,
    /// Function arity.
    pub arity: u8,
}

impl Mfa {
    /// Construct an MFA key from string-like module and function names.
    #[must_use]
    pub fn new(module: impl Into<String>, function: impl Into<String>, arity: u8) -> Self {
        Self {
            module: module.into(),
            function: function.into(),
            arity,
        }
    }

    /// Return the human-readable MFA as `module:function/arity`.
    #[must_use]
    pub fn display(&self) -> String {
        format!("{}:{}/{}", self.module, self.function, self.arity)
    }
}

/// A host- or engine-owned native implemented function entry.
#[derive(Clone, Debug)]
pub struct NifEntry {
    /// MFA key used by BEAM import resolution.
    pub mfa: Mfa,
    /// Rust function pointer compatible with beamr's native registry.
    pub function: NativeFn,
    /// Whether beamr should mark the entry for dirty scheduler execution.
    pub is_dirty: bool,
}

impl NifEntry {
    /// Construct a normal native implemented function entry.
    #[must_use]
    pub fn new(mfa: Mfa, function: NativeFn) -> Self {
        Self {
            mfa,
            function,
            is_dirty: false,
        }
    }

    /// Construct a native implemented function entry marked dirty.
    #[must_use]
    pub fn dirty(mfa: Mfa, function: NativeFn) -> Self {
        Self {
            mfa,
            function,
            is_dirty: true,
        }
    }
}

/// Accumulates NIF entries before they are installed into the runtime.
#[derive(Clone, Debug, Default)]
pub struct NifRegistration {
    entries: Vec<NifEntry>,
}

impl NifRegistration {
    /// Construct an empty NIF registration collection.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Add host-supplied NIF entries to the collection.
    pub fn add_host_nifs(&mut self, entries: impl IntoIterator<Item = NifEntry>) -> &mut Self {
        self.entries.extend(entries);
        self
    }

    /// Add engine-owned NIF entries to the collection.
    ///
    /// The engine currently owns no concrete NIFs in this cluster; later signal,
    /// timer, query, and activity NIFs can append here without changing host API.
    pub fn add_engine_nifs(&mut self) -> &mut Self {
        self
    }

    /// Consume the collection and return the entries to install.
    #[must_use]
    pub fn into_entries(self) -> Vec<NifEntry> {
        self.entries
    }

    /// Return true when no NIF entries have been collected.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Return the number of NIF entries collected.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use beamr::native::ProcessContext;
    use beamr::term::Term;

    use super::{Mfa, NifEntry, NifRegistration};

    fn native_zero(args: &[Term], _context: &mut ProcessContext) -> Result<Term, Term> {
        if args.len() > 255 {
            return Err(Term::small_int(0));
        }
        Ok(Term::small_int(0))
    }

    #[test]
    fn collects_host_and_engine_nifs() {
        let mut registration = NifRegistration::new();
        registration
            .add_engine_nifs()
            .add_host_nifs([NifEntry::dirty(Mfa::new("host", "zero", 0), native_zero)]);

        assert_eq!(registration.len(), 1);
        let entries = registration.into_entries();
        assert_eq!(entries[0].mfa.display(), "host:zero/0");
        assert!(entries[0].is_dirty);
    }
}
