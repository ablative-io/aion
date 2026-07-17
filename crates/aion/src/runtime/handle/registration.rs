//! Built-in and engine NIF registration helpers.

use beamr::atom::AtomTable;
use beamr::native::{BifRegistryImpl, NativeRegistrationError};

use crate::error::EngineError;

use super::super::nif::Mfa;
use super::runtime_error_from_display;

pub(super) fn nif_registration_error(mfa: &Mfa, error: NativeRegistrationError) -> EngineError {
    match error {
        NativeRegistrationError::DuplicateMfa { .. } => EngineError::NifRegistration {
            reason: format!("native function already registered for {}", mfa.display()),
        },
    }
}

pub(super) fn register_all_bifs(
    registry: &BifRegistryImpl,
    atom_table: &AtomTable,
) -> Result<(), EngineError> {
    use beamr::native::{
        bifs::register_gate1_bifs, gate3_bifs::register_gate3_bifs,
        gleam_ffi::register_gleam_ffi_bifs, otp_stubs::init_otp_atoms,
        otp_stubs::register_otp_stubs, process_bifs::register_gate2_bifs,
        selector_ffi::register_selector_bifs, stdlib_stubs::register_stdlib_stubs,
    };

    register_gate1_bifs(registry, atom_table).map_err(runtime_error_from_display)?;
    register_gate2_bifs(registry, atom_table).map_err(runtime_error_from_display)?;
    register_gate3_bifs(registry, atom_table).map_err(runtime_error_from_display)?;
    register_stdlib_stubs(registry, atom_table).map_err(runtime_error_from_display)?;
    register_selector_bifs(registry, atom_table).map_err(runtime_error_from_display)?;
    register_gleam_ffi_bifs(registry, atom_table).map_err(runtime_error_from_display)?;
    init_otp_atoms(atom_table);
    register_otp_stubs(registry, atom_table).map_err(runtime_error_from_display)?;
    Ok(())
}
