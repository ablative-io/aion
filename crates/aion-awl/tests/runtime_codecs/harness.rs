//! The embedded-VM + reference-build harness for the BC-2b-5 runtime codec
//! proof: emit the reference module (real emitter), append its Gleam
//! driver, `gleam build` it, then load every produced `.beam` plus the
//! direct-`select`ed modules into one beamr scheduler.

use std::error::Error;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use beamr::atom::AtomTable;
use beamr::loader::load_module_with_origin;
use beamr::module::{ModuleOrigin, ModuleRegistry};
use beamr::native::{
    BifRegistryImpl,
    bifs::register_gate1_bifs,
    gate3_bifs::register_gate3_bifs,
    gleam_ffi::register_gleam_ffi_bifs,
    otp_stubs::{init_otp_atoms, register_otp_stubs},
    process_bifs::register_gate2_bifs,
    selector_ffi::register_selector_bifs,
    stdlib_stubs::register_stdlib_stubs,
};
use beamr::process::ExitReason;
use beamr::scheduler::{Scheduler, SchedulerConfig, SchedulerServices};
use beamr::term::Term;
use beamr::term::binary_ref::BinaryRef;
use beamr::term::boxed::Tuple;
use beamr::term::format::format_term;

use aion_awl::{emit_in, parse};

/// One raw driver call's observable outcome: exit reason, formatted result
/// (or failure detail), and the first tuple element's binary payload.
pub(crate) type RawCall = (ExitReason, String, Option<Vec<u8>>);

pub(crate) fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

pub(crate) fn fixture(relative: &str) -> PathBuf {
    manifest_dir().join("tests/fixtures/rev2").join(relative)
}

/// Scratch root for throwaway reference builds: a stable, per-purpose
/// directory under the repo's shared cargo target pile — NEVER the system
/// temp directory (build artifacts in `/tmp` are banned; `cargo clean`
/// bounds this pile). Names are stable so repeat runs reuse build caches
/// instead of accumulating; every caller's label is unique within a run, so
/// parallel tests never race one directory.
pub(crate) fn scratch_build_dir(label: &str) -> PathBuf {
    manifest_dir()
        .parent()
        .and_then(Path::parent)
        .map_or_else(std::env::temp_dir, Path::to_path_buf)
        .join("target/awl-test-scratch")
        .join(label)
}

// ---- reference (gleam build) side ----------------------------------------

/// Emit a fixture through the REFERENCE emitter and append a Gleam driver.
pub(crate) fn reference_module(relative: &str, driver: &str) -> Result<String, Box<dyn Error>> {
    reference_module_at(&fixture(relative), driver)
}

/// [`reference_module`] over an arbitrary document path (for documents that
/// live outside the fixture tree, like the flagship examples).
pub(crate) fn reference_module_at(path: &Path, driver: &str) -> Result<String, Box<dyn Error>> {
    let source = fs::read_to_string(path)?;
    let document = parse(&source)?;
    let dir = path
        .parent()
        .ok_or("fixture path has no parent directory")?;
    let mut generated = emit_in(&document, dir)?;
    generated.push('\n');
    generated.push_str(driver);
    Ok(generated)
}

