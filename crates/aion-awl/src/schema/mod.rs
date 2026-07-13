//! JSON Schema derivation for AWL contracts (the one pure public
//! derivation), plus its error type.

mod derive;
mod error;

pub use derive::{
    schema_for_outcomes, schema_for_outcomes_in, schema_for_type, schema_for_type_in,
    schema_for_workflow, schema_for_workflow_in,
};
pub use error::SchemaError;
