//! Order-preserving JSON document model for schema parsing.
//!
//! `serde_json::Value` stores objects in sorted key order, but generation
//! must preserve the property order schema authors wrote (design rule:
//! property order in the generated module follows the JSON file). Rather
//! than enabling `serde_json`'s `preserve_order` cargo feature — which feature
//! unification would silently switch on for every crate in the workspace,
//! changing map ordering in manifest serialisation that feeds content
//! hashes — this module deserialises through `serde_json`'s parser into a
//! local value type whose objects are ordered entry lists.

use serde::Deserialize;
use serde::de::{self, Deserializer, MapAccess, SeqAccess, Visitor};

/// A JSON value whose object members preserve document order.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum OrderedValue {
    /// JSON `null`.
    Null,
    /// JSON `true` / `false`.
    Bool(bool),
    /// Any JSON number.
    Number(serde_json::Number),
    /// A JSON string.
    String(String),
    /// A JSON array in document order.
    Array(Vec<OrderedValue>),
    /// A JSON object as `(key, value)` entries in document order.
    Object(Vec<(String, OrderedValue)>),
}

impl OrderedValue {
    /// JSON type name for diagnostics (`object`, `array`, `string`, ...).
    pub(crate) fn type_name(&self) -> &'static str {
        match self {
            OrderedValue::Null => "null",
            OrderedValue::Bool(_) => "boolean",
            OrderedValue::Number(_) => "number",
            OrderedValue::String(_) => "string",
            OrderedValue::Array(_) => "array",
            OrderedValue::Object(_) => "object",
        }
    }

    /// The string content when this value is a JSON string.
    pub(crate) fn as_str(&self) -> Option<&str> {
        match self {
            OrderedValue::String(text) => Some(text),
            _ => None,
        }
    }
}

/// Parses JSON bytes into an [`OrderedValue`], rejecting duplicate object
/// keys (a duplicated property would make "preserve property order"
/// ambiguous and always indicates an authoring mistake).
pub(crate) fn parse_ordered(bytes: &[u8]) -> Result<OrderedValue, serde_json::Error> {
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    let value = OrderedValue::deserialize(&mut deserializer)?;
    deserializer.end()?;
    Ok(value)
}

impl<'de> Deserialize<'de> for OrderedValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(OrderedValueVisitor)
    }
}

struct OrderedValueVisitor;

impl<'de> Visitor<'de> for OrderedValueVisitor {
    type Value = OrderedValue;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("any JSON value")
    }

    fn visit_bool<E: de::Error>(self, value: bool) -> Result<Self::Value, E> {
        Ok(OrderedValue::Bool(value))
    }

    fn visit_i64<E: de::Error>(self, value: i64) -> Result<Self::Value, E> {
        Ok(OrderedValue::Number(serde_json::Number::from(value)))
    }

    fn visit_u64<E: de::Error>(self, value: u64) -> Result<Self::Value, E> {
        Ok(OrderedValue::Number(serde_json::Number::from(value)))
    }

    fn visit_f64<E: de::Error>(self, value: f64) -> Result<Self::Value, E> {
        serde_json::Number::from_f64(value)
            .map(OrderedValue::Number)
            .ok_or_else(|| de::Error::custom("non-finite numbers are not valid JSON"))
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
        Ok(OrderedValue::String(value.to_owned()))
    }

    fn visit_string<E: de::Error>(self, value: String) -> Result<Self::Value, E> {
        Ok(OrderedValue::String(value))
    }

    fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
        Ok(OrderedValue::Null)
    }

    fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut items = Vec::new();
        while let Some(item) = seq.next_element::<OrderedValue>()? {
            items.push(item);
        }
        Ok(OrderedValue::Array(items))
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut entries: Vec<(String, OrderedValue)> = Vec::new();
        while let Some((key, value)) = map.next_entry::<String, OrderedValue>()? {
            if entries.iter().any(|(existing, _)| *existing == key) {
                return Err(de::Error::custom(format!("duplicate object key `{key}`")));
            }
            entries.push((key, value));
        }
        Ok(OrderedValue::Object(entries))
    }
}

#[cfg(test)]
mod tests {
    use super::{OrderedValue, parse_ordered};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn objects_preserve_document_order() -> TestResult {
        let parsed = parse_ordered(br#"{"zeta": 1, "alpha": {"nested_z": true, "a": null}}"#)?;

        let OrderedValue::Object(entries) = parsed else {
            return Err("expected object".into());
        };
        assert_eq!(entries[0].0, "zeta");
        assert_eq!(entries[1].0, "alpha");
        let OrderedValue::Object(nested) = &entries[1].1 else {
            return Err("expected nested object".into());
        };
        assert_eq!(nested[0].0, "nested_z");
        assert_eq!(nested[1].0, "a");
        Ok(())
    }

    #[test]
    fn all_json_value_kinds_parse() -> TestResult {
        let parsed = parse_ordered(br#"[null, true, 7, -3, 0.5, "text", [], {}]"#)?;

        let OrderedValue::Array(items) = parsed else {
            return Err("expected array".into());
        };
        assert_eq!(items.len(), 8);
        assert_eq!(items[0], OrderedValue::Null);
        assert_eq!(items[1], OrderedValue::Bool(true));
        assert_eq!(items[5].as_str(), Some("text"));
        assert_eq!(items[6], OrderedValue::Array(Vec::new()));
        assert_eq!(items[7], OrderedValue::Object(Vec::new()));
        Ok(())
    }

    #[test]
    fn duplicate_keys_are_rejected() -> TestResult {
        let result = parse_ordered(br#"{"a": 1, "a": 2}"#);

        let Err(error) = result else {
            return Err("duplicate keys must be rejected".into());
        };
        assert!(error.to_string().contains("duplicate object key `a`"));
        Ok(())
    }

    #[test]
    fn trailing_garbage_is_rejected() {
        assert!(parse_ordered(b"{} extra").is_err());
    }

    #[test]
    fn type_names_cover_every_kind() -> TestResult {
        let parsed = parse_ordered(br#"[null, true, 1, "s", [], {}]"#)?;
        let OrderedValue::Array(items) = &parsed else {
            return Err("expected array".into());
        };

        let names: Vec<&str> = items.iter().map(OrderedValue::type_name).collect();
        assert_eq!(
            names,
            vec!["null", "boolean", "number", "string", "array", "object"]
        );
        assert_eq!(parsed.type_name(), "array");
        Ok(())
    }
}
