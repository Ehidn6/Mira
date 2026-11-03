//! Stage-1 literal decoding runs real Mira code. Only the immutable decoder's
//! own source uses the explicit stage-0 seed; failures never fall back to Rust.
use mira_core::{Id, Result};
use std::sync::OnceLock;

struct Decoder {
    code: &'static crate::behavior::Prepared,
    entry: Id,
    natural: Id,
    float: Id,
    input: Id,
    output: Id,
}
fn decoder() -> Result<&'static Decoder> {
    static DECODER: OnceLock<Result<Decoder>> = OnceLock::new();
    DECODER
        .get_or_init(|| {
            let library = crate::bootstrap_compiler::library()?;
            let program = &library.program;
            let entry = program
                .code_modules
                .iter()
                .find(|d| d.name == "parse_integer")
                .ok_or("Mira integer decoder missing")?
                .id;
            let natural = program
                .code_modules
                .iter()
                .find(|d| d.name == "parse_natural")
                .ok_or("Mira natural decoder missing")?
                .id;
            let input = program
                .global_types
                .iter()
                .find(|d| d.name == "LiteralBytes")
                .ok_or("literal input type missing")?
                .id;
            let float = program
                .code_modules
                .iter()
                .find(|d| d.name == "parse_float")
                .ok_or("Mira f32 decoder missing")?
                .id;
            let output = program
                .global_types
                .iter()
                .find(|d| d.name == "LiteralValue")
                .ok_or("literal result type missing")?
                .id;
            Ok(Decoder {
                code: &library.code,
                entry,
                natural,
                float,
                input,
                output,
            })
        })
        .as_ref()
        .map_err(Clone::clone)
}
pub(crate) fn parse(text: &str, negative: bool) -> Result<i64> {
    decode(text, Some(negative))
}
pub(crate) fn natural(text: &str) -> Result<u64> {
    decode(text, None).map(|n| n as u64)
}
pub(crate) fn float(text: &str) -> Result<f32> {
    if text.len() > 128 {
        return Err("f32 literal exceeds 128-byte input limit".into());
    }
    let decoder = decoder()?;
    let mut arena = crate::data_runtime::Arena::default();
    let mut bytes = vec![0; 128];
    for (index, byte) in text.bytes().enumerate() {
        bytes[index] = byte as i64;
    }
    let input = arena.allocate(decoder.input, bytes)?;
    let output = arena.allocate(decoder.output, vec![0])?;
    match decoder.code.call_compiler(
        decoder.float,
        &[input, text.len() as i64, output],
        &mut arena,
    )? {
        0 => Ok(f32::from_bits(arena.get(output, 0)? as u32)),
        1 => Err("invalid f32 literal".into()),
        2 => Err("nonfinite f32 literal".into()),
        _ => Err("invalid status from Mira f32 decoder".into()),
    }
}
fn decode(text: &str, negative: Option<bool>) -> Result<i64> {
    if text.len() > 128 {
        return Err("integer literal exceeds 128-byte input limit".into());
    }
    let decoder = decoder()?;
    let mut arena = crate::data_runtime::Arena::default();
    let mut input = vec![0; 128];
    for (index, byte) in text.bytes().enumerate() {
        input[index] = byte as i64;
    }
    let input = arena.allocate(decoder.input, input)?;
    let output = arena.allocate(decoder.output, vec![0])?;
    let status = if let Some(negative) = negative {
        decoder.code.call(
            decoder.entry,
            &[input, text.len() as i64, i64::from(negative), output],
            &mut arena,
        )?
    } else {
        decoder.code.call(
            decoder.natural,
            &[input, text.len() as i64, output],
            &mut arena,
        )?
    };
    match status {
        0 => arena.get(output, 0),
        1 => Err(if negative.is_some() {
            "expected i64 literal"
        } else {
            "expected decimal u64 literal"
        }
        .into()),
        2 => Err(if negative.is_some() {
            "literal outside i64 range"
        } else {
            "literal outside u64 range"
        }
        .into()),
        _ => Err("invalid status from Mira integer decoder".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mira_float_decoder_rounds_exactly_and_preserves_output_on_error() -> Result<()> {
        let decoder = decoder()?;
        let mut arena = crate::data_runtime::Arena::default();
        let input = arena.allocate(decoder.input, vec![0; 128])?;
        let output = arena.allocate(decoder.output, vec![0])?;
        let mut cases: Vec<String> = [
            "0", "0.0", "000.000", "1.", ".5", "0.1", "123.456", "1E+3",
            "1e-45", "1e-46", "1.17549435e-38", "3.4028234663852886e38",
            "1.000000059604644775390625", "1.000000178813934326171875",
            "7.00649232162408535461864791644958065640130970938257885878534141944895541342930300743319094181060791015625e-46",
            "7.00649232162408535461864791644958065640130970938257885878534141944895541342930300743319094181060791015626e-46",
            "3.40282356779733661637539395458142568448e38",
            "0e9999999999999999999999", "1e-9999999999999999999999", "1e99999",
            "16777217.0", "16777219.0", "1.175494280757364291727882991035766513322858992758990427682963118425003064965173038558532425798475984e-38",
        ].into_iter().map(str::to_owned).collect();
        cases.push(format!("{}e-100", "9".repeat(100)));
        let mut seed = 0x12345678u32;
        for exponent in [-46, -38, -10, 0, 10, 38] {
            for _ in 0..8 {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                cases.push(format!("{}.{:09}e{exponent}", seed % 10, seed));
            }
        }
        for text in cases.iter().map(String::as_str).chain([
            "", ".", "1e", "1e+", "1e-", "1.2.3", "1e2e3", "1_0", "NaN", "inf", "-1.0",
        ]) {
            assert!(text.len() <= 128, "{text}");
            for (index, byte) in text.bytes().enumerate() {
                arena.set(input, index, byte as i64)?;
            }
            arena.set(output, 0, 0x12345678)?;
            let status = decoder.code.call_compiler(
                decoder.float,
                &[input, text.len() as i64, output],
                &mut arena,
            )?;
            let expected = if text.starts_with('-') {
                None
            } else {
                text.parse::<f32>().ok().filter(|v| v.is_finite())
            };
            if let Some(expected) = expected {
                assert_eq!(status, 0, "{text}");
                assert_eq!(arena.get(output, 0)? as u32, expected.to_bits(), "{text}");
            } else {
                assert_ne!(status, 0, "{text}");
                assert_eq!(arena.get(output, 0)?, 0x12345678, "{text}");
            }
        }
        Ok(())
    }
}
