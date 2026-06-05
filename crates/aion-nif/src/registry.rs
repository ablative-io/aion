//! `NifSet` builder; the value `register_nifs` consumes.

use std::collections::BTreeSet;

use crate::{Nif, NifDeclError};

/// Collection of declared NIF descriptors for the engine registration path.
///
/// `NifSet` is the argument shape consumed by `EngineBuilder::register_nifs`:
/// each contained [`Nif`] exposes its module/function/arity identity, native
/// shim, dirty flag, and determinism tag. `aion-nif` only emits this descriptor
/// set; it does not perform beamr registration itself.
#[derive(Clone, Debug)]
pub struct NifSet {
    module: String,
    nifs: Vec<Nif>,
}

/// Builder type for a [`NifSet`].
#[derive(Clone, Debug)]
pub struct NifSetBuilder {
    module: String,
    nifs: Vec<Nif>,
}

impl NifSet {
    /// Starts a new builder for declarations under `module`.
    #[must_use]
    pub fn builder(module: impl Into<String>) -> NifSetBuilder {
        NifSetBuilder::new(module)
    }

    /// Module name this set declares NIFs for.
    #[must_use]
    pub fn module(&self) -> &str {
        &self.module
    }

    /// Declared NIF descriptors in insertion order.
    #[must_use]
    pub fn nifs(&self) -> &[Nif] {
        &self.nifs
    }

    /// Iterates over declared NIF descriptors in insertion order.
    pub fn iter(&self) -> impl Iterator<Item = &Nif> {
        self.nifs.iter()
    }

    /// Consumes the set and returns its descriptors.
    #[must_use]
    pub fn into_nifs(self) -> Vec<Nif> {
        self.nifs
    }
}

impl NifSetBuilder {
    /// Starts a builder for declarations under `module`.
    #[must_use]
    pub fn new(module: impl Into<String>) -> Self {
        Self {
            module: module.into(),
            nifs: Vec::new(),
        }
    }

    /// Appends a declared NIF descriptor.
    ///
    /// Descriptors are accumulated without overwriting. Duplicate
    /// module/function/arity triples are rejected by [`Self::build`].
    #[must_use]
    pub fn with_nif(mut self, nif: Nif) -> Self {
        self.nifs.push(nif);
        self
    }

    /// Validates the declarations and produces a [`NifSet`].
    ///
    /// Duplicate module/function/arity triples are rejected before the engine's
    /// beamr registration boundary is reached.
    ///
    /// # Errors
    ///
    /// Returns [`NifDeclError::Duplicate`] when more than one descriptor has the
    /// same module/function/arity identity.
    pub fn build(self) -> Result<NifSet, NifDeclError> {
        let mut identities = BTreeSet::new();

        for nif in &self.nifs {
            let identity = (
                nif.module().to_owned(),
                nif.function().to_owned(),
                nif.arity(),
            );
            if !identities.insert(identity) {
                return Err(NifDeclError::Duplicate {
                    module: nif.module().to_owned(),
                    function: nif.function().to_owned(),
                    arity: nif.arity(),
                });
            }
        }

        Ok(NifSet {
            module: self.module,
            nifs: self.nifs,
        })
    }
}

impl IntoIterator for NifSet {
    type Item = Nif;
    type IntoIter = std::vec::IntoIter<Nif>;

    fn into_iter(self) -> Self::IntoIter {
        self.nifs.into_iter()
    }
}

impl<'a> IntoIterator for &'a NifSet {
    type Item = &'a Nif;
    type IntoIter = std::slice::Iter<'a, Nif>;

    fn into_iter(self) -> Self::IntoIter {
        self.nifs.iter()
    }
}

#[cfg(test)]
mod tests {
    use beamr::{native::ProcessContext, term::Term};

    use crate::{Determinism, Nif, NifDeclError, NifSet};

    fn identity_native(args: &[Term], ctx: &mut ProcessContext) -> Result<Term, Term> {
        let _ = ctx;
        args.first().copied().ok_or(Term::NIL)
    }

    #[test]
    fn set_builds_and_exposes_descriptors() -> Result<(), NifDeclError> {
        let pure = Nif::pure("example/module", "upper", 1, identity_native);
        let activity = Nif::side_effectful("example/module", "read", 1, identity_native);

        let set = NifSet::builder("example/module")
            .with_nif(pure)
            .with_nif(activity)
            .build()?;

        assert_eq!(set.module(), "example/module");
        assert_eq!(set.nifs().len(), 2);
        assert_eq!(set.nifs()[0].module(), "example/module");
        assert_eq!(set.nifs()[0].function(), "upper");
        assert_eq!(set.nifs()[0].arity(), 1);
        assert!(!set.nifs()[0].is_dirty());
        assert_eq!(set.nifs()[0].determinism(), Determinism::Pure);
        assert_eq!(set.nifs()[1].function(), "read");
        assert!(set.nifs()[1].is_dirty());
        assert_eq!(set.nifs()[1].determinism(), Determinism::SideEffectful);
        assert_eq!(set.iter().count(), 2);
        Ok(())
    }

    #[test]
    fn build_rejects_duplicate_module_function_arity() {
        let first = Nif::pure("example/module", "extract", 1, identity_native);
        let second = Nif::side_effectful("example/module", "extract", 1, identity_native);

        match NifSet::builder("example/module")
            .with_nif(first)
            .with_nif(second)
            .build()
        {
            Err(error) => assert_eq!(
                error,
                NifDeclError::Duplicate {
                    module: "example/module".to_owned(),
                    function: "extract".to_owned(),
                    arity: 1,
                }
            ),
            Ok(set) => panic!("duplicate build unexpectedly succeeded with {set:?}"),
        }
    }

    #[test]
    fn same_name_with_different_arity_is_distinct_mfa() -> Result<(), NifDeclError> {
        let unary = Nif::pure("example/module", "format", 1, identity_native);
        let binary = Nif::pure("example/module", "format", 2, identity_native);

        let set = NifSet::builder("example/module")
            .with_nif(unary)
            .with_nif(binary)
            .build()?;

        assert_eq!(set.nifs().len(), 2);
        assert_eq!(set.nifs()[0].arity(), 1);
        assert_eq!(set.nifs()[1].arity(), 2);
        Ok(())
    }
}
