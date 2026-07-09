//! Golden tests for public AWL type-to-JSON-Schema derivation.

use std::error::Error;

use aion_awl::{parse, schema_for_type, schema_for_workflow};

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

#[test]
fn recursive_records_use_draft_refs() -> Result<(), Box<dyn Error>> {
    let source =
        "workflow recursive\noutput String\n\ntype Node { next: Option(Node) }\n\nfinish \"ok\"\n";
    let document = parse(source)?;
    let schema = schema_for_type(&document, "Node")?;
    assert_eq!(schema["properties"]["next"]["$ref"], "#");
    assert!(
        !schema["required"]
            .as_array()
            .ok_or("required array")?
            .iter()
            .any(|name| name == "next")
    );
    Ok(())
}

#[test]
fn nested_type_and_field_descriptions_both_survive() -> Result<(), Box<dyn Error>> {
    let source = "workflow nested\noutput String\n\n/// The reusable address object.\ntype Address { street: String }\ntype User {\n  /// Where this user receives mail.\n  address: Address\n}\n\nfinish \"ok\"\n";
    let document = parse(source)?;
    let schema = schema_for_type(&document, "User")?;
    assert_eq!(
        schema["properties"]["address"]["description"],
        "Where this user receives mail."
    );
    assert_eq!(
        schema["$defs"]["Address"]["description"],
        "The reusable address object."
    );
    Ok(())
}

#[test]
fn workflow_contract_handles_builtins_and_multiple_inputs() -> Result<(), Box<dyn Error>> {
    let source = "workflow primitive\ninput name: String\ninput note: Option(String)\noutput String\n\nfinish \"ok\"\n";
    let document = parse(source)?;
    let schema = schema_for_workflow(&document)?;
    assert_eq!(schema["properties"]["output"]["type"], "string");
    assert_eq!(
        schema["properties"]["input"]["properties"]["name"]["type"],
        "string"
    );
    assert_eq!(
        schema["properties"]["input"]["properties"]["note"]["type"],
        "string"
    );
    let required = schema["properties"]["input"]["required"]
        .as_array()
        .ok_or("input required array")?;
    assert!(required.iter().any(|name| name == "name"));
    assert!(!required.iter().any(|name| name == "note"));
    Ok(())
}
