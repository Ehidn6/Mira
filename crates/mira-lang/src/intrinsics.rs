//! Explicit, typed system operations. Host interpretation never dereferences guest addresses.
use mira_core::*;
pub(crate) const MAX_BULK_BYTES: u32 = 64 * 1024 * 1024;
pub(crate) fn aligned_type(name: &str) -> Option<Id> {
    name.strip_prefix("#data.aligned.")?.parse().ok()
}
pub(crate) const INTEGER_OPERATIONS: &[(&str, usize)] = &[
    ("math.wrapping_add", 2),
    ("math.wrapping_sub", 2),
    ("math.wrapping_mul", 2),
    ("math.rotate_left", 2),
    ("math.rotate_right", 2),
    ("math.count_ones", 1),
    ("math.leading_zeros", 1),
    ("math.trailing_zeros", 1),
    ("math.byte_swap", 1),
];
pub(crate) fn pure_integer(name: &str) -> bool {
    INTEGER_OPERATIONS.iter().any(|(n, _)| *n == name)
}
pub(crate) fn pure_address(name: &str) -> bool {
    matches!(
        name,
        "sys.addr"
            | "sys.address_value"
            | "sys.offset"
            | "sys.align_up"
            | "sys.address_distance"
            | "#addr.lt"
            | "#addr.le"
            | "#addr.gt"
            | "#addr.ge"
    )
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ForeignAbi {
    SysV64,
    Win64,
}
pub(crate) fn foreign_abi(name: &str) -> Option<ForeignAbi> {
    for (base, abi) in [
        ("sys.call_sysv64", ForeignAbi::SysV64),
        ("sys.call_win64", ForeignAbi::Win64),
    ] {
        if let Some(suffix) = name.strip_prefix(base) {
            if matches!(suffix, "" | "_f32" | "_bool" | "_void" | "_i32" | "_u32") {
                return Some(abi);
            }
        }
    }
    None
}
pub(crate) fn dynamic_native(name: &str) -> bool {
    foreign_abi(name).is_some() || matches!(name, "sys.data_address" | "sys.address_of" | "sys.procedure_by_id" | "sys.call_mira64")
}
/// These arguments identify definitions, rather than evaluating local values.
pub(crate) fn symbol_reference(name: &str) -> bool {
    handler_kind(name).is_some() || name == "sys.procedure_address"
}
/// Native boundaries use the actual argument types, not a fictitious nominal buffer.
pub(crate) fn native_signature(name: &str, args: &[Type]) -> Option<Result<Type>> {
    if foreign_abi(name).is_some() || name == "sys.call_mira64" {
        return Some((|| {
            if args.is_empty() || args.len() > 17 {
                return Err(format!(
                    "{name}: expected an address and 0..16 word arguments"
                ));
            }
            if args[0] != Type::Addr {
                return Err(format!("{name}: procedure address must have type addr"));
            }
            if args[1..].iter().any(|t| {
                !matches!(t, Type::I64 | Type::Addr | Type::Bool)
                    && !(*t == Type::F32 && foreign_abi(name).is_some())
            }) {
                return Err(format!(
                    "{name}: expected scalar ABI arguments; f32 requires a foreign ABI, aggregate storage must be passed explicitly with sys.data_address"
                ));
            }
            Ok(if name.ends_with("_f32") {
                Type::F32
            } else if name.ends_with("_bool") {
                Type::Bool
            } else if name.ends_with("_void") {
                Type::Unit
            } else {
                Type::I64
            })
        })());
    }
    if name == "sys.address_of" {
        return Some(if args.len()==1{Ok(Type::Addr)}else{Err("sys.address_of expects one addressable member".into())});
    }
    if name == "sys.procedure_by_id" {
        return Some(if args==[Type::I64]{Ok(Type::Addr)}else{Err("sys.procedure_by_id expects one image procedure identity".into())});
    }
    if name == "sys.data_address" {
        return Some(if matches!(args, [Type::Data(_)]) {
            Ok(Type::Addr)
        } else {
            Err("sys.data_address: expected one aggregate value".into())
        });
    }
    None
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum MemoryOrder {
    Relaxed,
    Acquire,
    Release,
    AcqRel,
    SeqCst,
}
pub(crate) fn atomic_operation(name: &str) -> Option<(&'static str, MemoryOrder, u8)> {
    for base in [
        "sys.atomic_load64",
        "sys.atomic_store64",
        "sys.atomic_exchange64",
        "sys.atomic_fetch_add64",
        "sys.atomic_compare_exchange64",
    ] {
        for width in [8, 16, 32, 64] {
            let prefix = format!("{}{width}", base.strip_suffix("64").unwrap());
            let Some(suffix) = name.strip_prefix(&prefix) else {
                continue;
            };
            let order = match suffix {
                "" | "_seq_cst" => MemoryOrder::SeqCst,
                "_relaxed" => MemoryOrder::Relaxed,
                "_acquire" => MemoryOrder::Acquire,
                "_release" => MemoryOrder::Release,
                "_acq_rel" => MemoryOrder::AcqRel,
                _ => continue,
            };
            if base == "sys.atomic_load64"
                && matches!(order, MemoryOrder::Release | MemoryOrder::AcqRel)
                || base == "sys.atomic_store64"
                    && matches!(order, MemoryOrder::Acquire | MemoryOrder::AcqRel)
            {
                return None;
            }
            return Some((base, order, width));
        }
    }
    None
}
pub(crate) fn signature(name: &str) -> Option<(Vec<Type>, Type)> {
    use Type::*;
    if let Some(id) = aligned_type(name) {
        return Some((vec![I64], Data(id)));
    }
    if let Some((_, arity)) = INTEGER_OPERATIONS.iter().find(|(n, _)| *n == name) {
        return Some((vec![I64; *arity], I64));
    }
    if let Some((_, arity)) = crate::reflection::QUERIES.iter().find(|(n, _)| *n == name) {
        return Some((vec![I64; *arity], I64));
    }
    if let Some(signature) = crate::metaprogram::signature(name) {
        return Some(signature);
    }
    if let Some((op, _, _)) = atomic_operation(name) {
        return Some(match op {
            "sys.atomic_load64" => (vec![Addr], I64),
            "sys.atomic_store64" => (vec![Addr, I64], Unit),
            "sys.atomic_compare_exchange64" => (vec![Addr, I64, I64], I64),
            _ => (vec![Addr, I64], I64),
        });
    }
    if name
        .strip_prefix("#kernel.slot.")
        .is_some_and(|n| n.parse::<usize>().is_ok())
    {
        return Some((vec![], Addr));
    }
    if let Some(id) = name
        .strip_prefix("#data.identity.")
        .and_then(|s| s.parse().ok())
    {
        return Some((vec![Data(id)], I64));
    }
    if let Some(rest) = name.strip_prefix("#buffer.") {
        if let Some((_, id)) = rest.split_once('.') {
            if let Ok(id) = id.parse() {
                return Some((vec![], Data(id)));
            }
        }
    }
    Some(match name {
        "#graph.require" | "#kernel.require" => (vec![Bool], Unit),
        "math.sqrt" => (vec![F32], F32),
        "math.to_f32" => (vec![I64], F32),
        "math.bits" | "#f32.bits" => (vec![F32], I64),
        "math.from_bits" | "#f32.from_bits" => (vec![I64], F32),
        "sys.addr" => (vec![I64], Addr),
        "sys.address_value" => (vec![Addr], I64),
        "sys.offset" | "sys.align_up" => (vec![Addr, I64], Addr),
        "sys.address_distance" => (vec![Addr, Addr], I64),
        "#addr.lt" | "#addr.le" | "#addr.gt" | "#addr.ge" => (vec![Addr, Addr], Bool),
        "sys.memory_copy" => (vec![Addr, Addr, I64], Unit),
        "sys.memory_fill" => (vec![Addr, I64, I64], Unit),
        "sys.read_cr0" | "sys.read_cr4" => (vec![], I64),
        "sys.write_cr0" | "sys.write_cr4" | "sys.ltr" => (vec![I64], Unit),
        "sys.read_msr" => (vec![I64], I64),
        "sys.write_msr" => (vec![I64, I64], Unit),
        "sys.swapgs" | "sys.acquire_fence" | "sys.release_fence" => (vec![], Unit),
        "sys.load8" | "sys.load16" | "sys.load32" | "sys.load64" => (vec![Addr], I64),
        "sys.store8" | "sys.store16" | "sys.store32" | "sys.store64" => (vec![Addr, I64], Unit),
        "sys.in8" | "sys.in16" | "sys.in32" => (vec![I64], I64),
        "sys.out8" | "sys.out16" | "sys.out32" => (vec![I64, I64], Unit),
        "sys.atomic_load64" => (vec![Addr], I64),
        "sys.atomic_store64" => (vec![Addr, I64], Unit),
        "sys.atomic_exchange64" | "sys.atomic_fetch_add64" => (vec![Addr, I64], I64),
        "sys.atomic_compare_exchange64" => (vec![Addr, I64, I64], I64),
        "sys.memory_fence" => (vec![], Unit),
        "sys.cpuid_eax" | "sys.cpuid_ebx" | "sys.cpuid_ecx" | "sys.cpuid_edx" => {
            (vec![I64, I64], I64)
        }
        "sys.cpu_local" => (vec![], Addr),
        "sys.set_cpu_local" => (vec![Addr], Unit),
        "sys.boot_info" => (vec![], Addr),
        "sys.halt" | "sys.cli" | "sys.sti" | "sys.hlt" | "sys.sti_hlt" | "sys.pause"
        | "sys.breakpoint" => (vec![], Unit),
        "sys.next_tick" | "sys.current_tick" => (vec![], I64),
        "sys.flags" | "sys.cs" | "sys.neural_status" | "#kernel.flags" => (vec![], I64),
        "#kernel.cli" | "#kernel.sti" => (vec![], Unit),
        "#kernel.region_required" => (vec![], Bool),
        "sys.read_cr2" | "sys.read_cr3" => (vec![], Addr),
        "sys.write_cr3" | "sys.invlpg" | "sys.lidt" | "sys.sidt" | "sys.lgdt" | "sys.sgdt" => {
            (vec![Addr], Unit)
        }
        _ => return None,
    })
}
pub(crate) fn interpret(name: &str, args: &[i64]) -> Result<i64> {
    if atomic_operation(name).is_some() {
        return Err("system operation requires native machine-code execution; portable Run does not access host memory".into());
    }
    if name.starts_with("#data.identity.") {
        return Ok(args[0]);
    }
    match name {
        "math.wrapping_add" => Ok(args[0].wrapping_add(args[1])),
        "math.wrapping_sub" => Ok(args[0].wrapping_sub(args[1])),
        "math.wrapping_mul" => Ok(args[0].wrapping_mul(args[1])),
        "math.rotate_left" => Ok(args[0].rotate_left(args[1] as u32)),
        "math.rotate_right" => Ok(args[0].rotate_right(args[1] as u32)),
        "math.count_ones" => Ok(args[0].count_ones() as i64),
        "math.leading_zeros" => Ok(args[0].leading_zeros() as i64),
        "math.trailing_zeros" => Ok(args[0].trailing_zeros() as i64),
        "math.byte_swap" => Ok(args[0].swap_bytes()),
        "#addr.lt" => Ok(i64::from((args[0] as u64) < args[1] as u64)),
        "#addr.le" => Ok(i64::from((args[0] as u64) <= args[1] as u64)),
        "#addr.gt" => Ok(i64::from((args[0] as u64) > args[1] as u64)),
        "#addr.ge" => Ok(i64::from((args[0] as u64) >= args[1] as u64)),
        "sys.address_distance" => (args[1] as u64)
            .checked_sub(args[0] as u64)
            .and_then(|n| i64::try_from(n).ok())
            .ok_or("address distance is negative or exceeds i64".into()),
        "sys.memory_copy" | "sys.memory_fill" => Err(format!(
            "{name}: requires native machine-code execution; portable Run does not expose host addresses"
        )),
        "math.sqrt" => mira_core::vm::f32_word(mira_core::vm::word_f32(args[0])?.sqrt()),
        "math.to_f32" => mira_core::vm::f32_word(args[0] as f32),
        "math.bits" | "#f32.bits" => {
            mira_core::vm::word_f32(args[0])?;
            Ok(args[0])
        }
        "math.from_bits" | "#f32.from_bits" => {
            mira_core::vm::word_f32(args[0])?;
            Ok(args[0])
        }
        "#graph.require" => {
            if args[0] != 0 {
                Ok(0)
            } else {
                Err("graph output requires exactly one active delivery".into())
            }
        }
        "sys.addr" | "sys.address_value" => Ok(args[0]),
        "sys.align_up" => {
            let alignment = args[1];
            if alignment <= 0 || !(alignment as u64).is_power_of_two() {
                return Err("alignment must be a positive power of two".into());
            }
            (args[0] as u64)
                .checked_add(alignment as u64 - 1)
                .map(|n| (n & !(alignment as u64 - 1)) as i64)
                .ok_or("address alignment overflow".into())
        }
        "sys.offset" => (args[0] as u64)
            .checked_add_signed(args[1])
            .map(|n| n as i64)
            .ok_or("address offset overflow".into()),
        _ => Err(format!(
            "{name}: system operation requires build-kernel and execution in QEMU; host memory and ports are not accessible"
        )),
    }
}

/// These take a compile-time behavior symbol, not a runtime address/value.
pub(crate) fn handler_kind(name: &str) -> Option<bool> {
    match name {
        "sys.interrupt_handler" => Some(false),
        "sys.exception_handler" => Some(true),
        _ => None,
    }
}
