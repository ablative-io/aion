//! Typed search attributes used by workflow visibility projections.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Closed set of supported workflow search attribute types.
#[derive(Serialize, Deserialize, ts_rs::TS, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SearchAttributeType {
    /// UTF-8 string value.
    String,
    /// Signed 64-bit integer value.
    Int,
    /// Double-precision floating point value.
    Float,
    /// Boolean value.
    Bool,
    /// UTC timestamp value.
    Datetime,
    /// List of keyword strings.
    KeywordList,
}

/// Typed value for a workflow search attribute.
#[derive(Serialize, Deserialize, ts_rs::TS, Clone, Debug, PartialEq)]
#[serde(tag = "type", content = "data")]
pub enum SearchAttributeValue {
    /// UTF-8 string value.
    String(String),
    /// Signed 64-bit integer value.
    Int(i64),
    /// Double-precision floating point value.
    Float(f64),
    /// Boolean value.
    Bool(bool),
    /// UTC timestamp value.
    Datetime(DateTime<Utc>),
    /// List of keyword strings.
    KeywordList(Vec<String>),
}

impl SearchAttributeValue {
    /// Returns the declared search attribute type matching this value variant.
    #[must_use]
    pub const fn attribute_type(&self) -> SearchAttributeType {
        match self {
            Self::String(_) => SearchAttributeType::String,
            Self::Int(_) => SearchAttributeType::Int,
            Self::Float(_) => SearchAttributeType::Float,
            Self::Bool(_) => SearchAttributeType::Bool,
            Self::Datetime(_) => SearchAttributeType::Datetime,
            Self::KeywordList(_) => SearchAttributeType::KeywordList,
        }
    }
}

/// Per-namespace registry of search attribute names to their declared types.
#[derive(Serialize, Deserialize, ts_rs::TS, Clone, Debug, Default, PartialEq, Eq)]
pub struct SearchAttributeSchema {
    attributes: HashMap<String, SearchAttributeType>,
}

impl SearchAttributeSchema {
    /// Creates an empty search attribute schema.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a search attribute name with its declared type.
    ///
    /// # Errors
    ///
    /// Returns [`SearchAttributeError::ConflictingType`] when the name is already registered with a
    /// different type. Re-registering the same name with the same type is idempotent.
    pub fn register(
        &mut self,
        name: impl Into<String>,
        attribute_type: SearchAttributeType,
    ) -> Result<(), SearchAttributeError> {
        let name = name.into();
        if let Some(existing) = self.attributes.get(&name).copied() {
            if existing == attribute_type {
                return Ok(());
            }

            return Err(SearchAttributeError::ConflictingType {
                name,
                existing,
                requested: attribute_type,
            });
        }

        self.attributes.insert(name, attribute_type);
        Ok(())
    }

    /// Validates that a value matches the type registered for a search attribute name.
    ///
    /// # Errors
    ///
    /// Returns [`SearchAttributeError::UnregisteredAttribute`] when no type is registered for the
    /// name, or [`SearchAttributeError::TypeMismatch`] when the value variant does not match the
    /// declared type.
    pub fn validate(
        &self,
        name: &str,
        value: &SearchAttributeValue,
    ) -> Result<(), SearchAttributeError> {
        let expected = self.attributes.get(name).copied().ok_or_else(|| {
            SearchAttributeError::UnregisteredAttribute {
                name: String::from(name),
            }
        })?;
        let actual = value.attribute_type();

        if expected == actual {
            Ok(())
        } else {
            Err(SearchAttributeError::TypeMismatch {
                name: String::from(name),
                expected,
                actual,
            })
        }
    }
}

/// Projects the current search attributes of a workflow from its event history.
///
/// Later [`crate::Event::SearchAttributesUpdated`] events override earlier
/// values key by key, matching how visibility projections fold attribute
/// updates. Histories without attribute events project to an empty map.
#[must_use]
pub fn search_attributes_from_events(
    events: &[crate::Event],
) -> HashMap<String, SearchAttributeValue> {
    let mut attributes = HashMap::new();
    for event in events {
        if let crate::Event::SearchAttributesUpdated {
            attributes: updated,
            ..
        } = event
        {
            attributes.extend(updated.clone());
        }
    }
    attributes
}

