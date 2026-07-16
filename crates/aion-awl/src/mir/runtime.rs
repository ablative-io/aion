//! The closed runtime-capability surface (AWL-BC-IR.md §6 / IR-18).
//!
//! `RuntimeFn` is the complete import surface = the tree-shake manifest
//! (D-AOT2). `lower` may mint imports from this table only; `verify` (S1)
//! fails on anything else. `signature` is the ONE static
//! `RuntimeFn -> (module_atom, function_atom, arity)` table; the emitted `ImpT`
//! is exactly the used subset in first-use order (IR-24).

use super::tydesc::Leaf;

/// A durable-operation family (S16): the derived, park-point-granularity
/// capability manifest layered above the flat import table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum DurableFamily {
    Timers,
    Activities,
    Children,
    Signals,
}

/// The closed set of runtime functions generated code may call (§6). Includes
/// `LeafCodec(Leaf)` per the §6 capability row `leaf ×{_codec/0,…}` (the
/// Appendix A sketch omitted it — recorded in `AWL-BC-IR.md` §6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RuntimeFn {
    // aion@workflow
    WfDefine,
    WfRun,
    WfAll,
    WfMap,
    WfMapSettled,
    WfSpawn,
    WfSpawnAndWait,
    WfReceive,
    WfWithTimeout,
    WfSleep,
    WfId,
    // aion@activity
    ActNew,
    ActTaskQueue,
    ActRetry,
    ActTimeout,
    ActNode,
    // aion@awl@error
    ErrCodec,
    MapActivityError,
    MapReceiveError,
    MapChildError,
    MapSpawnError,
    MapTimerError,
    MapEngineError,
    // aion@awl@codec
    LeafCodec(Leaf),
    LeafToJson(Leaf),
    LeafDecoder(Leaf),
    NilCodec,
    RawCodec,
    Decoded,
    JsonValueCodec,
    // aion@awl@runtime
    RtRun,
    RtIndex,
    // aion@codec / aion@duration / aion@error / aion@signal / aion@child
    JsonCodec,
    DurationMs,
    ErrorTerminal,
    SignalNew,
    ChildAwait,
    // gleam@json
    JObject,
    JString,
    JArray,
    JNullable,
    JToString,
    // gleam@dynamic@decode. NOTE: there is deliberately NO `DString` row —
    // `decode.string` is a Gleam CONSTANT (inlined at use sites; the module
    // exports only `decode_string/1`), so a `string/0` import row would be
    // validator-clean and runtime-undefined. Generated code reaches the
    // string decoder through `awlc.string_decoder/0` (`LeafDecoder(Str)`),
    // proven by `tests/runtime_codecs.rs` (BC-2b-5 runtime proof finding).
    DField,
    DOptionalField,
    DSuccess,
    DFailure,
    DThen,
    DMap,
    DList,
    DOptional,
    // gleam@list
    LFlatten,
    LFilter,
    LMap,
    LAny,
    LAll,
    LSort,
    LLength,
    LTryFold,
    LFold,
    LReverse,
    LIsEmpty,
    // gleam@option / compares / string
    OIsSome,
    OIsNone,
    CmpInt,
    CmpFloat,
    CmpString,
    StrAppend,
    // R1 fallback ONLY (unused in the primary design; marked row in §6).
    ResultTry,
    // Bif-position only (§6): the Increment burst's `gc_bif2 erlang:'+'`
    // target. beamr's `Bif` instruction resolves its BIF through the import
    // table, so this needs an ImpT row like real OTP `.beam` files carry —
    // but `lower` never mints it as a `CallRt`/`TailRt` callee (`verify`
    // rejects that alongside `ResultTry`).
    IntAdd,
}

impl RuntimeFn {
    /// The `(module_atom, function_atom, arity)` this call lowers to. The
    /// module atom is the Gleam-mangled form (`aion@workflow`, …).
    pub(crate) fn signature(self) -> (&'static str, String, u32) {
        let (module, function, arity) = self.raw_signature();
        (module, function.to_owned(), arity)
    }

