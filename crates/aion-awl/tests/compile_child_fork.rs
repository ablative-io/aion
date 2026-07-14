//! Direct-compile flagship golden for child collection fan-out.

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use aion_awl::compile;
use aion_awl::mir::{lower, select};

type TestResult = Result<(), Box<dyn std::error::Error>>;
type BeamChunk = (String, Vec<u8>);

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

fn unhex(text: &str) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let text = text.trim();
    Ok((0..text.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&text[index..index + 2], 16))
        .collect::<Result<Vec<u8>, _>>()?)
}

fn beam_chunks(bytes: &[u8]) -> Result<Vec<BeamChunk>, Box<dyn std::error::Error>> {
    assert!(bytes.starts_with(b"FOR1"), "not a BEAM container");
    let mut chunks = Vec::new();
    let mut offset = 12;
    while offset + 8 <= bytes.len() {
        let name = String::from_utf8(bytes[offset..offset + 4].to_vec())?;
        let size_bytes: [u8; 4] = bytes[offset + 4..offset + 8].try_into()?;
        let size = u32::from_be_bytes(size_bytes) as usize;
        let payload_end = offset + 8 + size;
        let payload = bytes
            .get(offset + 8..payload_end)
            .ok_or("BEAM chunk payload extends beyond the container")?
            .to_vec();
        chunks.push((name, payload));
        offset += 8 + size.div_ceil(4) * 4;
    }
    Ok(chunks)
}

fn inflate_litt(payload: &[u8]) -> Result<(u32, Vec<u8>), Box<dyn std::error::Error>> {
    let declared = payload
        .get(..4)
        .ok_or("LitT payload has no declared length")?;
    let declared_bytes: [u8; 4] = declared.try_into()?;
    let compressed = payload
        .get(4..)
        .ok_or("LitT payload has no compressed content")?;
    let mut out = Vec::new();
    std::io::Read::read_to_end(&mut flate2::read::ZlibDecoder::new(compressed), &mut out)?;
    Ok((u32::from_be_bytes(declared_bytes), out))
}

/// Compare chunk bytes exactly except for `LitT`'s feature-graph-dependent
/// deflate stream, whose decompressed bytes are the stable contract.
fn beam_equivalent(actual: &[u8], expected: &[u8]) -> TestResult {
    let actual_chunks = beam_chunks(actual)?;
    let expected_chunks = beam_chunks(expected)?;
    let names = |chunks: &[BeamChunk]| {
        chunks
            .iter()
            .map(|(name, _)| name.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(names(&actual_chunks), names(&expected_chunks));
    for ((name, actual_payload), (_, expected_payload)) in
        actual_chunks.iter().zip(expected_chunks.iter())
    {
        if name == "LitT" {
            assert_eq!(
                inflate_litt(actual_payload)?,
                inflate_litt(expected_payload)?
            );
        } else {
            assert_eq!(actual_payload, expected_payload, "chunk {name} drifted");
        }
    }
    Ok(())
}

fn read(path: &Path) -> Result<(String, PathBuf), Box<dyn std::error::Error>> {
    let source = fs::read_to_string(path)?;
    let root = path
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", path.display()))?
        .to_path_buf();
    Ok((source, root))
}

/// The child collection fixture compiles deterministically, equals direct MIR
/// selection, and matches committed BEAM and sidecar goldens.
#[test]
fn child_collection_fork_compiles_deterministically_to_the_select_bytes() -> TestResult {
    let fixture =
        manifest_dir().join("tests/fixtures/rev2/dag-fork/valid/child_collection_fork.awl");
    let (source, root) = read(&fixture)?;
    let first = compile(&source, &root).map_err(|error| error.to_string())?;
    let second = compile(&source, &root).map_err(|error| error.to_string())?;
    assert_eq!(first.workflow_name, "child_collection_fork");
    assert_eq!(first, second, "compile is not deterministic");

    let document = aion_awl::parse(&source)?;
    let module = lower(&document, Some(&root))?;
    assert_eq!(first.beam_bytes, select(&module)?);

    let golden_root = manifest_dir().join("tests/mir-goldens/dag-fork/valid");
    let beam_golden = golden_root.join("child_collection_fork.beam.hex");
    let beam_hex = hex(&first.beam_bytes);
    if std::env::var("AWL_BC2_BLESS").is_ok() {
        fs::write(&beam_golden, &beam_hex)?;
    }
    let expected = fs::read_to_string(&beam_golden).map_err(|error| {
        format!(
            "missing beam golden {} (run with AWL_BC2_BLESS=1 to create): {error}",
            beam_golden.display()
        )
    })?;
    beam_equivalent(&first.beam_bytes, &unhex(&expected)?)?;
    assert_eq!(
        hex(&first.sidecar_bytes),
        fs::read_to_string(golden_root.join("child_collection_fork.gleam_types.hex"))?
    );
    Ok(())
}