/// Errors produced when registering and validating typed search attributes.
#[derive(thiserror::Error, Clone, Debug, PartialEq, Eq)]
pub enum SearchAttributeError {
    /// A name was registered more than once with incompatible types.
    #[error("search attribute `{name}` is already registered as {existing:?}, not {requested:?}")]
    ConflictingType {
        /// Attribute name that was already registered.
        name: String,
        /// Existing declared type.
        existing: SearchAttributeType,
        /// Requested incompatible type.
        requested: SearchAttributeType,
    },
    /// A value was validated for an attribute name that is not registered.
    #[error("search attribute `{name}` is not registered")]
    UnregisteredAttribute {
        /// Attribute name missing from the schema.
        name: String,
    },
    /// A value's concrete type did not match the registered type.
    #[error("search attribute `{name}` expected {expected:?}, got {actual:?}")]
    TypeMismatch {
        /// Attribute name whose value failed validation.
        name: String,
        /// Registered expected type.
        expected: SearchAttributeType,
        /// Actual type derived from the supplied value.
        actual: SearchAttributeType,
    },
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, Utc};

    use super::{
        SearchAttributeError, SearchAttributeSchema, SearchAttributeType, SearchAttributeValue,
    };

    fn recorded_at() -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000, 123_000_000).unwrap_or_default()
    }

    #[test]
    fn values_report_matching_attribute_types() {
        let values = [
            SearchAttributeValue::String(String::from("customer-123")),
            SearchAttributeValue::Int(42),
            SearchAttributeValue::Float(12.5),
            SearchAttributeValue::Bool(true),
            SearchAttributeValue::Datetime(recorded_at()),
            SearchAttributeValue::KeywordList(vec![String::from("vip"), String::from("west")]),
        ];
        let expected_types = [
            SearchAttributeType::String,
            SearchAttributeType::Int,
            SearchAttributeType::Float,
            SearchAttributeType::Bool,
            SearchAttributeType::Datetime,
            SearchAttributeType::KeywordList,
        ];

        for (value, expected_type) in values.iter().zip(expected_types) {
            assert_eq!(value.attribute_type(), expected_type);
        }
    }

    #[test]
    fn search_attribute_types_round_trip_through_json() -> Result<(), Box<dyn std::error::Error>> {
        let attribute_types = [
            SearchAttributeType::String,
            SearchAttributeType::Int,
            SearchAttributeType::Float,
            SearchAttributeType::Bool,
            SearchAttributeType::Datetime,
            SearchAttributeType::KeywordList,
        ];

        for attribute_type in attribute_types {
            let json = serde_json::to_string(&attribute_type)?;
            let decoded = serde_json::from_str::<SearchAttributeType>(&json)?;
            assert_eq!(attribute_type, decoded);
        }
        Ok(())
    }

    #[test]
    fn search_attribute_values_round_trip_through_json() -> Result<(), Box<dyn std::error::Error>> {
        let values = [
            SearchAttributeValue::String(String::from("customer-123")),
            SearchAttributeValue::Int(42),
            SearchAttributeValue::Float(12.5),
            SearchAttributeValue::Bool(true),
            SearchAttributeValue::Datetime(recorded_at()),
            SearchAttributeValue::KeywordList(vec![String::from("vip"), String::from("west")]),
        ];

        for value in values {
            let json = serde_json::to_string(&value)?;
            let decoded = serde_json::from_str::<SearchAttributeValue>(&json)?;
            assert_eq!(value, decoded);
        }
        Ok(())
    }

    #[test]
    fn schema_registers_and_validates_matching_types() -> Result<(), Box<dyn std::error::Error>> {
        let mut schema = SearchAttributeSchema::new();
        schema.register("customer_id", SearchAttributeType::String)?;
        schema.register("customer_id", SearchAttributeType::String)?;

        schema.validate(
            "customer_id",
            &SearchAttributeValue::String(String::from("customer-123")),
        )?;
        Ok(())
    }

    #[test]
    fn registering_same_name_with_different_type_errors() -> Result<(), Box<dyn std::error::Error>>
    {
        let mut schema = SearchAttributeSchema::new();
        schema.register("customer_id", SearchAttributeType::String)?;

        assert_eq!(
            schema.register("customer_id", SearchAttributeType::Int),
            Err(SearchAttributeError::ConflictingType {
                name: String::from("customer_id"),
                existing: SearchAttributeType::String,
                requested: SearchAttributeType::Int,
            })
        );
        Ok(())
    }

    #[test]
    fn validating_unregistered_attribute_errors() {
        let schema = SearchAttributeSchema::new();

        assert_eq!(
            schema.validate(
                "customer_id",
                &SearchAttributeValue::String(String::from("customer-123"))
            ),
            Err(SearchAttributeError::UnregisteredAttribute {
                name: String::from("customer_id"),
            })
        );
    }

    #[test]
    fn validating_mismatched_type_errors() -> Result<(), Box<dyn std::error::Error>> {
        let mut schema = SearchAttributeSchema::new();
        schema.register("customer_id", SearchAttributeType::String)?;

        assert_eq!(
            schema.validate("customer_id", &SearchAttributeValue::Int(42)),
            Err(SearchAttributeError::TypeMismatch {
                name: String::from("customer_id"),
                expected: SearchAttributeType::String,
                actual: SearchAttributeType::Int,
            })
        );
        Ok(())
    }
}
