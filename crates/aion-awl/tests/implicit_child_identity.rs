//! Parent routing identity is part of every implicit child workflow type.

use aion_awl::{emit_artifact, parse};

const BODY: &str = r"
//! Parent-identity collision proof.
workflow PARENT
  input items: [String]
  outcome done: type Done, route success

type Done { count: Int }

worker proof
  action first(item: String) -> String
  action second(item: String) -> String

step fan
  distribute item in items
step one
  first(item: item) -> prepared
step two
  second(item: prepared) -> result
step gather
  collect result -> results
  results |> count -> total
  route done(count: total)
";

#[test]
fn same_shape_parents_receive_distinct_reserved_child_types()
-> Result<(), Box<dyn std::error::Error>> {
    let left = emit_artifact(&parse(&BODY.replace("PARENT", "alpha_flow"))?)?;
    let right = emit_artifact(&parse(&BODY.replace("PARENT", "beta_flow"))?)?;
    let [left_child] = left.synthesized_workflows.as_slice() else {
        return Err("alpha did not emit exactly one child".into());
    };
    let [right_child] = right.synthesized_workflows.as_slice() else {
        return Err("beta did not emit exactly one child".into());
    };
    assert_ne!(left_child.workflow_type, right_child.workflow_type);
    assert!(
        left_child
            .workflow_type
            .starts_with("aion_internal_awl_child_alpha_flow_")
    );
    assert!(
        right_child
            .workflow_type
            .starts_with("aion_internal_awl_child_beta_flow_")
    );
    assert_eq!(
        left.project_metadata()["synthesized_workflows"][0]["workflow_type"],
        left_child.workflow_type
    );
    Ok(())
}
