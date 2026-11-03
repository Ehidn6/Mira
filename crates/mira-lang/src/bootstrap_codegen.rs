//! One compiler implementation, authored in Mira and bootstrapped by the VM.
//! Native emission consumes its output; there is no fallback Rust opcode table.
use mira_core::Result;
use std::sync::OnceLock;

pub(crate) struct WordInstructions {
    words: Vec<Vec<u8>>,
    immediates: Vec<Vec<u8>>,
    frames: Vec<Vec<u8>>,
    aggregates: Vec<AggregateEncoding>,
}
pub(crate) struct AggregateEncoding {
    pub bytes: Vec<u8>,
    /// Relative byte offsets: literal parameter=-1, invalid=3, bounds=6, memory=7, state references=8..10.
    pub fixups: Vec<(usize, i64)>,
}
impl WordInstructions {
    pub(crate) fn aggregate(&self, operation: usize) -> &AggregateEncoding {
        &self.aggregates[operation]
    }

    pub(crate) fn get(&self, operation: usize) -> &[u8] {
        &self.words[operation]
    }
    pub(crate) fn immediate(&self, register: usize) -> &[u8] {
        &self.immediates[register]
    }
    pub(crate) fn frame(&self, write: bool, register: usize) -> &[u8] {
        &self.frames[usize::from(write) * 16 + register]
    }
}
fn decode(encoded: i64) -> Result<Vec<u8>> {
    if encoded < 0 {
        return Err("Mira compiler rejected an encoding request".into());
    }
    let length = (encoded as u64 >> 32) as usize;
    if !(1..=4).contains(&length) || (encoded as u32 as u64) >> (length * 8) != 0 {
        return Err("invalid packed instruction from Mira compiler".into());
    }
    Ok((encoded as u32).to_le_bytes()[..length].to_vec())
}
pub(crate) fn instructions() -> Result<&'static WordInstructions> {
    static WORDS: OnceLock<Result<WordInstructions>> = OnceLock::new();
    WORDS
        .get_or_init(|| {
            let library = crate::bootstrap_compiler::library()?;
            let program = &library.program;
            let prepared = &library.code;
            let mut arena = crate::data_runtime::Arena::default();
            let selector = program
                .code_modules
                .iter()
                .find(|d| d.name == "word_encoding")
                .ok_or("Mira instruction selector missing")?
                .id;
            let mut instructions = Vec::new();
            for operation in 0..11 {
                instructions.push(decode(prepared.call(
                    selector,
                    &[operation],
                    &mut arena,
                )?)?);
            }
            let selector = program
                .code_modules
                .iter()
                .find(|d| d.name == "immediate_prefix")
                .ok_or("Mira immediate selector missing")?
                .id;
            let mut immediates = Vec::new();
            for register in 0..16 {
                immediates.push(decode(prepared.call(selector, &[register], &mut arena)?)?);
            }
            let selector = program
                .code_modules
                .iter()
                .find(|d| d.name == "frame_access_prefix")
                .ok_or("Mira frame selector missing")?
                .id;
            let mut frames = Vec::new();
            for write in [0, 1] {
                for register in 0..16 {
                    frames.push(decode(prepared.call(
                        selector,
                        &[write, register],
                        &mut arena,
                    )?)?);
                }
            }
            let types = &program.global_types;
            let definition = |name: &str| {
                types
                    .iter()
                    .find(|d| d.name == name)
                    .ok_or_else(|| format!("compiler aggregate type missing: {name}"))
            };
            let state_type = definition("ScalarCode")?;
            let mira_core::data::Kind::Record(fields) = &state_type.kind else {
                return Err("compiler state must be a record".into());
            };
            let guards_field = fields
                .iter()
                .position(|f| f.name == "guards")
                .ok_or("compiler fixup counter missing")?;
            // Only the bounded compiler work arena is enlarged. No user or
            // kernel execution region acquires a larger memory allowance.
            arena.limit = 1024 * 1024;
            let state = arena.allocate(state_type.id, vec![0; state_type.len()])?;
            let bytes_type = definition("ScalarBytes")?;
            let output = arena.allocate(bytes_type.id, vec![0; 256])?;
            let fixup_type = definition("ScalarFixups")?;
            let fixups = arena.allocate(fixup_type.id, vec![0; fixup_type.len()])?;
            let selector = program
                .code_modules
                .iter()
                .find(|d| d.name == "aggregate_encoding")
                .ok_or("Mira aggregate emitter missing")?
                .id;
            let mut aggregates = Vec::new();
            for operation in 0..10 {
                let size =
                    prepared.call(selector, &[operation, state, output, fixups], &mut arena)?;
                if !(1..=256).contains(&size) {
                    return Err("Mira aggregate emitter rejected a native operation".into());
                }
                let bytes = (0..size as usize)
                    .map(|i| {
                        u8::try_from(arena.get(output, i)?)
                            .map_err(|_| "aggregate emitter produced a non-byte".into())
                    })
                    .collect::<Result<Vec<_>>>()?;
                let count = arena.get(state, guards_field)?;
                if !(0..=16).contains(&count) {
                    return Err("invalid aggregate fixup count".into());
                }
                let mut patches = Vec::new();
                for index in 0..count as usize {
                    let offset = usize::try_from(arena.get(fixups, index * 2)?)
                        .map_err(|_| "negative aggregate fixup offset")?;
                    let kind = arena.get(fixups, index * 2 + 1)?;
                    if offset.checked_add(4).is_none_or(|end| end > bytes.len())
                        || !matches!(kind, -1 | 3 | 6 | 7 | 8 | 9 | 10)
                        || patches
                            .iter()
                            .any(|(previous, _)| usize::abs_diff(*previous, offset) < 4)
                    {
                        return Err("invalid aggregate fixup".into());
                    }
                    patches.push((offset, kind));
                }
                aggregates.push(AggregateEncoding {
                    bytes,
                    fixups: patches,
                });
            }
            Ok(WordInstructions {
                words: instructions,
                immediates,
                frames,
                aggregates,
            })
        })
        .as_ref()
        .map_err(Clone::clone)
}
