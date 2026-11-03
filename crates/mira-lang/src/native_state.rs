//! Layout of image-owned shared state. CPU-private runtime state stays in registers.
//! A writer owns the inactive bank until commit/abort; readers pin publication.
pub(crate) const VERSION: usize = 0;
pub(crate) const PUBLISHED: usize = 1;
pub(crate) const WRITER: usize = 2;
pub(crate) const GATE: usize = 3;
pub(crate) const READERS: usize = 4;
pub(crate) const WORDS: usize = 5;
pub(crate) fn slot(state_words: usize, field: usize) -> usize {
    state_words * 2 + field
}
