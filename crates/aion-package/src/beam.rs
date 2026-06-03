//! `BeamModule` and `BeamSet` with canonical ordering.

use crate::PackageError;

/// A compiled BEAM module preserved exactly as supplied to the package layer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BeamModule {
    /// Logical module name before deployment-time namespacing.
    pub name: String,
    /// Exact compiled `.beam` bytes for the logical module.
    pub bytes: Vec<u8>,
}

impl BeamModule {
    /// Creates a beam module value without modifying its name or bytes.
    #[must_use]
    pub fn new(name: impl Into<String>, bytes: impl Into<Vec<u8>>) -> Self {
        Self {
            name: name.into(),
            bytes: bytes.into(),
        }
    }

    /// Returns the logical module name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the exact compiled module bytes.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

/// A canonical, duplicate-free collection of compiled BEAM modules.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BeamSet {
    modules: Vec<BeamModule>,
}

impl BeamSet {
    /// Creates a beam set sorted by logical module name.
    ///
    /// Duplicate logical module names are rejected because they would make the
    /// canonical order ambiguous.
    ///
    /// # Errors
    ///
    /// Returns [`PackageError::MalformedBeamEntry`] when two modules have the
    /// same logical module name.
    pub fn new(mut modules: Vec<BeamModule>) -> Result<Self, PackageError> {
        modules.sort_by(|left, right| left.name.cmp(&right.name));

        if let Some(duplicate) = modules
            .windows(2)
            .find(|pair| pair[0].name == pair[1].name)
            .map(|pair| pair[0].name.clone())
        {
            return Err(PackageError::MalformedBeamEntry { entry: duplicate });
        }

        Ok(Self { modules })
    }

    /// Returns the number of modules in the set.
    #[must_use]
    pub fn len(&self) -> usize {
        self.modules.len()
    }

    /// Returns true when the set contains no modules.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.modules.is_empty()
    }

    /// Iterates modules in canonical logical-name order.
    pub fn iter(&self) -> impl Iterator<Item = &BeamModule> {
        self.modules.iter()
    }

    /// Looks up exact module bytes by logical module name.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&[u8]> {
        self.modules
            .binary_search_by(|module| module.name.as_str().cmp(name))
            .ok()
            .map(|index| self.modules[index].bytes.as_slice())
    }
}

impl IntoIterator for BeamSet {
    type IntoIter = std::vec::IntoIter<BeamModule>;
    type Item = BeamModule;

    fn into_iter(self) -> Self::IntoIter {
        self.modules.into_iter()
    }
}

impl<'a> IntoIterator for &'a BeamSet {
    type IntoIter = std::slice::Iter<'a, BeamModule>;
    type Item = &'a BeamModule;

    fn into_iter(self) -> Self::IntoIter {
        self.modules.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::{BeamModule, BeamSet};
    use crate::PackageError;

    #[test]
    fn beam_set_order_is_independent_of_insertion_order() -> Result<(), PackageError> {
        let first = BeamSet::new(vec![
            BeamModule::new("workflow/c", vec![3]),
            BeamModule::new("workflow/a", vec![1]),
            BeamModule::new("workflow/b", vec![2]),
        ])?;
        let second = BeamSet::new(vec![
            BeamModule::new("workflow/b", vec![2]),
            BeamModule::new("workflow/c", vec![3]),
            BeamModule::new("workflow/a", vec![1]),
        ])?;

        let first_names: Vec<&str> = first.iter().map(BeamModule::name).collect();
        let second_names: Vec<&str> = second.iter().map(BeamModule::name).collect();

        assert_eq!(first_names, vec!["workflow/a", "workflow/b", "workflow/c"]);
        assert_eq!(first_names, second_names);
        assert_eq!(first, second);

        Ok(())
    }

    #[test]
    fn beam_set_rejects_duplicate_logical_names() {
        let result = BeamSet::new(vec![
            BeamModule::new("workflow/a", vec![1]),
            BeamModule::new("workflow/a", vec![2]),
        ]);

        assert!(matches!(
            result,
            Err(PackageError::MalformedBeamEntry { entry }) if entry == "workflow/a"
        ));
    }

    #[test]
    fn lookup_returns_exact_bytes_by_logical_name() -> Result<(), PackageError> {
        let bytes = vec![0, 1, 2, 3, 255];
        let beams = BeamSet::new(vec![BeamModule::new("workflow/a", bytes.clone())])?;

        assert_eq!(beams.get("workflow/a"), Some(bytes.as_slice()));
        assert_eq!(beams.get("workflow/missing"), None);

        Ok(())
    }
}
