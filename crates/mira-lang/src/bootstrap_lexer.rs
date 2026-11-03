//! Mira owns lexical scanning and binding power; Rust transports bytes and maps IR tags.
//! Only this component's initial load uses the explicit stage-0 frontend.
use mira_core::{
    code::{Assign, Binary, Unary},
    Id, Result,
};
use std::sync::OnceLock;

struct Lexer {
    code: &'static crate::behavior::Prepared,
    binary: Id,
    unary: Id,
    assignment: Id,
    scan: Id,
    input: Id,
    state: Id,
}
const BINARY_TAGS: &[Binary] = &[
    Binary::Or,
    Binary::And,
    Binary::Eq,
    Binary::Ne,
    Binary::Lt,
    Binary::Le,
    Binary::Gt,
    Binary::Ge,
    Binary::BitOr,
    Binary::BitXor,
    Binary::BitAnd,
    Binary::Shl,
    Binary::Shr,
    Binary::Add,
    Binary::Sub,
    Binary::Mul,
    Binary::Div,
    Binary::Rem,
];
const ASSIGN_TAGS: &[Assign] = &[
    Assign::Set,
    Assign::Add,
    Assign::Sub,
    Assign::Mul,
    Assign::Div,
    Assign::Rem,
];
const UNARY_TAGS: &[Unary] = &[Unary::Neg, Unary::Plus, Unary::BitNot, Unary::Not];
fn lexer() -> Result<&'static Lexer> {
    static LEXER: OnceLock<Result<Lexer>> = OnceLock::new();
    LEXER
        .get_or_init(|| {
            let library = crate::bootstrap_compiler::library()?;
            let program = &library.program;
            let entry = |name: &str| {
                program
                    .code_modules
                    .iter()
                    .find(|d| d.name == name)
                    .map(|d| d.id)
                    .ok_or_else(|| format!("Mira operator component missing {name}"))
            };
            let ty = |name: &str| {
                program
                    .global_types
                    .iter()
                    .find(|d| d.name == name)
                    .map(|d| d.id)
                    .ok_or_else(|| format!("Mira lexer missing type {name}"))
            };
            Ok(Lexer {
                code: &library.code,
                binary: entry("operator_descriptor")?,
                unary: entry("unary_descriptor")?,
                assignment: entry("assignment_descriptor")?,
                scan: entry("compiler")?,
                input: ty("LexBytes")?,
                state: ty("LexState")?,
            })
        })
        .as_ref()
        .map_err(Clone::clone)
}
fn packed(text: &str) -> Option<i64> {
    if text.is_empty() || text.len() > 3 {
        return None;
    }
    Some(
        text.bytes()
            .enumerate()
            .fold(0i64, |n, (i, b)| n | ((b as i64) << (i * 8))),
    )
}
pub(crate) fn binary(text: &str) -> Result<Option<(u8, Binary)>> {
    let Some(word) = packed(text) else {
        return Ok(None);
    };
    let ops = lexer()?;
    let encoded = ops.code.call(
        ops.binary,
        &[word, text.len() as i64],
        &mut crate::data_runtime::Arena::default(),
    )?;
    if encoded == -1 {
        return Ok(None);
    }
    if encoded < 0 || !(1..=9).contains(&(encoded >> 8)) {
        return Err("invalid binding power from Mira operator decoder".into());
    }
    let op = *BINARY_TAGS
        .get((encoded & 255) as usize)
        .ok_or("invalid core tag from Mira operator decoder")?;
    Ok(Some(((encoded >> 8) as u8, op)))
}
pub(crate) fn unary(text: &str) -> Result<Option<(u8, Unary)>> {
    let Some(word) = packed(text) else {
        return Ok(None);
    };
    let ops = lexer()?;
    let encoded = ops.code.call(
        ops.unary,
        &[word, text.len() as i64],
        &mut crate::data_runtime::Arena::default(),
    )?;
    if encoded == -1 {
        return Ok(None);
    }
    if encoded < 0 || encoded >> 8 != 10 {
        return Err("invalid binding power from Mira prefix decoder".into());
    }
    let op = *UNARY_TAGS
        .get((encoded & 255) as usize)
        .ok_or("invalid core tag from Mira prefix decoder")?;
    Ok(Some(((encoded >> 8) as u8, op)))
}
pub(crate) fn assignment(text: &str) -> Result<Option<Assign>> {
    let Some(word) = packed(text) else {
        return Ok(None);
    };
    let ops = lexer()?;
    let tag = ops.code.call(
        ops.assignment,
        &[word, text.len() as i64],
        &mut crate::data_runtime::Arena::default(),
    )?;
    if tag == -1 {
        return Ok(None);
    }
    ASSIGN_TAGS
        .get(tag as usize)
        .copied()
        .map(Some)
        .ok_or_else(|| "invalid assignment tag from Mira operator decoder".into())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TokenKind {
    Word,
    Text,
    Quoted,
    Whitespace,
    Comment,
}
pub(crate) struct Scanned {
    pub kind: TokenKind,
    pub end: usize,
}
/// A line can span arbitrarily many windows within the frontend source budget.
/// Only the current window and scanner state occupy the existing VM arena.
pub(crate) struct Scanner<'a> {
    lexer: &'static Lexer,
    line: &'a str,
    arena: crate::data_runtime::Arena,
    input: i64,
    state: i64,
    base: usize,
    end: usize,
}
impl<'a> Scanner<'a> {
    pub(crate) fn new(line: &'a str) -> Result<Self> {
        let lexer = lexer()?;
        let mut arena = crate::data_runtime::Arena::default();
        let input = arena.allocate(lexer.input, vec![0; 4096])?;
        let state = arena.allocate(lexer.state, vec![0; 3])?;
        Ok(Self {
            lexer,
            line,
            arena,
            input,
            state,
            base: 0,
            end: 0,
        })
    }
    fn refill(&mut self, position: usize) -> Result<()> {
        self.base = position;
        self.end = (position + 4096).min(self.line.len());
        for (index, byte) in self.line.as_bytes()[self.base..self.end].iter().enumerate() {
            self.arena.set(self.input, index, i64::from(*byte))?;
        }
        Ok(())
    }
    pub(crate) fn scan(&mut self, start: usize) -> Result<Scanned> {
        if start >= self.line.len() || !self.line.is_char_boundary(start) {
            return Err("invalid lexer start offset".into());
        }
        self.arena.set(self.state, 0, 0)?;
        self.arena.set(self.state, 1, 0)?;
        let mut position = start;
        loop {
            if position < self.base || position >= self.end {
                self.refill(position)?;
            }
            let before = position - self.base;
            let length = self.end - self.base;
            let final_window = self.end == self.line.len();
            let status = self.lexer.code.call(
                self.lexer.scan,
                &[
                    self.input,
                    before as i64,
                    length as i64,
                    i64::from(final_window),
                    self.state,
                ],
                &mut self.arena,
            )?;
            let cursor = self.arena.get(self.state, 2)?;
            if cursor < before as i64 || cursor > length as i64 {
                return Err("invalid cursor from Mira lexer".into());
            }
            position = self.base + cursor as usize;
            if status == 0 {
                if final_window || position == self.base {
                    return Err("Mira lexer made no progress".into());
                }
                self.refill(position)?;
                continue;
            }
            let kind = match status {
                1 => TokenKind::Word,
                2 => TokenKind::Text,
                3 => TokenKind::Quoted,
                4 => TokenKind::Whitespace,
                5 => TokenKind::Comment,
                -1 => return Err("unsupported character in scalar Mira".into()),
                -2 => return Err("unclosed text literal".into()),
                -3 => return Err("expected a closed quoted identifier such as `my-cell`".into()),
                _ => return Err("invalid result from Mira lexer".into()),
            };
            if position <= start || !self.line.is_char_boundary(position) {
                return Err("invalid token boundary from Mira lexer".into());
            }
            if kind == TokenKind::Quoted
                && (self.line.as_bytes()[start] != b'`'
                    || self.line.as_bytes()[position - 1] != b'`')
            {
                return Err("invalid quoted name from Mira lexer".into());
            }
            return Ok(Scanned {
                kind,
                end: position,
            });
        }
    }
}
