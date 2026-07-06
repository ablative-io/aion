//// Unit tests for the stack ordering logic (`pipeline_run/stack`): layered
//// topological strata, stable ordering, and every pointed rejection.

import gleeunit/should
import pipeline_run/stack
import pipeline_run/types.{
  type PlanUnit, DependencyCycle, DuplicateUnit, EmptyPlan, PlanUnit,
  SelfDependency, UnknownDependency,
}

/// A unit with the given id and dependencies; goal/files are irrelevant to
/// ordering so they are fixed.
fn unit(id: String, depends_on: List(String)) -> PlanUnit {
  PlanUnit(
    unit_id: id,
    goal: "goal " <> id,
    files_hint: [],
    depends_on: depends_on,
  )
}

pub fn empty_plan_is_rejected_test() {
  stack.stratify([])
  |> should.equal(Error(EmptyPlan))
}

pub fn single_unit_is_one_stratum_test() {
  stack.stratify([unit("a", [])])
  |> should.equal(Ok([["a"]]))
}

pub fn independent_units_share_one_stratum_test() {
  // No edges: all three are mutually independent -> one parallel stratum, in
  // input order.
  stack.stratify([unit("a", []), unit("b", []), unit("c", [])])
  |> should.equal(Ok([["a", "b", "c"]]))
}

pub fn a_linear_chain_is_one_unit_per_stratum_test() {
  // a <- b <- c : strictly sequential.
  stack.stratify([unit("a", []), unit("b", ["a"]), unit("c", ["b"])])
  |> should.equal(Ok([["a"], ["b"], ["c"]]))
}

pub fn a_diamond_has_a_parallel_middle_stratum_test() {
  //     a
  //    / \
  //   b   c     b and c both depend on a and are independent of each other
  //    \ /
  //     d       d depends on both
  stack.stratify([
    unit("a", []),
    unit("b", ["a"]),
    unit("c", ["a"]),
    unit("d", ["b", "c"]),
  ])
  |> should.equal(Ok([["a"], ["b", "c"], ["d"]]))
}

pub fn ordering_within_a_stratum_is_stable_input_order_test() {
  // Two roots given c-before-b: the stratum keeps input order, not sorted.
  stack.stratify([unit("c", []), unit("b", []), unit("d", ["c", "b"])])
  |> should.equal(Ok([["c", "b"], ["d"]]))
}

pub fn a_dependency_declared_before_its_dependent_still_layers_test() {
  // Dependency need not appear textually first; layering resolves regardless.
  stack.stratify([unit("late", ["early"]), unit("early", [])])
  |> should.equal(Ok([["early"], ["late"]]))
}

pub fn a_duplicate_unit_id_is_rejected_by_id_test() {
  stack.stratify([unit("a", []), unit("b", []), unit("a", [])])
  |> should.equal(Error(DuplicateUnit("a")))
}

pub fn a_self_dependency_is_rejected_test() {
  stack.stratify([unit("a", []), unit("b", ["b"])])
  |> should.equal(Error(SelfDependency("b")))
}

pub fn an_unknown_dependency_names_both_ends_test() {
  stack.stratify([unit("a", []), unit("b", ["ghost"])])
  |> should.equal(Error(UnknownDependency("b", "ghost")))
}

pub fn a_two_node_cycle_is_rejected_with_its_residue_test() {
  // a <-> b : neither can ever be placed.
  stack.stratify([unit("a", ["b"]), unit("b", ["a"])])
  |> should.equal(Error(DependencyCycle(["a", "b"])))
}

pub fn a_cycle_reports_only_the_residual_units_not_the_placed_ones_test() {
  // root places cleanly; x <- y <- x is the residue.
  stack.stratify([
    unit("root", []),
    unit("x", ["root", "y"]),
    unit("y", ["x"]),
  ])
  |> should.equal(Error(DependencyCycle(["x", "y"])))
}

pub fn resolve_cap_keeps_a_sane_override_test() {
  stack.resolve_cap(7, 4)
  |> should.equal(7)
}

pub fn resolve_cap_falls_back_when_below_one_test() {
  // A zero or negative cap would forbid the first round: fall back to default.
  stack.resolve_cap(0, 4)
  |> should.equal(4)
  stack.resolve_cap(-3, 2)
  |> should.equal(2)
}
