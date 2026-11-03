//! Activation-owned aggregate storage. Handles never refer to host addresses.
use mira_core::*;
pub const ARENA_BYTES: usize = 512 * 1024;
#[derive(Clone, Debug)]
pub struct Object {
    pub kind: Id,
    pub words: Vec<i64>,
    generation: u32,
}
#[derive(Clone, Debug)]
pub struct Arena {
    objects: Vec<Object>,
    next_generation: u32,
    bytes: usize,
    pub limit: usize,
    pub persistent: std::collections::BTreeMap<Id, (Id, i64)>,
}
impl Default for Arena {
    fn default() -> Self {
        Self {
            objects: Vec::new(),
            next_generation: 1,
            bytes: 0,
            limit: ARENA_BYTES,
            persistent: Default::default(),
        }
    }
}
impl Arena {
    pub fn mark(&self) -> (usize, usize) {
        (self.objects.len(), self.bytes)
    }
    pub fn reclaim(&mut self, mark: (usize, usize)) {
        self.objects.truncate(mark.0);
        self.bytes = mark.1;
    }

    pub fn allocate(&mut self, kind: Id, words: Vec<i64>) -> Result<i64> {
        self.allocate_aligned(kind, words, 8)
    }
    pub fn allocate_aligned(&mut self, kind: Id, words: Vec<i64>, alignment: i64) -> Result<i64> {
        if !(8..=4096).contains(&alignment) || !(alignment as u64).is_power_of_two() {
            return Err("data alignment must be a power of two in 8..4096".into());
        }
        let size = words
            .len()
            .max(1)
            .checked_mul(8)
            .and_then(|n| n.checked_add(alignment as usize - 8))
            .ok_or("data allocation overflow")?;
        if size > self.limit.saturating_sub(self.bytes) {
            return Err("activation data arena exhausted".into());
        }
        // Keys are opaque: reuse of an arena slot must not revive a released key.
        if self.next_generation >= (1 << 31) || self.objects.len() >= u32::MAX as usize {
            return Err("arena handle generation exhausted".into());
        }
        let generation = self.next_generation;
        self.next_generation += 1;
        self.bytes += size;
        self.objects.push(Object {
            kind,
            words,
            generation,
        });
        Ok(((generation as i64) << 32) | self.objects.len() as i64)
    }
    pub fn persistent_buffer(
        &mut self,
        cell: Id,
        ty: Id,
        types: &[data::Definition],
    ) -> Result<i64> {
        if let Some((old, handle)) = self.persistent.get(&cell) {
            if *old != ty {
                return Err("buffer owner type changed".into());
            }
            return Ok(*handle);
        }
        let definition = data::find(types, ty)?;
        let buffer = data::Buffer {
            ty,
            words: vec![0; definition.len()],
        };
        buffer.validate(types)?;
        let handle = self.allocate(ty, buffer.words)?;
        self.persistent.insert(cell, (ty, handle));
        Ok(handle)
    }
    pub fn initialize_buffers(
        &mut self,
        program: &Program,
        buffers: &std::collections::BTreeMap<Id, data::Buffer>,
    ) -> Result<()> {
        for (cell, buffer) in buffers {
            buffer.validate(&program.data_types())?;
            let handle = self.allocate(buffer.ty, buffer.words.clone())?;
            self.persistent.insert(*cell, (buffer.ty, handle));
        }
        Ok(())
    }
    pub fn export_buffers(&self) -> Result<std::collections::BTreeMap<Id, data::Buffer>> {
        self.persistent
            .iter()
            .map(|(id, (ty, handle))| {
                Ok((
                    *id,
                    data::Buffer {
                        ty: *ty,
                        words: self.object(*handle)?.words.clone(),
                    },
                ))
            })
            .collect()
    }
    pub(crate) fn restore(&mut self, mut snapshot: Self) {
        // Rollback also retires keys allocated by the failed transaction.
        snapshot.next_generation = snapshot.next_generation.max(self.next_generation);
        *self = snapshot;
    }
    fn index(&self, handle: i64) -> Result<usize> {
        let slot = (handle as u32)
            .checked_sub(1)
            .ok_or("invalid aggregate handle")? as usize;
        let generation = (handle as u64 >> 32) as u32;
        if handle <= 0
            || self
                .objects
                .get(slot)
                .is_none_or(|o| o.generation != generation)
        {
            return Err("invalid or stale aggregate handle".into());
        }
        Ok(slot)
    }
    pub fn object(&self, handle: i64) -> Result<&Object> {
        Ok(&self.objects[self.index(handle)?])
    }
    pub fn get(&self, handle: i64, index: usize) -> Result<i64> {
        self.object(handle)?
            .words
            .get(index)
            .copied()
            .ok_or("array index out of bounds".into())
    }
    pub fn set(&mut self, handle: i64, index: usize, value: i64) -> Result<()> {
        let slot = self.index(handle)?;
        *self.objects[slot]
            .words
            .get_mut(index)
            .ok_or("array index out of bounds")? = value;
        Ok(())
    }
    pub fn format(&self, types: &[data::Definition], ty: Type, value: i64) -> String {
        fn walk(
            arena: &Arena,
            types: &[data::Definition],
            ty: Type,
            value: i64,
            depth: usize,
        ) -> String {
            if depth > 4 {
                return "…".into();
            }
            match ty {
                Type::Infer => "<unresolved type>".into(),
                Type::F32 => f32::from_bits(value as u32).to_string(),
                Type::Bool => (value != 0).to_string(),
                Type::Addr => format!("0x{:x}", value as u64),
                Type::Unit => "()".into(),
                Type::Event => "event".into(),
                Type::I64 => value.to_string(),
                Type::Data(id) => {
                    let Ok(d) = data::find(types, id) else {
                        return "<unknown type>".into();
                    };
                    let Ok(o) = arena.object(value) else {
                        return "<unavailable aggregate>".into();
                    };
                    let fields = d.member_types();
                    let mut parts = Vec::new();
                    for (i, (ty, v)) in fields.iter().zip(&o.words).enumerate().take(12) {
                        let item = walk(arena, types, *ty, *v, depth + 1);
                        if let data::Kind::Record(names) = &d.kind {
                            parts.push(format!("{}: {item}", names[i].name));
                        } else {
                            parts.push(item);
                        }
                    }
                    if fields.len() > 12 {
                        parts.push("…".into());
                    }
                    match d.kind {
                        data::Kind::Record(_) => format!("{} {{ {} }}", d.name, parts.join(", ")),
                        data::Kind::Array { .. } | data::Kind::Tensor { .. } => {
                            format!("{} [{}]", d.name, parts.join(", "))
                        }
                    }
                }
            }
        }
        walk(self, types, ty, value, 0)
    }
}

#[cfg(test)]
mod tests {
    use super::Arena;
    #[test]
    fn rollback_retires_keys_and_generation_exhaustion_does_not_wrap() {
        let mut arena = Arena::default();
        let saved = arena.clone();
        let discarded = arena.allocate(1, vec![11]).unwrap();
        arena.restore(saved);
        let live = arena.allocate(1, vec![23]).unwrap();
        assert_ne!(discarded, live);
        assert!(arena.get(discarded, 0).is_err());
        arena.next_generation = (1 << 31) - 1;
        let last = arena.allocate(1, vec![42]).unwrap();
        let mark = arena.mark();
        assert!(
            arena
                .allocate(1, vec![99])
                .unwrap_err()
                .contains("generation exhausted")
        );
        assert_eq!(arena.mark(), mark);
        assert_eq!(arena.get(live, 0).unwrap(), 23);
        assert_eq!(arena.get(last, 0).unwrap(), 42);
    }
}
