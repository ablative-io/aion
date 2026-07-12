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
    WfSpawn,
    WfSpawnAndWait,
    WfReceive,
    WfWithTimeout,
    WfSleep,
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
    // gleam@dynamic@decode
    DField,
    DOptionalField,
    DSuccess,
    DFailure,
    DThen,
    DMap,
    DList,
    DOptional,
    DString,
    // gleam@list
    LFlatten,
    LFilter,
    LMap,
    LSort,
    LLength,
    LTryFold,
    LReverse,
    LIsEmpty,
    // gleam@option / compares / string
    OIsSome,
    OIsNone,
    CmpInt,
    CmpFloat,
    CmpString,
    CmpBool,
    StrAppend,
    // R1 fallback ONLY (unused in the primary design; marked row in §6).
    ResultTry,
}

impl RuntimeFn {
    /// The `(module_atom, function_atom, arity)` this call lowers to. The
    /// module atom is the Gleam-mangled form (`aion@workflow`, …).
    pub(crate) fn signature(self) -> (&'static str, String, u32) {
        let (module, function, arity) = self.raw_signature();
        (module, function.to_owned(), arity)
    }

    fn raw_signature(self) -> (&'static str, &'static str, u32) {
        match self {
            Self::WfDefine => ("aion@workflow", "define", 5),
            Self::WfRun => ("aion@workflow", "run", 1),
            Self::WfAll => ("aion@workflow", "all", 1),
            Self::WfMap => ("aion@workflow", "map", 2),
            Self::WfSpawn => ("aion@workflow", "spawn", 6),
            Self::WfSpawnAndWait => ("aion@workflow", "spawn_and_wait", 6),
            Self::WfReceive => ("aion@workflow", "receive", 1),
            Self::WfWithTimeout => ("aion@workflow", "with_timeout", 2),
            Self::WfSleep => ("aion@workflow", "sleep", 1),
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
            Self::DString => ("gleam@dynamic@decode", "string", 0),
            Self::LFlatten => ("gleam@list", "flatten", 1),
            Self::LFilter => ("gleam@list", "filter", 2),
            Self::LMap => ("gleam@list", "map", 2),
            Self::LSort => ("gleam@list", "sort", 2),
            Self::LLength => ("gleam@list", "length", 1),
            Self::LTryFold => ("gleam@list", "try_fold", 3),
            Self::LReverse => ("gleam@list", "reverse", 1),
            Self::LIsEmpty => ("gleam@list", "is_empty", 1),
            Self::OIsSome => ("gleam@option", "is_some", 1),
            Self::OIsNone => ("gleam@option", "is_none", 1),
            Self::CmpInt => ("gleam@int", "compare", 2),
            Self::CmpFloat => ("gleam@float", "compare", 2),
            Self::CmpString => ("gleam@string", "compare", 2),
            Self::CmpBool => ("gleam@bool", "compare", 2),
            Self::StrAppend => ("gleam@string", "append", 2),
            Self::ResultTry => ("gleam@result", "try", 2),
        }
    }

    /// The durable family this call belongs to (S16), when it is a park point.
    pub(crate) fn durable_family(self) -> Option<DurableFamily> {
        match self {
            Self::WfSleep | Self::WfWithTimeout => Some(DurableFamily::Timers),
            Self::WfRun
            | Self::WfAll
            | Self::WfMap
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
