//! Attributes for elements about to be created (`Initializer`).

use std::collections::BTreeMap;

use super::AdtError;

/// The attributes an element is created with.
pub type InitialAttributes = BTreeMap<String, String>;

/// Contributes attributes to an element about to be created.
pub trait Initializer {
    fn initialize(&self, target: &mut InitialAttributes) -> Result<(), AdtError>;
}

impl<F: Fn(&mut InitialAttributes) -> Result<(), AdtError>> Initializer for F {
    fn initialize(&self, target: &mut InitialAttributes) -> Result<(), AdtError> {
        self(target)
    }
}

/// Adds an attribute, failing if the target already has one with that name
/// (`Initializer.Helper.initialiseAttribute`).
pub fn initialise_attribute(
    target: &mut InitialAttributes,
    key: &str,
    value: &str,
) -> Result<(), AdtError> {
    if target.contains_key(key) {
        return Err(AdtError::DuplicateInitialiserKey(key.to_owned()));
    }
    target.insert(key.to_owned(), value.to_owned());
    Ok(())
}

/// The attributes an initializer produces
/// (`Initializer.Helper.buildAttributes`).
pub fn build_attributes(initializer: &dyn Initializer) -> Result<InitialAttributes, AdtError> {
    let mut attributes = InitialAttributes::new();
    initializer.initialize(&mut attributes)?;
    Ok(attributes)
}

/// Several initializers applied in order (`CompoundInitializer`).
#[derive(Default)]
pub struct CompoundInitializer<'a> {
    components: Vec<Box<dyn Initializer + 'a>>,
}

impl<'a> CompoundInitializer<'a> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a component, to be applied after those already added.
    pub fn with(mut self, component: impl Initializer + 'a) -> Self {
        self.components.push(Box::new(component));
        self
    }
}

impl Initializer for CompoundInitializer<'_> {
    fn initialize(&self, target: &mut InitialAttributes) -> Result<(), AdtError> {
        for component in &self.components {
            component.initialize(target)?;
        }
        Ok(())
    }
}