/// Build a throwaway Gleam project holding the reference modules + drivers,
/// returning every produced `ebin` directory.
pub(crate) fn gleam_build(modules: &[(&str, &str)]) -> Result<Vec<PathBuf>, Box<dyn Error>> {
    let repo_root = manifest_dir()
        .parent()
        .and_then(Path::parent)
        .ok_or("cannot resolve the repository root")?
        .to_path_buf();
    let names: Vec<&str> = modules.iter().map(|(name, _)| *name).collect();
    let project = scratch_build_dir(&format!("gleam_{}", names.join("_")));
    fs::create_dir_all(project.join("src"))?;
    fs::write(
        project.join("gleam.toml"),
        format!(
            "name = \"awl_rt_codec_proof\"\nversion = \"0.1.0\"\ntarget = \"erlang\"\n\n\
             [dependencies]\naion_flow = {{ path = \"{}\" }}\ngleam_stdlib = \
             \">= 0.34.0 and < 2.0.0\"\ngleam_json = \">= 2.0.0 and < 4.0.0\"\n",
            repo_root.join("gleam/aion_flow").display()
        ),
    )?;
    for (name, source) in modules {
        fs::write(project.join("src").join(format!("{name}.gleam")), source)?;
    }
    let output = Command::new("gleam")
        .arg("build")
        .current_dir(&project)
        .output()
        .map_err(|error| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("gleam binary is required for the runtime codec proof: {error}"),
            )
        })?;
    if !output.status.success() {
        return Err(format!(
            "gleam build failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    let mut ebins = Vec::new();
    let packages = project.join("build/dev/erlang");
    for entry in fs::read_dir(&packages)? {
        let ebin = entry?.path().join("ebin");
        if ebin.is_dir() {
            ebins.push(ebin);
        }
    }
    Ok(ebins)
}

/// Build the production FFI namespace used by the cross-module child proof.
/// `spawn_child/3` executes the selected generated child's exported `run/1`
/// host on the exact encoded input and records its encoded output verbatim.
/// The optional bare mode is the strict-envelope negative pin.
pub(crate) fn child_host_ebin(
    label: &str,
    child_module: &str,
    bare: bool,
) -> Result<PathBuf, Box<dyn Error>> {
    let suffix = if bare { "_bare" } else { "" };
    let dir = scratch_build_dir(&format!("child_host_{label}{suffix}"));
    fs::create_dir_all(&dir)?;
    let source = dir.join("aion_flow_ffi.erl");
    let terminal = if bare {
        "{ok, <<\"ok:{\\\"spec\\\":{\\\"harness\\\":\\\"legacy\\\",\\\"model\\\":\\\"bare\\\",\\\"effort\\\":\\\"none\\\"},\\\"first_try\\\":true}\">>}"
            .to_owned()
    } else {
        format!(
            "case {child_module}:run(Input) of {{ok, Output}} -> {{ok, <<\"ok:\", Output/binary>>}}; \
             {{error, _}} -> {{error, <<\"child run failed\">>}} end"
        )
    };
    fs::write(
        &source,
        format!(
            "-module(aion_flow_ffi).\n-export([spawn_child/3, await_child/1]).\n\
             spawn_child(<<\"sit_one\">>, Input, _Config) ->\n\
             ChildId = Input,\n\
             Result = {terminal},\n\
             erlang:put({{awl_child_result, ChildId}}, Result),\n\
             {{ok, ChildId}}.\n\
             await_child(ChildId) ->\n\
             case erlang:get({{awl_child_result, ChildId}}) of\n\
             undefined -> {{error, <<\"unknown child\">>}}; Result -> Result end.\n"
        ),
    )?;
    let (parent_module, run_fn, bare_fn, roundtrip_fn, child_error_fn) =
        if child_module.starts_with("ref_") {
            (
                "ref_child_collection_fork",
                "awl_rt_run",
                "awl_rt_bare_decode_fails",
                "awl_rt_neutral_roundtrip",
                "awl_rt_bare_child_error",
            )
        } else {
            (
                "child_collection_fork",
                "'awl$rt_run'",
                "'awl$rt_bare_decode_fails'",
                "'awl$rt_neutral_roundtrip'",
                "'awl$rt_bare_child_error'",
            )
        };
    let runner = dir.join("aion_awl_test_heap.erl");
    fs::write(
        &runner,
        format!(
            "-module(aion_awl_test_heap).\n-export([run/2, target/2]).\n\
         run(_Module, {run_fn}) -> run_fun(run_target);\n\
         run(_Module, {bare_fn}) -> run_fun(bare_target);\n\
         run(_Module, {roundtrip_fn}) -> run_fun(roundtrip_target);\n\
         run(_Module, {child_error_fn}) -> run_fun(child_error_target).\n\
         run_fun(Target) ->\n\
         Parent = self(),\n\
         _ = erlang:spawn_opt(?MODULE, target, [Parent, Target], [{{min_heap_size, 2048}}]),\n\
         receive {{aion_awl_test_result, Result}} -> Result end.\n\
         target(Parent, run_target) ->\n\
         Parent ! {{aion_awl_test_result, {parent_module}:{run_fn}()}};\n\
         target(Parent, bare_target) ->\n\
         Parent ! {{aion_awl_test_result, {parent_module}:{bare_fn}()}};\n\
         target(Parent, roundtrip_target) ->\n\
         Parent ! {{aion_awl_test_result, {parent_module}:{roundtrip_fn}()}};\n\
         target(Parent, child_error_target) ->\n\
         Parent ! {{aion_awl_test_result, {parent_module}:{child_error_fn}()}}.\n"
        ),
    )?;
    let output = Command::new("erlc")
        .arg("-o")
        .arg(&dir)
        .arg(&source)
        .arg(&runner)
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "child host erlc failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(dir)
}

/// Build a test-only `aion_flow_ffi` whose workflow context lookup fails. Load
/// this ebin before the SDK test double to exercise the real `EngineError` path.
pub(crate) fn workflow_id_error_ebin() -> Result<PathBuf, Box<dyn Error>> {
    workflow_id_ebin(
        "error",
        "workflow_id() -> {error, <<\"missing workflow context\">>}.\n",
    )
}

/// First lookup succeeds; a second lookup in the same process fails. A
/// correctly short-circuited predicate never reaches that poisoned lookup.
pub(crate) fn workflow_id_poison_ebin() -> Result<PathBuf, Box<dyn Error>> {
    workflow_id_ebin(
        "poison",
        "workflow_id() -> case get(awl_workflow_id_seen) of undefined -> put(awl_workflow_id_seen, true), {ok, <<\"test-workflow-id\">>}; _ -> {error, <<\"poisoned workflow context\">>} end.\n",
    )
}

/// Build a test-only `aion_flow_ffi` whose activity dispatch always refuses.
/// Input ENCODING happens before dispatch in `workflow.run`, so reaching this
/// stub (a clean `{error, _}` return instead of a crash) proves the dispatch
/// input survived its codec.
pub(crate) fn dispatch_refused_ebin() -> Result<PathBuf, Box<dyn Error>> {
    let dir = scratch_build_dir("dispatch_refused");
    fs::create_dir_all(&dir)?;
    let source = dir.join("aion_flow_ffi.erl");
    fs::write(
        &source,
        "-module(aion_flow_ffi).\n-export([dispatch_activity/3]).\n\
         dispatch_activity(_Name, _Input, _Config) -> {error, <<\"stub dispatch\">>}.\n",
    )?;
    let output = Command::new("erlc")
        .arg("-o")
        .arg(&dir)
        .arg(&source)
        .output()
        .map_err(|error| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("erlc is required for the dispatch-refusal proof: {error}"),
            )
        })?;
    if !output.status.success() {
        return Err(format!(
            "dispatch stub erlc failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(dir)
}

/// Build a test-only `aion_flow_ffi` collection collector that records the
/// joined specs in the WORKFLOW PROCESS dictionary (key `awl_ffi_echo`) and
/// refuses dispatch. Every `workflow.all`/`workflow.map` fan-out funnels
/// through `ffi.collect_all(id, specs)` where each spec is a JSON object
/// embedding `correlation` (input order), `name`, the codec-encoded `input`,
/// and the full merged `config` JSON (`concurrency.gleam::activity_spec`).
/// The AWL error mapper deliberately collapses activity errors
/// (`awl/error.gleam::map_activity_error` → `AwlActivityFailed("activity
/// failed")`), so the wire bytes are observed through the recorded echo (an
/// in-process runner reads it back after the run — the echo-runner pattern
/// in `fork_generality.rs`), not through the workflow result.
/// `label` keys the scratch dir per TEST: concurrent tests must never share
/// one stub dir, or one test's `erlc` re-compile races another's VM load
/// (best-effort loading silently skips a mid-write `.beam`).
pub(crate) fn collect_echo_ebin(label: &str) -> Result<PathBuf, Box<dyn Error>> {
    ffi_stub_ebin(
        &format!("collect_echo_{label}"),
        "-module(aion_flow_ffi).\n-export([collect_all/2]).\n\
         collect_all(_Id, Specs) ->\n\
         erlang:put(awl_ffi_echo, join(Specs, <<>>)),\n\
         {error, <<\"terminal:collect\">>}.\n\
         join([], Acc) -> Acc;\n\
         join([Spec | Rest], Acc) -> join(Rest, <<Acc/binary, Spec/binary>>).\n",
    )
}

/// The per-item wire twin of [`collect_echo_ebin`] for sequential folds and
/// plain calls: `dispatch_activity` records `name|input|config`
/// (`run.gleam::dispatch`) under the same process-dictionary key, then
/// refuses.
/// `label` keys the scratch dir per test (see [`collect_echo_ebin`]).
pub(crate) fn dispatch_echo_ebin(label: &str) -> Result<PathBuf, Box<dyn Error>> {
    ffi_stub_ebin(
        &format!("dispatch_echo_{label}"),
        "-module(aion_flow_ffi).\n-export([dispatch_activity/3]).\n\
         dispatch_activity(Name, Input, Config) ->\n\
         erlang:put(awl_ffi_echo, <<Name/binary, \"|\", Input/binary, \"|\", \
         Config/binary>>),\n\
         {error, <<\"terminal:dispatch\">>}.\n",
    )
}

/// A wait-driving `aion_flow_ffi`: `receive_signal` and `with_timeout` stubs
/// whose bodies the caller supplies, exercising the 4-arm timeout case
/// (`timer.gleam::with_timeout` — `Ok(Op())` completes, `timeout:`-prefixed
/// errors are the deadline arm, anything else is the engine-failure arm).
pub(crate) fn wait_ffi_ebin(label: &str, body: &str) -> Result<PathBuf, Box<dyn Error>> {
    ffi_stub_ebin(&format!("wait_{label}"), body)
}

/// Compile one hand-written stub module into its own ebin directory.
fn ffi_stub_ebin(label: &str, source_text: &str) -> Result<PathBuf, Box<dyn Error>> {
    let dir = scratch_build_dir(&format!("ffi_stub_{label}"));
    fs::create_dir_all(&dir)?;
    let source = dir.join("aion_flow_ffi.erl");
    fs::write(&source, source_text)?;
    let output = Command::new("erlc")
        .arg("-o")
        .arg(&dir)
        .arg(&source)
        .output()
        .map_err(|error| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("erlc is required for the ffi stub proof: {error}"),
            )
        })?;
    if !output.status.success() {
        return Err(format!(
            "ffi stub erlc failed for {label}\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(dir)
}

fn workflow_id_ebin(label: &str, implementation: &str) -> Result<PathBuf, Box<dyn Error>> {
    let dir = scratch_build_dir(&format!("workflow_id_{label}"));
    fs::create_dir_all(&dir)?;
    let source = dir.join("aion_flow_ffi.erl");
    fs::write(
        &source,
        format!("-module(aion_flow_ffi).\n-export([workflow_id/0]).\n{implementation}"),
    )?;
    let output = Command::new("erlc")
        .arg("-o")
        .arg(&dir)
        .arg(&source)
        .output()
        .map_err(|error| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("erlc is required for the workflow-id error proof: {error}"),
            )
        })?;
    if !output.status.success() {
        return Err(format!(
            "erlc failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(dir)
}

// ---- embedded VM ----------------------------------------------------------

pub(crate) struct Vm {
    atoms: Arc<AtomTable>,
    scheduler: Scheduler,
}

pub(crate) fn build_vm(ebins: &[PathBuf], direct: &[Vec<u8>]) -> Result<Vm, Box<dyn Error>> {
    let atoms = Arc::new(AtomTable::with_common_atoms());
    let bifs = Arc::new(BifRegistryImpl::new());
    register_gate1_bifs(&bifs, &atoms).map_err(|error| error.to_string())?;
    register_gate2_bifs(&bifs, &atoms).map_err(|error| error.to_string())?;
    register_gate3_bifs(&bifs, &atoms).map_err(|error| error.to_string())?;
    register_stdlib_stubs(&bifs, &atoms).map_err(|error| error.to_string())?;
    register_selector_bifs(&bifs, &atoms).map_err(|error| error.to_string())?;
    register_gleam_ffi_bifs(&bifs, &atoms).map_err(|error| error.to_string())?;
    init_otp_atoms(&atoms);
    register_otp_stubs(&bifs, &atoms).map_err(|error| error.to_string())?;
    let registry = ModuleRegistry::new();
    for ebin in ebins {
        for entry in fs::read_dir(ebin)? {
            let path = entry?.path();
            if path.extension().is_some_and(|ext| ext == "beam") {
                let Ok(bytes) = fs::read(&path) else { continue };
                // Best effort, exactly like the beamr CLI: modules with
                // unsupported shapes that the proof never calls may skip.
                let _ = load_module_with_origin(
                    &bytes,
                    &atoms,
                    &registry,
                    &*bifs,
                    ModuleOrigin::Filesystem(path),
                );
            }
        }
    }
    for bytes in direct {
        load_module_with_origin(bytes, &atoms, &registry, &*bifs, ModuleOrigin::Embedded)
            .map_err(|error| format!("direct module failed to load: {error}"))?;
    }
    let scheduler = Scheduler::with_services_and_code_server(
        SchedulerConfig {
            thread_count: Some(1),
            ..SchedulerConfig::default()
        },
        SchedulerServices::full_runtime(),
        Arc::new(registry),
        Arc::clone(&atoms),
        bifs,
    )
    .map_err(|error| format!("scheduler start failed: {error}"))?;
    Ok(Vm { atoms, scheduler })
}

impl Vm {
    /// Spawn `module:function/0`, run to exit, and return the formatted
    /// result (exit must be Normal).
    pub(crate) fn call0(&self, module: &str, function: &str) -> Result<String, Box<dyn Error>> {
        let (reason, formatted, _bytes) = self.call0_raw(module, function)?;
        if reason == ExitReason::Normal {
            Ok(formatted)
        } else {
            Err(format!("{module}:{function}/0 exited {reason:?}: {formatted}").into())
        }
    }

    /// Run a zero-arity target in the test heap runner's independent 2048-word
    /// workflow process. This models the production child execution boundary;
    /// beamr's ordinary top-level test process is intentionally only 233 words.
    pub(crate) fn call0_large(
        &self,
        module: &str,
        function: &str,
    ) -> Result<String, Box<dyn Error>> {
        let (reason, formatted, _bytes) = self.call0_large_raw(module, function)?;
        if reason == ExitReason::Normal {
            Ok(formatted)
        } else {
            Err(format!("{module}:{function}/0 exited {reason:?}: {formatted}").into())
        }
    }

    /// Spawn and return `(exit reason, formatted result, first tuple-element
    /// binary when the result is a tuple whose element 0 is a binary)`.
    pub(crate) fn call0_raw(
        &self,
        module: &str,
        function: &str,
    ) -> Result<RawCall, Box<dyn Error>> {
        let module_atom = self.atoms.intern(module);
        let function_atom = self.atoms.intern(function);
        let pid = self
            .scheduler
            .spawn(module_atom, function_atom, Vec::new())
            .map_err(|error| error.format_with_atoms(&self.atoms))?;
        Ok(self.finish_call(pid))
    }

    fn call0_large_raw(&self, module: &str, function: &str) -> Result<RawCall, Box<dyn Error>> {
        let runner = self.atoms.intern("aion_awl_test_heap");
        let run = self.atoms.intern("run");
        let module_atom = self.atoms.intern(module);
        let function_atom = self.atoms.intern(function);
        let pid = self
            .scheduler
            .spawn(
                runner,
                run,
                vec![Term::atom(module_atom), Term::atom(function_atom)],
            )
            .map_err(|error| error.format_with_atoms(&self.atoms))?;
        Ok(self.finish_call(pid))
    }

    fn finish_call(&self, pid: u64) -> RawCall {
        let (reason, owned) = self.scheduler.run_until_exit(pid);
        let root = owned.root();
        let formatted = if reason == ExitReason::Normal {
            format_term(root, &self.atoms)
        } else {
            let exception = self.scheduler.take_exit_exception(pid);
            let error = self.scheduler.take_exit_error(pid);
            exception
                .map(|exception| exception.format_with_atoms(&self.atoms))
                .or_else(|| error.map(|error| error.format_with_atoms(&self.atoms)))
                .unwrap_or_else(|| format_term(root, &self.atoms))
        };
        let payload = Tuple::new(root)
            .and_then(|tuple| tuple.get(0))
            .and_then(BinaryRef::new)
            .map(|binary| binary.as_bytes().to_vec());
        (reason, formatted, payload)
    }

    /// Run one side (`direct`/`ref`) of an echo-runner proof: spawn
    /// `aion_awl_echo_runner:run(side)`, which executes the target driver in
    /// its own workflow-sized process and returns `{Result, Echo}` — the
    /// collapsed workflow result AND the raw wire bytes the ffi echo stub
    /// recorded in that process's dictionary.
    pub(crate) fn call_echo(&self, side: &str) -> Result<String, Box<dyn Error>> {
        let runner = self.atoms.intern("aion_awl_echo_runner");
        let run = self.atoms.intern("run");
        let side_atom = self.atoms.intern(side);
        let pid = self
            .scheduler
            .spawn(runner, run, vec![Term::atom(side_atom)])
            .map_err(|error| error.format_with_atoms(&self.atoms))?;
        let (reason, formatted, _payload) = self.finish_call(pid);
        if reason == ExitReason::Normal {
            Ok(formatted)
        } else {
            Err(format!("echo runner ({side}) exited {reason:?}: {formatted}").into())
        }
    }

    /// A round-trip driver's `#(encoded, equal)` pair: the encoded JSON
    /// bytes and the round-trip equality flag.
    pub(crate) fn roundtrip(
        &self,
        module: &str,
        function: &str,
    ) -> Result<(String, bool), Box<dyn Error>> {
        let (reason, formatted, payload) = self.call0_raw(module, function)?;
        if reason != ExitReason::Normal {
            return Err(format!("{module}:{function}/0 exited {reason:?}: {formatted}").into());
        }
        let bytes = payload.ok_or_else(|| format!("{function} returned no binary: {formatted}"))?;
        let equal = formatted.ends_with(", true}");
        Ok((String::from_utf8(bytes)?, equal))
    }

    /// [`roundtrip`] through the workflow-sized heap runner.
    pub(crate) fn roundtrip_large(
        &self,
        module: &str,
        function: &str,
    ) -> Result<(String, bool), Box<dyn Error>> {
        let (reason, formatted, payload) = self.call0_large_raw(module, function)?;
        if reason != ExitReason::Normal {
            return Err(format!("{module}:{function}/0 exited {reason:?}: {formatted}").into());
        }
        let bytes = payload.ok_or_else(|| format!("{function} returned no binary: {formatted}"))?;
        let equal = formatted.ends_with(", true}");
        Ok((String::from_utf8(bytes)?, equal))
    }
}