    /// Every `(module, function, arity)` the lowering may mint, with the
    /// leaf-parametrised rows expanded over every [`Leaf`] — the complete
    /// static import surface of generated code.
    ///
    /// This exists for the bundle guard in `aion-awl-package`: iterating
    /// this table against the embedded SDK closure's export sets makes
    /// lowering/bundle skew (a `RuntimeFn` row the shipped beams do not
    /// export) a test failure instead of an `undef` crash in a running VM.
    /// The variant list below is kept honest by `variant_ordinal` in this
    /// module's tests: adding a `RuntimeFn` variant breaks that exhaustive
    /// match at compile time, and its test fails until the variant is
    /// enumerated here.
    #[must_use]
    pub fn import_surface() -> Vec<(&'static str, String, u32)> {
        all_runtime_fns().into_iter().map(Self::signature).collect()
    }

    fn raw_signature(self) -> (&'static str, &'static str, u32) {
        match self {
            Self::WfDefine => ("aion@workflow", "define", 5),
            Self::WfRun => ("aion@workflow", "run", 1),
            Self::WfAll => ("aion@workflow", "all", 1),
            Self::WfMap => ("aion@workflow", "map", 2),
            Self::WfMapSettled => ("aion@workflow", "map_settled", 2),
            Self::WfSpawn => ("aion@workflow", "spawn", 6),
            Self::WfSpawnAndWait => ("aion@workflow", "spawn_and_wait", 6),
            Self::WfReceive => ("aion@workflow", "receive", 1),
            Self::WfWithTimeout => ("aion@workflow", "with_timeout", 2),
            Self::WfSleep => ("aion@workflow", "sleep", 1),
            Self::WfId => ("aion@workflow", "id", 0),
            Self::ActNew => ("aion@activity", "new", 5),
            Self::ActTaskQueue => ("aion@activity", "task_queue", 2),
            Self::ActRetry => ("aion@activity", "retry", 2),
            Self::ActTimeout => ("aion@activity", "timeout", 2),
            Self::ActNode => ("aion@activity", "node", 2),
            Self::ErrCodec => ("aion@awl@error", "codec", 0),
            Self::MapActivityError => ("aion@awl@error", "map_activity_error", 1),
            Self::MapReceiveError => ("aion@awl@error", "map_receive_error", 1),
            Self::MapChildError => ("aion@awl@error", "map_child_error", 1),
            Self::MapSpawnError => ("aion@awl@error", "map_spawn_error", 1),
            Self::MapTimerError => ("aion@awl@error", "map_timer_error", 1),
            Self::MapEngineError => ("aion@awl@error", "map_engine_error", 1),
            Self::LeafCodec(leaf) => ("aion@awl@codec", leaf_fn(leaf, "codec"), 0),
            Self::LeafToJson(leaf) => ("aion@awl@codec", leaf_fn(leaf, "to_json"), 1),
            Self::LeafDecoder(leaf) => ("aion@awl@codec", leaf_fn(leaf, "decoder"), 0),
            Self::NilCodec => ("aion@awl@codec", "nil_codec", 0),
            Self::RawCodec => ("aion@awl@codec", "raw", 0),
            Self::Decoded => ("aion@awl@codec", "decoded", 3),
            Self::JsonValueCodec => ("aion@awl@codec", "json_value", 0),
            Self::RtRun => ("aion@awl@runtime", "run", 4),
            Self::RtIndex => ("aion@awl@runtime", "index", 3),
            Self::JsonCodec => ("aion@codec", "json_codec", 2),
            Self::DurationMs => ("aion@duration", "milliseconds", 1),
            Self::ErrorTerminal => ("aion@error", "terminal", 1),
            Self::SignalNew => ("aion@signal", "new", 2),
            Self::ChildAwait => ("aion@child", "await", 1),
            Self::JObject => ("gleam@json", "object", 1),
            Self::JString => ("gleam@json", "string", 1),
            Self::JArray => ("gleam@json", "array", 2),
            Self::JNullable => ("gleam@json", "nullable", 2),
            Self::JToString => ("gleam@json", "to_string", 1),
            Self::DField => ("gleam@dynamic@decode", "field", 3),
            Self::DOptionalField => ("gleam@dynamic@decode", "optional_field", 4),
            Self::DSuccess => ("gleam@dynamic@decode", "success", 1),
            Self::DFailure => ("gleam@dynamic@decode", "failure", 2),
            Self::DThen => ("gleam@dynamic@decode", "then", 2),
            Self::DMap => ("gleam@dynamic@decode", "map", 2),
            Self::DList => ("gleam@dynamic@decode", "list", 1),
            Self::DOptional => ("gleam@dynamic@decode", "optional", 1),
            Self::LFlatten => ("gleam@list", "flatten", 1),
            Self::LFilter => ("gleam@list", "filter", 2),
            Self::LMap => ("gleam@list", "map", 2),
            Self::LAny => ("gleam@list", "any", 2),
            Self::LAll => ("gleam@list", "all", 2),
            Self::LSort => ("gleam@list", "sort", 2),
            Self::LLength => ("gleam@list", "length", 1),
            Self::LTryFold => ("gleam@list", "try_fold", 3),
            Self::LFold => ("gleam@list", "fold", 3),
            Self::LReverse => ("gleam@list", "reverse", 1),
            Self::LIsEmpty => ("gleam@list", "is_empty", 1),
            Self::OIsSome => ("gleam@option", "is_some", 1),
            Self::OIsNone => ("gleam@option", "is_none", 1),
            Self::CmpInt => ("gleam@int", "compare", 2),
            Self::CmpFloat => ("gleam@float", "compare", 2),
            Self::CmpString => ("gleam@string", "compare", 2),
            Self::StrAppend => ("gleam@string", "append", 2),
            Self::ResultTry => ("gleam@result", "try", 2),
            Self::IntAdd => ("erlang", "+", 2),
        }
    }

