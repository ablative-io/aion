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

// ---- reference (gleam build) side ----------------------------------------

/// Emit a fixture through the REFERENCE emitter and append a Gleam driver.
pub(crate) fn reference_module(relative: &str, driver: &str) -> Result<String, Box<dyn Error>> {
    let path = fixture(relative);
    let source = fs::read_to_string(&path)?;
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
    let mut project = std::env::temp_dir();
    project.push(format!(
        "aion_awl_rt_codecs_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos())
    ));
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
        Ok((reason, formatted, payload))
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
}
