//// The stack ordering logic: turn a plan's `depends_on` graph into an ordered
//// list of STRATA (dependency layers). Units in the same stratum are mutually
//// independent — they branch on already-landed work only, so the parent
//// workflow can dev them in PARALLEL (fan-out/collect); units in a later
//// stratum branch on an earlier stratum's landed result and run after it.
////
//// This module is pure: no effects, no engine, no I/O. It is the intellectual
//// heart of the pipeline and is exhaustively unit-tested (`test/stack_test`),
//// because a wrong ordering silently corrupts every downstream branch.
////
//// The algorithm is a layered (Kahn) topological sort with STABLE ordering:
//// within a stratum, and across strata, units keep the plan's original order,
//// so the same plan always produces the same strata (determinism the workflow
//// engine requires). Every rejection names the offending unit(s): a cycle is
//// reported with its residual units, an unknown dependency with both ends — an
//// actionable error, never a bare "invalid".

import gleam/list
import gleam/set.{type Set}
import gleam/string
import pipeline_run/types.{
  type PlanUnit, type StackError, DependencyCycle, DuplicateUnit, EmptyPlan,
  SelfDependency, UnknownDependency,
}

/// Resolve the layered strata for a plan's units, or reject the plan with a
/// pointed [`StackError`].
///
/// Guarantees on `Ok(strata)`:
/// - every unit id appears in exactly one stratum,
/// - a unit's stratum is strictly after every unit it depends on,
/// - within a stratum no unit depends on another in the same stratum, and
/// - order (both of strata and within each stratum) follows the plan's input
///   order — the sort is stable and total.
///
/// Rejections (checked in this order, so the earliest structural fault wins):
/// `EmptyPlan`, `DuplicateUnit`, `SelfDependency`, `UnknownDependency`,
/// `DependencyCycle`.
pub fn stratify(
  units: List(PlanUnit),
) -> Result(List(List(String)), StackError) {
  case units {
    [] -> Error(EmptyPlan)
    _ -> {
      use _ <- with(check_unique(units))
      use _known <- with(check_dependencies(units))
      layer(units, set.new(), [])
    }
  }
}

/// Resolve a cap: the caller's value if it is a sane ceiling (>= 1), otherwise
/// the supplied default. A cap below 1 would forbid the very first round, which
/// can never be the author's intent, so it falls back to the default rather
/// than deadlocking the loop — an overridable default, never a silent zero.
pub fn resolve_cap(provided: Int, default: Int) -> Int {
  case provided >= 1 {
    True -> provided
    False -> default
  }
}

/// Render a [`StackError`] as a single actionable line naming the offending
/// unit(s) — the message the parent workflow carries in a `StackInvalid` error.
pub fn stack_error_message(stack_error: StackError) -> String {
  case stack_error {
    EmptyPlan -> "the plan proposed no units"
    DuplicateUnit(unit_id) ->
      "duplicate unit id `" <> unit_id <> "` in the plan"
    SelfDependency(unit_id) -> "unit `" <> unit_id <> "` depends on itself"
    UnknownDependency(unit_id, missing) ->
      "unit `"
      <> unit_id
      <> "` depends on `"
      <> missing
      <> "`, which is not a unit in the plan"
    DependencyCycle(remaining) ->
      "dependency cycle among units: " <> string.join(remaining, ", ")
  }
}

// --- internals -------------------------------------------------------------

/// Reject a plan whose unit ids are not unique. A duplicate id makes "the unit
/// this branch depends on" ambiguous, so it is a hard structural fault.
fn check_unique(units: List(PlanUnit)) -> Result(Nil, StackError) {
  fold_until(units, set.new(), fn(seen, unit) {
    case set.contains(seen, unit.unit_id) {
      True -> Error(DuplicateUnit(unit.unit_id))
      False -> Ok(set.insert(seen, unit.unit_id))
    }
  })
  |> result_replace(Nil)
}

/// Reject self-dependencies and dangling dependencies, returning the set of
/// known unit ids for the topological pass. Every `depends_on` entry must name
/// a DIFFERENT unit that exists in the plan.
fn check_dependencies(
  units: List(PlanUnit),
) -> Result(Set(String), StackError) {
  let known = units |> list.map(fn(unit) { unit.unit_id }) |> set.from_list
  let validation =
    fold_until(units, Nil, fn(_acc, unit) {
      fold_until(unit.depends_on, Nil, fn(_inner, dependency) {
        case dependency == unit.unit_id {
          True -> Error(SelfDependency(unit.unit_id))
          False ->
            case set.contains(known, dependency) {
              True -> Ok(Nil)
              False -> Error(UnknownDependency(unit.unit_id, dependency))
            }
        }
      })
    })
  case validation {
    Ok(_) -> Ok(known)
    Error(stack_error) -> Error(stack_error)
  }
}

/// Extract layers until every unit is placed. Each layer is the set of
/// still-unplaced units whose dependencies are ALL already placed; if no such
/// unit exists while units remain, the residue is a dependency cycle.
fn layer(
  remaining: List(PlanUnit),
  placed: Set(String),
  acc: List(List(String)),
) -> Result(List(List(String)), StackError) {
  case remaining {
    [] -> Ok(list.reverse(acc))
    _ -> {
      let ready =
        list.filter(remaining, fn(unit) {
          list.all(unit.depends_on, fn(dependency) {
            set.contains(placed, dependency)
          })
        })
      case ready {
        [] ->
          Error(DependencyCycle(list.map(remaining, fn(unit) { unit.unit_id })))
        _ -> {
          let ready_ids = list.map(ready, fn(unit) { unit.unit_id })
          let next_placed =
            list.fold(ready_ids, placed, fn(current, id) {
              set.insert(current, id)
            })
          let ready_set = set.from_list(ready_ids)
          let next_remaining =
            list.filter(remaining, fn(unit) {
              !set.contains(ready_set, unit.unit_id)
            })
          layer(next_remaining, next_placed, [ready_ids, ..acc])
        }
      }
    }
  }
}

/// `use`-friendly bind over `Result` with our [`StackError`].
fn with(
  result: Result(a, StackError),
  next: fn(a) -> Result(b, StackError),
) -> Result(b, StackError) {
  case result {
    Ok(value) -> next(value)
    Error(stack_error) -> Error(stack_error)
  }
}

/// Fold over `items`, short-circuiting on the first `Error` the step returns.
fn fold_until(
  items: List(a),
  initial: acc,
  step: fn(acc, a) -> Result(acc, StackError),
) -> Result(acc, StackError) {
  case items {
    [] -> Ok(initial)
    [first, ..rest] ->
      case step(initial, first) {
        Ok(next) -> fold_until(rest, next, step)
        Error(stack_error) -> Error(stack_error)
      }
  }
}

/// Replace an `Ok` value while preserving an `Error`.
fn result_replace(result: Result(a, e), value: b) -> Result(b, e) {
  case result {
    Ok(_) -> Ok(value)
    Error(error) -> Error(error)
  }
}