    /// The durable family this call belongs to (S16), when it is a park point.
    pub(crate) fn durable_family(self) -> Option<DurableFamily> {
        match self {
            Self::WfSleep | Self::WfWithTimeout => Some(DurableFamily::Timers),
            Self::WfRun
            | Self::WfAll
            | Self::WfMap
            | Self::WfMapSettled
            | Self::ActNew
            | Self::ActTaskQueue
            | Self::ActRetry
            | Self::ActTimeout
            | Self::ActNode => Some(DurableFamily::Activities),
            Self::WfSpawn | Self::WfSpawnAndWait | Self::ChildAwait => {
                Some(DurableFamily::Children)
            }
            Self::WfReceive | Self::SignalNew => Some(DurableFamily::Signals),
            _ => None,
        }
    }

    /// A stable identifier used in golden output and `verify` diagnostics.
    pub(crate) fn label(self) -> String {
        let (module, function, arity) = self.signature();
        format!("{module}:{function}/{arity}")
    }
}

/// Every [`Leaf`], for expanding the leaf-parametrised runtime rows.
const ALL_LEAVES: [Leaf; 5] = [Leaf::Bool, Leaf::Int, Leaf::Float, Leaf::Str, Leaf::Nil];

/// Every [`RuntimeFn`] value (leaf rows expanded), backing
/// [`RuntimeFn::import_surface`]. Kept exhaustive by the `variant_ordinal`
/// staleness guard in this module's tests.
fn all_runtime_fns() -> Vec<RuntimeFn> {
    let mut all = vec![
        RuntimeFn::WfDefine,
        RuntimeFn::WfRun,
        RuntimeFn::WfAll,
        RuntimeFn::WfMap,
        RuntimeFn::WfMapSettled,
        RuntimeFn::WfSpawn,
        RuntimeFn::WfSpawnAndWait,
        RuntimeFn::WfReceive,
        RuntimeFn::WfWithTimeout,
        RuntimeFn::WfSleep,
        RuntimeFn::WfId,
        RuntimeFn::ActNew,
        RuntimeFn::ActTaskQueue,
        RuntimeFn::ActRetry,
        RuntimeFn::ActTimeout,
        RuntimeFn::ActNode,
        RuntimeFn::ErrCodec,
        RuntimeFn::MapActivityError,
        RuntimeFn::MapReceiveError,
        RuntimeFn::MapChildError,
        RuntimeFn::MapSpawnError,
        RuntimeFn::MapTimerError,
        RuntimeFn::MapEngineError,
        RuntimeFn::NilCodec,
        RuntimeFn::RawCodec,
        RuntimeFn::Decoded,
        RuntimeFn::JsonValueCodec,
        RuntimeFn::RtRun,
        RuntimeFn::RtIndex,
        RuntimeFn::JsonCodec,
        RuntimeFn::DurationMs,
        RuntimeFn::ErrorTerminal,
        RuntimeFn::SignalNew,
        RuntimeFn::ChildAwait,
        RuntimeFn::JObject,
        RuntimeFn::JString,
        RuntimeFn::JArray,
        RuntimeFn::JNullable,
        RuntimeFn::JToString,
        RuntimeFn::DField,
        RuntimeFn::DOptionalField,
        RuntimeFn::DSuccess,
        RuntimeFn::DFailure,
        RuntimeFn::DThen,
        RuntimeFn::DMap,
        RuntimeFn::DList,
        RuntimeFn::DOptional,
        RuntimeFn::LFlatten,
        RuntimeFn::LFilter,
        RuntimeFn::LMap,
        RuntimeFn::LAny,
        RuntimeFn::LAll,
        RuntimeFn::LSort,
        RuntimeFn::LLength,
        RuntimeFn::LTryFold,
        RuntimeFn::LFold,
        RuntimeFn::LReverse,
        RuntimeFn::LIsEmpty,
        RuntimeFn::OIsSome,
        RuntimeFn::OIsNone,
        RuntimeFn::CmpInt,
        RuntimeFn::CmpFloat,
        RuntimeFn::CmpString,
        RuntimeFn::StrAppend,
        RuntimeFn::ResultTry,
        RuntimeFn::IntAdd,
    ];
    for leaf in ALL_LEAVES {
        all.push(RuntimeFn::LeafCodec(leaf));
        all.push(RuntimeFn::LeafToJson(leaf));
        all.push(RuntimeFn::LeafDecoder(leaf));
    }
    all
}

