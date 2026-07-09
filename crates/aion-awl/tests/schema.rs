//! Golden tests for public AWL type-to-JSON-Schema derivation.

use std::error::Error;

use aion_awl::{parse, schema_for_type};

#[test]
fn brief_schema_matches_the_committed_golden() -> Result<(), Box<dyn Error>> {
    let document = parse(include_str!("fixtures/typed_contract.awl"))?;
    let schema = schema_for_type(&document, "Brief")?;
    let rendered = format!("{}\n", serde_json::to_string_pretty(&schema)?);
    assert_eq!(rendered, include_str!("fixtures/brief.schema.golden.json"));
    for forbidden in ["minLength", "pattern", "minimum", "maximum", "format"] {
        assert!(
            !rendered.contains(forbidden),
            "unexpected constraint key `{forbidden}`"
        );
    }
    Ok(())
}

#[test]
fn option_fields_are_optional_but_not_nullable() -> Result<(), Box<dyn Error>> {
    let document = parse(include_str!("fixtures/typed_contract.awl"))?;
    let schema = schema_for_type(&document, "Brief")?;
    let optional = &schema["properties"]["optional_note"];
    assert_eq!(optional["type"], "string");
    assert!(
        !schema["required"]
            .as_array()
            .ok_or("required array")?
            .iter()
            .any(|name| name == "optional_note")
    );
    assert!(optional.get("anyOf").is_none());
    Ok(())
}