fn leaf_fn(leaf: Leaf, suffix: &str) -> &'static str {
    match (leaf, suffix) {
        (Leaf::Bool, "codec") => "bool_codec",
        (Leaf::Bool, "to_json") => "bool_to_json",
        (Leaf::Bool, _) => "bool_decoder",
        (Leaf::Int, "codec") => "int_codec",
        (Leaf::Int, "to_json") => "int_to_json",
        (Leaf::Int, _) => "int_decoder",
        (Leaf::Float, "codec") => "float_codec",
        (Leaf::Float, "to_json") => "float_to_json",
        (Leaf::Float, _) => "float_decoder",
        (Leaf::Str, "codec") => "string_codec",
        (Leaf::Str, "to_json") => "string_to_json",
        (Leaf::Str, _) => "string_decoder",
        (Leaf::Nil, "codec") => "nil_codec",
        (Leaf::Nil, "to_json") => "nil_to_json",
        (Leaf::Nil, _) => "nil_decoder",
    }
}

#[cfg(test)]
mod tests {
    use super::{ALL_LEAVES, Leaf, RuntimeFn, all_runtime_fns};

    /// Compile-time staleness anchor for [`all_runtime_fns`]: a NEW
    /// `RuntimeFn` variant breaks this exhaustive match (no wildcard arm) so
    /// the enumeration behind `import_surface` cannot silently lag the enum.
    /// The leaf-parametrised rows are anchored by [`leaf_ordinal`] the same
    /// way.
    fn variant_ordinal(function: RuntimeFn) -> usize {
        match function {
            RuntimeFn::WfDefine => 0,
            RuntimeFn::WfRun => 1,
            RuntimeFn::WfAll => 2,
            RuntimeFn::WfMap => 3,
            RuntimeFn::WfMapSettled => 4,
            RuntimeFn::WfSpawn => 5,
            RuntimeFn::WfSpawnAndWait => 6,
            RuntimeFn::WfReceive => 7,
            RuntimeFn::WfWithTimeout => 8,
            RuntimeFn::WfSleep => 9,
            RuntimeFn::WfId => 10,
            RuntimeFn::ActNew => 11,
            RuntimeFn::ActTaskQueue => 12,
            RuntimeFn::ActRetry => 13,
            RuntimeFn::ActTimeout => 14,
            RuntimeFn::ActNode => 15,
            RuntimeFn::ErrCodec => 16,
            RuntimeFn::MapActivityError => 17,
            RuntimeFn::MapReceiveError => 18,
            RuntimeFn::MapChildError => 19,
            RuntimeFn::MapSpawnError => 20,
            RuntimeFn::MapTimerError => 21,
            RuntimeFn::MapEngineError => 22,
            RuntimeFn::NilCodec => 23,
            RuntimeFn::RawCodec => 24,
            RuntimeFn::Decoded => 25,
            RuntimeFn::JsonValueCodec => 26,
            RuntimeFn::RtRun => 27,
            RuntimeFn::RtIndex => 28,
            RuntimeFn::JsonCodec => 29,
            RuntimeFn::DurationMs => 30,
            RuntimeFn::ErrorTerminal => 31,
            RuntimeFn::SignalNew => 32,
            RuntimeFn::ChildAwait => 33,
            RuntimeFn::JObject => 34,
            RuntimeFn::JString => 35,
            RuntimeFn::JArray => 36,
            RuntimeFn::JNullable => 37,
            RuntimeFn::JToString => 38,
            RuntimeFn::DField => 39,
            RuntimeFn::DOptionalField => 40,
            RuntimeFn::DSuccess => 41,
            RuntimeFn::DFailure => 42,
            RuntimeFn::DThen => 43,
            RuntimeFn::DMap => 44,
            RuntimeFn::DList => 45,
            RuntimeFn::DOptional => 46,
            RuntimeFn::LFlatten => 47,
            RuntimeFn::LFilter => 48,
            RuntimeFn::LMap => 49,
            RuntimeFn::LAny => 50,
            RuntimeFn::LAll => 51,
            RuntimeFn::LSort => 52,
            RuntimeFn::LLength => 53,
            RuntimeFn::LTryFold => 54,
            RuntimeFn::LFold => 55,
            RuntimeFn::LReverse => 56,
            RuntimeFn::LIsEmpty => 57,
            RuntimeFn::OIsSome => 58,
            RuntimeFn::OIsNone => 59,
            RuntimeFn::CmpInt => 60,
            RuntimeFn::CmpFloat => 61,
            RuntimeFn::CmpString => 62,
            RuntimeFn::StrAppend => 63,
            RuntimeFn::ResultTry => 64,
            RuntimeFn::IntAdd => 65,
            RuntimeFn::LeafCodec(leaf) => 66 + 3 * leaf_ordinal(leaf),
            RuntimeFn::LeafToJson(leaf) => 67 + 3 * leaf_ordinal(leaf),
            RuntimeFn::LeafDecoder(leaf) => 68 + 3 * leaf_ordinal(leaf),
        }
    }

    /// Compile-time staleness anchor for [`ALL_LEAVES`] (no wildcard arm).
    fn leaf_ordinal(leaf: Leaf) -> usize {
        match leaf {
            Leaf::Bool => 0,
            Leaf::Int => 1,
            Leaf::Float => 2,
            Leaf::Str => 3,
            Leaf::Nil => 4,
        }
    }

    /// `all_runtime_fns` enumerates every variant exactly once: 66 plain
    /// variants plus 3 leaf-parametrised rows over the 5 leaves.
    #[test]
    fn enumeration_covers_every_variant_exactly_once() {
        let all = all_runtime_fns();
        let expected = 66 + 3 * ALL_LEAVES.len();
        assert_eq!(all.len(), expected);
        let mut seen = vec![false; expected];
        for function in all {
            let ordinal = variant_ordinal(function);
            assert!(
                !seen[ordinal],
                "duplicate enumeration of {}",
                function.label()
            );
            seen[ordinal] = true;
        }
        assert!(seen.iter().all(|covered| *covered));
    }

    /// The projected surface carries one row per enumerated function, in
    /// enumeration order, and each row is the function's own signature.
    #[test]
    fn import_surface_projects_the_full_enumeration() {
        let surface = RuntimeFn::import_surface();
        let all = all_runtime_fns();
        assert_eq!(surface.len(), all.len());
        for (row, function) in surface.iter().zip(all) {
            assert_eq!(*row, function.signature());
        }
    }
}
