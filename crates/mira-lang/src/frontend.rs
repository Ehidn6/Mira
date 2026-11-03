//! One layout frontend for graph structure and scalar cell behavior.
use mira_core::{code::*, *};
use std::collections::{BTreeMap, BTreeSet};
#[derive(Clone, serde::Serialize)]
pub(crate) struct Token {
    text: String,
    line: usize,
    column: usize,
}
fn lex(source: &str, seed_frontend: bool) -> Result<Vec<Token>> {
    use crate::bootstrap_lexer::{Scanner, TokenKind};
    let mut tokens = Vec::new();
    let mut levels = vec![0];
    let mut brackets = 0i32;
    for (ln, line) in source.lines().enumerate() {
        let b = line.as_bytes();
        let mut i = 0;
        while i < b.len() && b[i] == b' ' {
            i += 1;
        }
        if b.get(i) == Some(&b'\t') {
            return Err(format!("{}:{}: use spaces for indentation", ln + 1, i + 1));
        }
        if i == b.len() || line[i..].starts_with("//") {
            continue;
        }
        if brackets == 0 {
            if i > *levels.last().unwrap() {
                levels.push(i);
                tokens.push(Token {
                    text: "<indent>".into(),
                    line: ln + 1,
                    column: 1,
                });
            } else {
                while i < *levels.last().unwrap() {
                    levels.pop();
                    tokens.push(Token {
                        text: "<dedent>".into(),
                        line: ln + 1,
                        column: 1,
                    });
                }
                if i != *levels.last().unwrap() {
                    return Err(format!("{}:1: inconsistent indentation", ln + 1));
                }
            }
            if levels.len() > MAX_DEPTH * 3 + 8 {
                return Err(format!("{}:1: nesting budget exceeded", ln + 1));
            }
        }
        let mut scanner = if seed_frontend {
            None
        } else {
            Some(Scanner::new(line)?)
        };
        while i < b.len() {
            let start = i;
            let scanned = if let Some(scanner) = &mut scanner {
                scanner.scan(start)
            } else {
                seed_token(line, start)
            }
            .map_err(|e| format!("{}:{}: {e}", ln + 1, start + 1))?;
            i = scanned.end;
            match scanned.kind {
                TokenKind::Whitespace => continue,
                TokenKind::Comment => break,
                _ => (),
            }
            let text = if scanned.kind == TokenKind::Quoted {
                let name = &line[start + 1..i - 1];
                if !name_ok(name) {
                    return Err(format!(
                        "{}:{}: expected a closed quoted identifier such as `my-cell`",
                        ln + 1,
                        start + 1
                    ));
                }
                name
            } else {
                &line[start..i]
            };
            if scanned.kind == TokenKind::Word {
                if b[start] == b'(' || b[start] == b'[' {
                    brackets += 1;
                }
                if b[start] == b')' || b[start] == b']' {
                    brackets -= 1;
                    if brackets < 0 {
                        return Err(format!(
                            "{}:{}: unmatched closing bracket",
                            ln + 1,
                            start + 1
                        ));
                    }
                }
            }
            tokens.push(Token {
                text: text.into(),
                line: ln + 1,
                column: start + 1,
            });
            if tokens.len() > 200_000 {
                return Err("token budget exceeded".into());
            }
        }
        if brackets == 0 {
            tokens.push(Token {
                text: "\n".into(),
                line: ln + 1,
                column: line.len() + 1,
            });
        }
        if tokens.len() > 200_000 {
            return Err("token budget exceeded".into());
        }
    }
    if brackets != 0 {
        return Err("unclosed bracket".into());
    }
    while levels.len() > 1 {
        levels.pop();
        tokens.push(Token {
            text: "<dedent>".into(),
            line: source.lines().count() + 1,
            column: 1,
        });
    }
    tokens.push(Token {
        text: "<eof>".into(),
        line: source.lines().count() + 1,
        column: 1,
    });
    if tokens.len() > 200_000 {
        return Err("token budget exceeded".into());
    }
    Ok(tokens)
}

// Stage 0 is used only to load Mira's own immutable frontend components.
fn seed_token(line: &str, start: usize) -> Result<crate::bootstrap_lexer::Scanned> {
    use crate::bootstrap_lexer::{Scanned, TokenKind};
    let b = line.as_bytes();
    let mut i = start;
    let kind;
    if b[i].is_ascii_whitespace() {
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        kind = TokenKind::Whitespace;
    } else if line[i..].starts_with("//") {
        i = b.len();
        kind = TokenKind::Comment;
    } else if b[i] == b'"' {
        i += 1;
        while i < b.len() && b[i] != b'"' {
            if b[i] == b'\\' {
                i += 1;
            }
            i += 1;
        }
        if i >= b.len() {
            return Err("unclosed text literal".into());
        }
        i += 1;
        kind = TokenKind::Text;
    } else if b[i] == b'`' {
        i += 1;
        while i < b.len() && b[i] != b'`' {
            i += 1;
        }
        if i == b.len() {
            return Err("expected a closed quoted identifier such as `my-cell`".into());
        }
        i += 1;
        kind = TokenKind::Quoted;
    } else if b[i].is_ascii_alphabetic() || b[i] == b'_' {
        i += 1;
        while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
            i += 1;
        }
        kind = TokenKind::Word;
    } else if b[i] == b'0' && b.get(i + 1).is_some_and(|c| matches!(c, b'x' | b'b')) {
        i += 2;
        while i < b.len() && b[i].is_ascii_alphanumeric() {
            i += 1;
        }
        kind = TokenKind::Word;
    } else if b[i].is_ascii_digit() {
        i += 1;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        if b.get(i) == Some(&b'.') && b.get(i + 1).is_some_and(u8::is_ascii_digit) {
            i += 1;
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
            }
        }
        if b.get(i).is_some_and(|c| matches!(c, b'e' | b'E')) {
            i += 1;
            if b.get(i).is_some_and(|c| matches!(c, b'+' | b'-')) {
                i += 1;
            }
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
            }
        }
        kind = TokenKind::Word;
    } else {
        let width = seed_punctuation(&line[i..]);
        if width == 0 {
            return Err("unsupported character in scalar Mira".into());
        }
        i += width;
        kind = TokenKind::Word;
    }
    Ok(Scanned { kind, end: i })
}
struct Pending {
    space: Id,
    id: Id,
    from: String,
    from_port: Option<String>,
    to: String,
    to_port: Option<String>,
}
struct Parser<'a> {
    source: &'a str,
    seed_frontend: bool,
    tokens: Vec<Token>,
    at: usize,
    used: BTreeSet<Id>,
    reserved: BTreeSet<Id>,
    next: Id,
    pending: Vec<Pending>,
    constructors: Vec<(Id, Id, String)>,
    adapters: BTreeSet<Id>,
    depth: usize,
    type_names: BTreeMap<String, Id>,
    hyphenated_names: BTreeSet<String>,
    implicit_ids: Vec<(usize, Id)>,
}
impl Parser<'_> {
    fn peek(&self) -> &str {
        self.tokens
            .get(self.at)
            .map_or("<eof>", |t| t.text.as_str())
    }
    fn look(&self, n: usize) -> &str {
        self.tokens
            .get(self.at + n)
            .map_or("<eof>", |t| t.text.as_str())
    }
    fn error(&self, msg: &str) -> String {
        let t = &self.tokens[self.at.min(self.tokens.len() - 1)];
        format!("{}:{}: {msg}", t.line, t.column)
    }
    fn take(&mut self) -> Result<String> {
        if self.peek() == "<eof>" {
            return Err(self.error("unexpected end of file"));
        }
        let s = self.tokens[self.at].text.clone();
        self.at += 1;
        Ok(s)
    }
    fn eat(&mut self, s: &str) -> bool {
        if self.peek() == s {
            self.at += 1;
            true
        } else {
            false
        }
    }
    fn expect(&mut self, s: &str) -> Result<()> {
        if self.eat(s) {
            Ok(())
        } else {
            Err(self.error(&format!("expected {s}, found {}", self.peek())))
        }
    }
    fn identifier(&mut self) -> Result<String> {
        if !name_ok(self.peek()) {
            Err(self.error("expected a name"))
        } else {
            self.take()
        }
    }
    // Legacy graph/declaration names retain hyphens. Expressions require an
    // explicitly quoted identifier, so a-b always means subtraction.
    fn name(&mut self) -> Result<String> {
        let mut name = self.identifier()?;
        while self.peek() == "-" && name_ok(self.look(1)) {
            self.take()?;
            name.push('-');
            name.push_str(&self.identifier()?);
        }
        if name.contains('-') {
            self.hyphenated_names.insert(name.clone());
        }
        Ok(name)
    }
    fn expression_identifier(&mut self) -> Result<String> {
        let mut candidate = self.peek().to_string();
        let mut at = self.at;
        while self.tokens.get(at + 1).is_some_and(|t| t.text == "-")
            && self.tokens.get(at + 2).is_some_and(|t| name_ok(&t.text))
            && self.tokens[at].line == self.tokens[at + 2].line
            && self.tokens[at].column + self.tokens[at].text.len() == self.tokens[at + 1].column
            && self.tokens[at + 1].column + 1 == self.tokens[at + 2].column
        {
            candidate.push('-');
            candidate.push_str(&self.tokens[at + 2].text);
            at += 2;
            if self.hyphenated_names.contains(&candidate) {
                return Err(self.error(&format!("legacy identifier `{candidate}` requires backticks in expressions; use spaces for subtraction")));
            }
        }
        self.identifier()
    }
    fn natural(&mut self) -> Result<Id> {
        natural_value(&self.take()?, self.seed_frontend).map_err(|e| self.error(&e))
    }
    fn id(&mut self) -> Result<Id> {
        let id = if self.eat("@") {
            self.natural()?
        } else {
            while self.used.contains(&self.next) || self.reserved.contains(&self.next) {
                self.next = self.next.checked_add(1).ok_or("identity exhausted")?;
            }
            let id = self.next;
            self.implicit_ids.push((self.at, id));
            id
        };
        if id == 0 || !self.used.insert(id) {
            return Err(self.error("zero or duplicate identity"));
        }
        if self.used.len() > MAX_ELEMENTS {
            return Err(self.error("element budget exceeded"));
        }
        Ok(id)
    }
    fn ty(&mut self) -> Result<Type> {
        match self.take()?.as_str() {
            "_" => Ok(Type::Infer),
            "i64" => Ok(Type::I64),
            "f32" => Ok(Type::F32),
            "bool" => Ok(Type::Bool),
            "unit" => Ok(Type::Unit),
            "event" => Ok(Type::Event),
            "addr" => Ok(Type::Addr),
            name => self
                .type_names
                .get(name)
                .copied()
                .map(Type::Data)
                .ok_or_else(|| self.error("unknown type")),
        }
    }
    fn signed(&mut self) -> Result<i64> {
        let negative = self.eat("-");
        let text = self.take()?;
        self.integer_value(&text, negative)
    }
    fn integer_value(&self, text: &str, negative: bool) -> Result<i64> {
        if !self.seed_frontend {
            return crate::bootstrap_literals::parse(text, negative).map_err(|e| self.error(&e));
        }
        let number = seed_integer(text).ok_or_else(|| self.error("expected i64 literal"))?;
        i64::try_from(if negative { -number } else { number })
            .map_err(|_| self.error("literal outside i64 range"))
    }
    fn float_value(&self, text: &str) -> Result<f32> {
        if !self.seed_frontend {
            return crate::bootstrap_literals::float(text).map_err(|e| self.error(&e));
        }
        let value = text.parse::<f32>().map_err(|_| self.error("invalid f32 literal"))?;
        if !value.is_finite() {
            return Err(self.error("nonfinite f32 literal"));
        }
        Ok(value)
    }
    fn binary(&self, text: &str) -> Result<Option<(u8, Binary)>> {
        if self.seed_frontend {
            Ok(seed_binary(text))
        } else {
            crate::bootstrap_lexer::binary(text).map_err(|e| self.error(&e))
        }
    }
    fn assignment(&self, text: &str) -> Result<Option<Assign>> {
        if self.seed_frontend {
            Ok(seed_assignment(text))
        } else {
            crate::bootstrap_lexer::assignment(text).map_err(|e| self.error(&e))
        }
    }
    fn unary(&self, text: &str) -> Result<Option<(u8, Unary)>> {
        if self.seed_frontend {
            Ok(seed_unary(text))
        } else {
            crate::bootstrap_lexer::unary(text).map_err(|e| self.error(&e))
        }
    }
    fn comparison(&self) -> Result<Option<Comparison>> {
        Ok(self.binary(self.peek())?.and_then(|(_, op)| match op {
            Binary::Lt => Some(Comparison::Lt),
            Binary::Le => Some(Comparison::Le),
            Binary::Gt => Some(Comparison::Gt),
            Binary::Ge => Some(Comparison::Ge),
            Binary::Eq => Some(Comparison::Eq),
            Binary::Ne => Some(Comparison::Ne),
            _ => None,
        }))
    }
    fn position(&mut self, default: [f32; 3]) -> Result<[f32; 3]> {
        if !self.eat("at") {
            return Ok(default);
        }
        self.expect("(")?;
        let mut p = [0.; 3];
        for (i, x) in p.iter_mut().enumerate() {
            if i > 0 {
                self.expect(",")?;
            }
            let sign = if self.eat("-") { -1. } else { 1. };
            let token = self.take()?;
            *x = self.float_value(&token)? * sign;
            if !x.is_finite() {
                return Err(self.error("coordinate must be finite"));
            }
        }
        self.expect(")")?;
        Ok(p)
    }
    fn behavior(&mut self) -> Result<CodeModule> {
        let first_token = self.at;
        let compile_time = self.peek() == "macro" && self.look(1) != "(";
        if compile_time {
            self.take()?;
        }
        let name = self.name()?;
        let id = self.id()?;
        self.expect("(")?;
        let mut parameters = Vec::new();
        if !self.eat(")") {
            loop {
                let name = self.name()?;
                let ty = if self.eat(":") { self.ty()? } else { Type::Infer };
                if ty == Type::Unit {
                    return Err(self.error("unit cannot be an input payload"));
                }
                parameters.push(Parameter { name, ty });
                if self.eat(")") {
                    break;
                }
                self.expect(",")?;
            }
        }
        let annotated = self.eat("->");
        let output_ports = annotated && self.eat("ports");
        let output = if annotated { self.ty()? } else { Type::I64 };
        self.expect("=")?;
        let (data_types, locals, body) = if self.peek() == "\n" {
            self.module_body()?
        } else {
            let e = self.expr(0)?;
            self.expect("\n")?;
            (Vec::new(), Vec::new(), vec![Statement::Expr(e)])
        };
        let mut definition = CodeModule {
            data_types,
            id,
            name,
            parameters,
            output,
            annotated,
            output_ports,
            compile_time,
            body,
            locals,
        };
        if !self.seed_frontend && crate::bootstrap_scalar::eligible(&definition) {
            let first = &self.tokens[first_token];
            let offset = |line: usize| self.source.split_inclusive('\n').take(line.saturating_sub(1)).map(str::len).sum::<usize>();
            let start = offset(first.line) + first.column - 1;
            let end = self.tokens.get(self.at).map_or(self.source.len(),|t| offset(t.line));
            if let Some(text) = self.source.get(start..end) {
                // Wider grammar remains in the existing frontend during porting.
                // Within this admitted profile the Mira result is authoritative:
                // an error does not silently fall back to the Rust expression.
                if text.len() <= 4096 && self.at - first_token <= 512 && !text.contains('`') && !text.contains("use ") && !text.contains("expand ") {
                    let body = crate::bootstrap_scalar::parse(text,&definition.parameters)
                        .map_err(|e|self.error(&e))?;
                    definition.body = body;
                }
            }
        }
        Ok(definition)
    }
    fn definition_ahead(&self) -> bool {
        let mut i = if self.peek() == "macro" && self.look(1) != "(" {
            2
        } else {
            1
        };
        if self.look(i) == "@" {
            i += 2;
        }
        if self.look(i) != "(" {
            return false;
        }
        let mut depth = 0;
        while self.look(i) != "<eof>" && self.look(i) != "\n" {
            match self.look(i) {
                "(" => depth += 1,
                ")" => {
                    depth -= 1;
                    if depth == 0 {
                        i += 1;
                        break;
                    }
                }
                _ => {}
            }
            i += 1;
        }
        if self.look(i) == "->" {
            i += 1;
            if self.look(i) == "ports" {
                i += 1;
            }
            i += 1;
        }
        self.look(i) == "="
    }
    fn module_body(&mut self) -> Result<(Vec<data::Definition>, Vec<CodeModule>, Vec<Statement>)> {
        self.expect("\n")?;
        self.expect("<indent>")?;
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(self.error("code nesting budget exceeded"));
        }
        let mut data_types = Vec::new();
        let mut locals = Vec::new();
        let mut body = Vec::new();
        while self.peek() != "<dedent>" && self.peek() != "<eof>" {
            if matches!(self.peek(), "struct" | "type")
                && self.type_names.contains_key(self.look(1))
            {
                data_types.push(self.data_definition()?);
            } else if self.definition_ahead() {
                locals.push(self.behavior()?);
            } else {
                body.push(self.statement()?);
            }
        }
        self.expect("<dedent>")?;
        self.depth -= 1;
        if body.is_empty() {
            return Err(self.error("cell code requires an entry expression or algorithm"));
        }
        Ok((data_types, locals, body))
    }
    fn data_definition(&mut self) -> Result<data::Definition> {
        let kind = self.take()?;
        let name = self.name()?;
        let id = *self.type_names.get(&name).ok_or("unregistered data type")?;
        if self.eat("@") {
            let explicit = self.natural()?;
            if explicit != id {
                return Err(self.error("data identity mismatch"));
            }
        }
        let kind = if kind == "struct" {
            self.expect(":")?;
            self.expect("\n")?;
            if !self.eat("<indent>") {
                return Ok(data::Definition {
                    id,
                    name,
                    kind: data::Kind::Record(Vec::new()),
                });
            }
            let mut fields = Vec::new();
            while self.peek() != "<dedent>" && self.peek() != "<eof>" {
                let name = self.name()?;
                self.expect(":")?;
                let ty = self.ty()?;
                self.expect("\n")?;
                fields.push(Parameter { name, ty });
            }
            self.expect("<dedent>")?;
            data::Kind::Record(fields)
        } else {
            self.expect("=")?;
            let tensor = self.eat("tensor");
            self.expect("[")?;
            let element = self.ty()?;
            self.expect(",")?;
            let length = usize::try_from(self.natural()?)
                .map_err(|_| self.error("array length too large"))?;
            if tensor {
                let mut shape = vec![length];
                while self.eat(",") {
                    shape.push(
                        usize::try_from(self.natural()?)
                            .map_err(|_| self.error("tensor dimension too large"))?,
                    );
                }
                let count = shape
                    .iter()
                    .try_fold(1usize, |a, b| a.checked_mul(*b))
                    .ok_or_else(|| self.error("tensor shape overflow"))?;
                if shape.len() > 8
                    || shape.contains(&0)
                    || count > data::MAX_WORDS
                    || !matches!(element, Type::F32 | Type::I64 | Type::Bool)
                {
                    return Err(self.error("tensor requires rank 1..8, positive bounded dimensions and scalar elements"));
                }
                self.expect("]")?;
                self.expect("\n")?;
                data::Kind::Tensor { element, shape }
            } else {
                if length > data::MAX_WORDS {
                    return Err(self.error(&format!("array length exceeds {} words", data::MAX_WORDS)));
                }
                self.expect("]")?;
                self.expect("\n")?;
                data::Kind::Array { element, length }
            }
        };
        Ok(data::Definition { id, name, kind })
    }
    fn block(&mut self) -> Result<Vec<Statement>> {
        self.expect("\n")?;
        self.expect("<indent>")?;
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(self.error("code nesting budget exceeded"));
        }
        let mut body = Vec::new();
        while self.peek() != "<dedent>" && self.peek() != "<eof>" {
            body.push(self.statement()?);
        }
        self.expect("<dedent>")?;
        self.depth -= 1;
        if body.is_empty() {
            return Err(self.error("empty code block"));
        }
        Ok(body)
    }
    fn statement(&mut self) -> Result<Statement> {
        if self.eat("if") {
            return self.if_body();
        }
        if self.eat("while") {
            let condition = self.expr(0)?;
            let body = self.block()?;
            return Ok(Statement::While { condition, body });
        }
        if self.eat("for") {
            let name = self.identifier()?;
            self.expect("in")?;
            let start = self.expr(0)?;
            self.expect("..")?;
            let end = self.expr(0)?;
            let body = self.block()?;
            return Ok(Statement::For {
                name,
                start,
                end,
                body,
            });
        }
        let immutable = self.eat("const");
        if immutable || self.look(1) == ":" || self.assignment(self.look(1))?.is_some() {
            let name = self.identifier()?;
            let ty = if self.eat(":") {
                Some(self.ty()?)
            } else {
                None
            };
            let token = self.take()?;
            let op = self
                .assignment(&token)?
                .ok_or_else(|| self.error("expected assignment"))?;
            let value = self.expr(0)?;
            self.expect("\n")?;
            return Ok(Statement::Bind {
                name,
                ty,
                immutable,
                op,
                value,
            });
        }
        let value = self.expr(0)?;
        if let Some(op) = self.assignment(self.peek())? {
            self.take()?;
            let rhs = self.expr(0)?;
            self.expect("\n")?;
            return Ok(Statement::Set {
                target: value,
                op,
                value: rhs,
            });
        }
        self.expect("\n")?;
        Ok(Statement::Expr(value))
    }
    fn if_body(&mut self) -> Result<Statement> {
        let condition = self.expr(0)?;
        let yes = self.block()?;
        let no = if self.eat("else") {
            if self.eat("if") {
                vec![self.if_body()?]
            } else {
                self.block()?
            }
        } else {
            Vec::new()
        };
        Ok(Statement::If { condition, yes, no })
    }
    fn expr(&mut self, min: u8) -> Result<Expr> {
        self.depth += 1;
        if self.depth > 96 {
            return Err(self.error("expression nesting budget exceeded"));
        }
        let mut lhs = if self.eat("?") {
            let label = if name_ok(self.peek()) { self.identifier()? } else { String::new() };
            Expr::Hole(label)
        } else if let Some((precedence, op)) = self.unary(self.peek())? {
            self.take()?;
            if op == Unary::Neg
                && self
                    .peek()
                    .bytes()
                    .next()
                    .is_some_and(|b| b.is_ascii_digit())
            {
                let token = self.take()?;
                if is_float(&token) {
                    let v = self.float_value(&token)?;
                    Expr::Float((-v).to_bits())
                } else {
                    Expr::Int(self.integer_value(&token, true)?)
                }
            } else {
                Expr::Unary(op, Box::new(self.expr(precedence)?))
            }
        } else if self.eat("(") {
            if self.eat(")") {
                Expr::Unit
            } else {
                let e = self.expr(0)?;
                self.expect(")")?;
                e
            }
        } else if self.eat("true") {
            Expr::Bool(true)
        } else if self.eat("false") {
            Expr::Bool(false)
        } else if self
            .peek()
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_digit())
        {
            if is_float(self.peek()) {
                let token = self.take()?;
                let value = self.float_value(&token)?;
                Expr::Float(value.to_bits())
            } else {
                Expr::Int(self.signed()?)
            }
        } else {
            let mut name = self.expression_identifier()?;
            // Qualified definition calls share the lexical Z tree. Record field
            // expressions remain fields unless the path is followed by a call.
            let mut look = 0;
            while self.look(look) == "." && name_ok(self.look(look + 1)) {
                look += 2;
            }
            if look > 0 && self.look(look) == "(" && !self.type_names.contains_key(&name) {
                while self.eat(".") {
                    name.push('.');
                    name.push_str(&self.identifier()?);
                }
            }
            if (matches!(
                name.as_str(),
                "cells"
                    | "sys"
                    | "graph"
                    | "tensor"
                    | "math"
                    | "relation"
                    | "reflect"
                    | "diff"
                    | "meta"
            ) || self.type_names.contains_key(&name))
                && self.eat(".")
            {
                let member = if self.type_names.contains_key(&name) {
                    let member = self.take()?;
                    if !matches!(
                        member.as_str(),
                        "at" | "persist" | "size_bytes" | "alignment" | "offset_of" | "aligned"
                    ) {
                        return Err(self.error(
                            "expected at, persist, aligned, size_bytes, alignment or offset_of",
                        ));
                    }
                    member
                } else {
                    self.identifier()?
                };
                name = format!("{name}.{member}");
                if self.peek() != "(" {
                    return Err(self.error("child cell access requires a call: cells.Name(...); quote hyphenated names as cells.`my-cell`(...)"));
                }
            }
            if self.eat("(") {
                if self.peek().starts_with('"') {
                    let id = self.type_names.get(&name).copied().ok_or_else(|| {
                        self.error("text literal requires a named i64 array constructor")
                    })?;
                    let value: String = serde_json::from_str(&self.take()?)
                        .map_err(|e| self.error(&format!("invalid text literal: {e}")))?;
                    if value.len() > 4096 {
                        return Err(self.error("text literal exceeds array limit"));
                    }
                    self.expect(")")?;
                    Expr::Construct(
                        id,
                        vec![Expr::Call(
                            "array.literal_utf8".into(),
                            value.bytes().map(|b| Expr::Int(b as i64)).collect(),
                        )],
                    )
                } else if name.ends_with(".offset_of") {
                    // A field name is layout metadata, never a runtime expression.
                    let field = self.name()?;
                    self.expect(")")?;
                    Expr::Call(format!("{name}.{field}"), vec![])
                } else {
                    let mut args = Vec::new();
                    if !self.eat(")") {
                        loop {
                            if args.is_empty() && (crate::intrinsics::symbol_reference(&name)
                                || matches!(name.as_str(), "sys.scratch" | "sys.neural_try" | "sys.read_only" | "diff.derivative"))
                            {
                                let mut symbol = self.name()?;
                                while self.eat(".") {
                                    symbol.push('.');
                                    symbol.push_str(&self.identifier()?);
                                }
                                args.push(Expr::Name(symbol));
                            } else {
                                args.push(self.expr(0)?);
                            }
                            if self.eat(")") {
                                break;
                            }
                            self.expect(",")?;
                        }
                    }
                    if let Some(id) = self.type_names.get(&name) {
                        Expr::Construct(*id, args)
                    } else if let Some(base) = name.strip_suffix(".at") {
                        if let Some(id) = self.type_names.get(base) {
                            if args.len() != 1 {
                                return Err(self.error("Type.at expects one addr"));
                            }
                            Expr::View(*id, Box::new(args.remove(0)))
                        } else {
                            Expr::Call(name, args)
                        }
                    } else {
                        Expr::Call(name, args)
                    }
                }
            } else {
                Expr::Name(name)
            }
        };
        let mut compared = false;
        loop {
            if self.eat(".") {
                let field = self.identifier()?;
                lhs = Expr::Field(Box::new(lhs), field);
                continue;
            }
            if self.eat("[") {
                let index = self.expr(0)?;
                self.expect("]")?;
                lhs = Expr::Index(Box::new(lhs), Box::new(index));
                continue;
            }
            if self.peek() == "?" && min == 0 {
                self.take()?;
                let yes = self.expr(0)?;
                self.expect(":")?;
                let no = self.expr(0)?;
                lhs = Expr::Choose(Box::new(lhs), Box::new(yes), Box::new(no));
                continue;
            }
            let Some((precedence, op)) = self.binary(self.peek())? else {
                break;
            };
            if precedence < min {
                break;
            }
            self.take()?;
            let rhs = self.expr(precedence + 1)?;
            if matches!(
                op,
                Binary::Eq | Binary::Ne | Binary::Lt | Binary::Le | Binary::Gt | Binary::Ge
            ) {
                if compared {
                    return Err(self.error("comparison chains require 'and'"));
                }
                compared = true;
            }
            lhs = Expr::Binary(op, Box::new(lhs), Box::new(rhs));
        }
        self.depth -= 1;
        Ok(lhs)
    }
    fn space(&mut self) -> Result<Space> {
        self.expect("space")?;
        let name = self.name()?;
        let id = self.id()?;
        let mut s = Space::new(id, name);
        s.position = self.position([0.; 3])?;
        self.expect(":")?;
        self.expect("\n")?;
        if !self.eat("<indent>") {
            return Ok(s);
        }
        if self.depth >= MAX_DEPTH {
            return Err(self.error("space depth budget exceeded"));
        }
        self.depth += 1;
        while self.peek() != "<dedent>" && self.peek() != "<eof>" {
            if self.peek() == "space" {
                return Err(
                    self.error("nested spaces must belong to a graph cell; use name = graph():")
                );
            }
            if matches!(self.peek(), "in" | "out") && self.look(2) == ":" {
                let input = self.take()? == "in";
                let name = self.name()?;
                self.expect(":")?;
                let ty = self.ty()?;
                self.expect("\n")?;
                if input {
                    s.inputs.push(Parameter { name, ty });
                } else {
                    s.outputs.push(Parameter { name, ty });
                }
                continue;
            }
            let name = self.name()?;
            if matches!(self.peek(), "." | "->") {
                let explicit = if self.eat(".") {
                    Some(self.name()?)
                } else {
                    None
                };
                self.expect("->")?;
                loop {
                    let selector = if self.look(1) == ":" {
                        let port = self.name()?;
                        self.expect(":")?;
                        Some(port)
                    } else {
                        None
                    };
                    if explicit.is_some() && selector.is_some() {
                        return Err(self.error("source port was specified twice"));
                    }
                    let to = self.name()?;
                    let to_port = if self.eat(".") {
                        Some(self.name()?)
                    } else {
                        None
                    };
                    let id = self.id()?;
                    self.pending.push(Pending {
                        space: s.id,
                        id,
                        from: name.clone(),
                        from_port: selector.or_else(|| explicit.clone()),
                        to,
                        to_port,
                    });
                    if !self.eat(",") {
                        break;
                    }
                }
                self.expect("\n")?;
                continue;
            }
            let id = self.id()?;
            self.expect("=")?;
            let (definition, parameter) = if self.eat("+") {
                (Definition::Add, self.signed()?)
            } else if self.eat("*") {
                (Definition::Mul, self.signed()?)
            } else if let Some(cmp) = self.comparison()? {
                self.take()?;
                let (ty, value) = if self.eat("true") {
                    (Type::Bool, 1)
                } else if self.eat("false") {
                    (Type::Bool, 0)
                } else {
                    (Type::I64, self.signed()?)
                };
                (Definition::Branch(cmp, ty), value)
            } else if self.eat("true") {
                (Definition::ConstBool, 1)
            } else if self.eat("false") {
                (Definition::ConstBool, 0)
            } else if self.peek() == "-"
                || self
                    .peek()
                    .bytes()
                    .next()
                    .is_some_and(|b| b.is_ascii_digit())
            {
                (Definition::Const, self.signed()?)
            } else {
                let mut behavior = self.name()?;
                while self.eat(".") {
                    behavior.push('.');
                    behavior.push_str(&self.identifier()?);
                }
                self.expect("(")?;
                if behavior == "adaptive" {
                    let mut name = self.name()?;
                    while self.eat(".") {
                        name.push('.');
                        name.push_str(&self.identifier()?);
                    }
                    self.expect(")")?;
                    self.constructors.push((s.id, id, format!("@{name}")));
                    (Definition::Adaptive(0), 0)
                } else if ["add", "mul", "const", "bool", "delay", "neural"]
                    .contains(&behavior.as_str())
                {
                    let value = self.signed()?;
                    self.expect(")")?;
                    (
                        match behavior.as_str() {
                            "add" => Definition::Add,
                            "mul" => Definition::Mul,
                            "delay" => Definition::Delay,
                            "neural" => Definition::Neural,
                            "bool" => Definition::ConstBool,
                            _ => Definition::Const,
                        },
                        value,
                    )
                } else {
                    if self.peek() != ")" {
                        return Err(self.error(
                            "cell inputs are connected with -> to named ports; call a function with arguments inside code, or create the cell with empty () and connect its inputs",
                        ));
                    }
                    self.expect(")")?;
                    match behavior.as_str() {
                        "graph" => (Definition::Graph, 0),
                        "as_event" => {
                            self.adapters.insert(id);
                            (Definition::AsEvent(Type::I64), 0)
                        }
                        "as_event_i64" => (Definition::AsEvent(Type::I64), 0),
                        "as_event_bool" => (Definition::AsEvent(Type::Bool), 0),
                        "as_event_event" => (Definition::AsEvent(Type::Event), 0),
                        "as_event_addr" => (Definition::AsEvent(Type::Addr), 0),
                        name if name.starts_with("as_event_data_") => (
                            Definition::AsEvent(Type::Data(
                                name[14..].parse().map_err(|_| "invalid data adapter")?,
                            )),
                            0,
                        ),
                        _ => {
                            self.constructors.push((s.id, id, behavior));
                            (Definition::Code(0), 0)
                        }
                    }
                }
            };
            let n = s.cells.len();
            let position = self.position([(n % 3) as f32 * 3.5 - 3.5, (n / 3) as f32 * 3.5, 0.])?;
            let inner = if self.eat(":") {
                self.expect("\n")?;
                self.expect("<indent>")?;
                let inner = self.space()?;
                self.expect("<dedent>")?;
                Some(inner)
            } else {
                self.expect("\n")?;
                None
            };
            s.cells.push(Instance {
                id,
                name,
                definition,
                parameter,
                position,
                space: inner,
            });
        }
        self.expect("<dedent>")?;
        self.depth -= 1;
        Ok(s)
    }
}
fn seed_integer(text: &str) -> Option<i128> {
    if let Some(n) = text.strip_prefix("0x") {
        i128::from_str_radix(n, 16).ok()
    } else if let Some(n) = text.strip_prefix("0b") {
        i128::from_str_radix(n, 2).ok()
    } else {
        text.parse().ok()
    }
}
fn natural_value(text: &str, seed: bool) -> Result<Id> {
    if seed {
        text.parse().map_err(|_| "expected positive integer".into())
    } else {
        crate::bootstrap_literals::natural(text)
    }
}
fn seed_binary(s: &str) -> Option<(u8, Binary)> {
    Some(match s {
        "or" => (1, Binary::Or),
        "and" => (2, Binary::And),
        "==" => (3, Binary::Eq),
        "!=" => (3, Binary::Ne),
        "<" => (3, Binary::Lt),
        "<=" => (3, Binary::Le),
        ">" => (3, Binary::Gt),
        ">=" => (3, Binary::Ge),
        "|" => (4, Binary::BitOr),
        "^" => (5, Binary::BitXor),
        "&" => (6, Binary::BitAnd),
        "<<" => (7, Binary::Shl),
        ">>" => (7, Binary::Shr),
        "+" => (8, Binary::Add),
        "-" => (8, Binary::Sub),
        "*" => (9, Binary::Mul),
        "/" => (9, Binary::Div),
        "%" => (9, Binary::Rem),
        _ => return None,
    })
}
fn seed_unary(s: &str) -> Option<(u8, Unary)> {
    Some((
        10,
        match s {
            "-" => Unary::Neg,
            "+" => Unary::Plus,
            "~" => Unary::BitNot,
            "not" => Unary::Not,
            _ => return None,
        },
    ))
}
fn seed_punctuation(text: &str) -> usize {
    if [
        "<<", ">>", "->", "..", "+=", "-=", "*=", "/=", "%=", "==", "!=", "<=", ">=",
    ]
    .iter()
    .any(|op| text.starts_with(op))
    {
        2
    } else if text
        .as_bytes()
        .first()
        .is_some_and(|b| b"()[]@=.,:+-*/%<>?&|^~".contains(b))
    {
        1
    } else {
        0
    }
}
fn seed_assignment(s: &str) -> Option<Assign> {
    Some(match s {
        "=" => Assign::Set,
        "+=" => Assign::Add,
        "-=" => Assign::Sub,
        "*=" => Assign::Mul,
        "/=" => Assign::Div,
        "%=" => Assign::Rem,
        _ => return None,
    })
}
pub fn parse(source: &str) -> Result<Program> {
    parse_impl(source, true, false)
}
pub(crate) fn parse_template(source: &str) -> Result<Program> {
    parse_impl(source, false, false)
}
pub(crate) fn parse_seed(source: &str) -> Result<Program> {
    parse_impl(source, true, true)
}
fn parse_impl(source: &str, validate_code: bool, seed_frontend: bool) -> Result<Program> {
    parse_with_sites(source, validate_code, seed_frontend).map(|(program, _, _)| program)
}
pub(crate) type ParsedSource = (Program, Vec<Token>, Vec<(usize, Id)>);
fn parse_with_sites(source: &str, validate_code: bool, seed_frontend: bool) -> Result<ParsedSource> {
    if source.len() > 4 * 1024 * 1024 { return Err("source exceeds 4 MiB".into()); }
    crate::compilation_cache::source(source,validate_code,seed_frontend,||parse_uncached_with_sites(source,validate_code,seed_frontend))
}
fn parse_uncached_with_sites(source: &str, validate_code: bool, seed_frontend: bool) -> Result<ParsedSource> {
    if source.len() > 4 * 1024 * 1024 {
        return Err("source exceeds 4 MiB".into());
    }
    let tokens = lex(source, seed_frontend)?;
    let reserved = tokens
        .windows(2)
        .filter(|w| w[0].text == "@")
        .filter_map(|w| natural_value(&w[1].text, seed_frontend).ok())
        .collect();
    let mut p = Parser {
        source,
        seed_frontend,
        tokens,
        at: 0,
        used: BTreeSet::new(),
        reserved,
        next: 1,
        pending: Vec::new(),
        constructors: Vec::new(),
        adapters: BTreeSet::new(),
        depth: 0,
        type_names: BTreeMap::new(),
        hyphenated_names: BTreeSet::new(),
        implicit_ids: Vec::new(),
    };
    let floor = if p.eat("identities") {
        let n = p.natural()?;
        if n == 0 {
            return Err(p.error("identity counter must be positive"));
        }
        p.expect("\n")?;
        n
    } else {
        1
    };
    p.next = floor;
    // Register data names/identities before signatures, including forward uses.
    let start = p.at;
    for index in start..p.tokens.len().saturating_sub(2) {
        if !matches!(p.tokens[index].text.as_str(), "struct" | "type") {
            continue;
        }
        p.at = index + 1;
        let Ok(name) = p.name() else {
            continue;
        };
        if !matches!(p.peek(), ":" | "=" | "@") {
            continue;
        }
        if p.type_names.contains_key(&name) {
            return Err(format!("{name}: duplicate data type"));
        }
        let id = p.id()?;
        p.type_names.insert(name, id);
    }
    p.at = start;
    let mut behaviors = Vec::new();
    let mut global_types = Vec::new();
    let mut root = None;
    let mut expansions = Vec::new();
    while p.peek() != "<eof>" {
        if p.peek() == "expand" && p.look(1) != "(" {
            if !validate_code {
                return Err(p.error("expand directives are only allowed in complete program source, not behavior edits"));
            }
            p.take()?;
            let name = p.name()?;
            p.expect("(")?;
            let mut args = Vec::new();
            if !p.eat(")") {
                loop {
                    args.push(p.signed()?);
                    if p.eat(")") {
                        break;
                    }
                    p.expect(",")?;
                }
            }
            p.expect("\n")?;
            expansions.push(crate::metaprogram::Invocation { name, args });
        } else if matches!(p.peek(), "struct" | "type") {
            global_types.push(p.data_definition()?);
        } else if p.peek() == "space" {
            if root.is_some() {
                return Err(p.error("one root space is supported"));
            }
            root = Some(p.space()?);
        } else {
            behaviors.push(p.behavior()?);
        }
    }

    let root = if let Some(root) = root {
        root
    } else {
        if behaviors.is_empty() {
            return Err(p.error("expected a space or behavior definition"));
        }
        let mut s = Space::new(p.id()?, "main".into());
        if let Some(main) = behaviors
            .iter()
            .find(|f| f.name == "main" && !f.compile_time)
            .or_else(|| {
                behaviors
                    .iter()
                    .find(|f| f.name == "kernel" && !f.compile_time)
            })
        {
            if !main.parameters.is_empty() {
                return Err("main: entry behavior must have no inputs".into());
            }
            s.cells.push(Instance {
                id: p.id()?,
                name: if main.name == "kernel" {
                    "kernel".into()
                } else {
                    "result".into()
                },
                definition: Definition::Code(main.id),
                parameter: 0,
                position: [0.; 3],
                space: None,
            });
        }
        s
    };
    let mut program = Program {
        global_types,
        root,
        code_modules: behaviors,
        next_identity: floor.max(
            p.used
                .last()
                .copied()
                .unwrap_or(0)
                .checked_add(1)
                .ok_or("identity exhausted")?,
        ),
    };
    for (space, id, name) in p.constructors {
        let adaptive = name.starts_with('@');
        let name = name.strip_prefix('@').unwrap_or(&name);
        let behavior = mira_core::code::resolve(&program.code_modules, 0, name)
            .ok_or_else(|| format!("{name}: unknown cell behavior"))?
            .id;
        program
            .root
            .find_mut(space)
            .ok_or("space missing")?
            .cells
            .iter_mut()
            .find(|c| c.id == id)
            .ok_or("cell missing")?
            .definition = if adaptive {
            Definition::Adaptive(behavior)
        } else {
            Definition::Code(behavior)
        };
    }
    let needs_inference = crate::inference::needed(&program);
    if needs_inference {
        crate::inference::analyze(&mut program, &[], false)
            .map_err(|error| contextualize(source, &error, seed_frontend))?;
    } else if validate_code {
        crate::composition::infer(&mut program)
            .map_err(|error| contextualize(source, &error, seed_frontend))?;
    }
    for raw in p.pending {
        let s = program.root.find(raw.space).ok_or("space missing")?;
        let candidates =
            |name: &str, explicit: Option<&str>, direction: Direction| -> Result<Vec<Endpoint>> {
                let boundary = if direction == Direction::Out {
                    &s.inputs
                } else {
                    &s.outputs
                };
                if boundary.iter().any(|p| p.name == name) {
                    if explicit.is_some() {
                        return Err(format!("{}/{}: boundary port already named", s.name, name));
                    }
                    return Ok(vec![Endpoint {
                        cell: s.id,
                        port: name.into(),
                    }]);
                }
                let c = s.cells.iter().find(|c| c.name == name).ok_or_else(|| {
                    format!(
                        "{}/{}: unknown cell; use a declared boundary for cross-space links",
                        s.name, name
                    )
                })?;
                let available: Vec<_> = program
                    .ports(c)
                    .into_iter()
                    .filter(|p| p.direction == direction && explicit.is_none_or(|n| n == p.name))
                    .map(|p| Endpoint {
                        cell: c.id,
                        port: p.name.into(),
                    })
                    .collect();
                if available.is_empty() {
                    return Err(format!(
                        "{}/{}.{}: unknown or wrong-direction port",
                        s.name,
                        name,
                        explicit.unwrap_or("?")
                    ));
                }
                Ok(available)
            };
        let source = candidates(&raw.from, raw.from_port.as_deref(), Direction::Out)?;
        let target = candidates(&raw.to, raw.to_port.as_deref(), Direction::In)?;
        let ty = |e: &Endpoint, direction: Direction| -> Result<Type> {
            if e.cell == s.id {
                let boundary = if direction == Direction::Out {
                    &s.inputs
                } else {
                    &s.outputs
                };
                Ok(boundary
                    .iter()
                    .find(|p| p.name == e.port)
                    .ok_or("boundary absent")?
                    .ty)
            } else {
                Ok(program
                    .ports(s.cell(e.cell)?)
                    .into_iter()
                    .find(|p| p.name == e.port)
                    .ok_or("port absent")?
                    .ty)
            }
        };
        let mut pairs = Vec::new();
        for from in &source {
            for to in &target {
                // An unannotated adapter takes its input type from the resolved
                // producer. It must not bias port selection towards i64.
                if p.adapters.contains(&to.cell)
                    || ty(from, Direction::Out)? == Type::Infer
                    || ty(to, Direction::In)? == Type::Infer
                    || ty(from, Direction::Out)? == ty(to, Direction::In)?
                {
                    pairs.push((from.clone(), to.clone()));
                }
            }
        }
        if pairs.len() != 1 {
            return Err(format!(
                "{}/{} -> {}: {}",
                s.name,
                raw.from,
                raw.to,
                if pairs.is_empty() {
                    "no compatible typed connection"
                } else {
                    "ambiguous connection; name the ports"
                }
            ));
        }
        let (from, to) = pairs.pop().unwrap();
        program
            .root
            .find_mut(raw.space)
            .ok_or("space missing")?
            .links
            .push(Link {
                id: raw.id,
                from,
                to,
            });
    }
    fn infer(
        s: &mut Space,
        adapters: &BTreeSet<Id>,
        definitions: &[CodeModule],
        global_types: &[data::Definition],
    ) -> Result<()> {
        for id in s.order()? {
            if !adapters.contains(&id) {
                continue;
            }
            if let Some(link) = s.links.iter().find(|l| l.to.cell == id) {
                let ty = if link.from.cell == s.id {
                    s.inputs
                        .iter()
                        .find(|p| p.name == link.from.port)
                        .ok_or("unknown input interface")?
                        .ty
                } else {
                    let from = s.cell(link.from.cell)?;
                    ports_for(definitions, global_types, from)
                        .into_iter()
                        .find(|p| p.name == link.from.port)
                        .ok_or_else(|| format!("{}.{}: unknown port", from.name, link.from.port))?
                        .ty
                };
                s.cells
                    .iter_mut()
                    .find(|c| c.id == id)
                    .ok_or("cell missing")?
                    .definition = Definition::AsEvent(ty);
            }
        }
        for cell in &mut s.cells {
            if let Some(inner) = &mut cell.space {
                infer(inner, adapters, definitions, global_types)?;
            }
        }

        Ok(())
    }
    if needs_inference {
        crate::inference::analyze(&mut program, &[], validate_code && expansions.is_empty())
            .map_err(|error| contextualize(source, &error, seed_frontend))?;
        if validate_code && expansions.is_empty() {
            crate::composition::infer(&mut program)
                .map_err(|error| contextualize(source, &error, seed_frontend))?;
        }
    }
    infer(
        &mut program.root,
        &p.adapters,
        &program.code_modules,
        &program.global_types,
    )?;
    program.validate()?;
    if validate_code && !expansions.is_empty() {
        crate::metaprogram::expand(&mut program, &expansions)
            .map_err(|error| contextualize(source, &error, seed_frontend))?;
    }
    if validate_code { program.require_complete()?; }
    Ok((program, p.tokens, p.implicit_ids))
}

/// Give source declarations their existing parser identities without formatting
/// their bodies. This is a storage view, not a second identity allocator.
pub(crate) fn pin_source(source: &str) -> Result<String> {
    // Already materialized, pinned containers are the common read path.
    if !source
        .lines()
        .any(|line| line.starts_with("expand "))
    {
        let (_, tokens, implicit) = parse_with_sites(source, true, false)?;
        if implicit.is_empty()
            && tokens
                .first()
                .is_some_and(|token| token.text == "identities")
        {
            return Ok(source.into());
        }
    }
    let original = parse(source)?;
    let mut expanded = source.to_string();
    let (program,tokens,implicit)=parse_with_sites(&expanded,true,false)?;
    if tokens
        .windows(2)
        .any(|pair| pair[0].column == 1 && pair[0].text == "expand" && pair[1].text != "(")
    {
        return Err(
            "expanded templates cannot be pinned without explicit source declarations".into(),
        );
    }
    let offsets = source_line_offsets(&expanded);
    let lines: Vec<_> = expanded.lines().collect();
    let mut insertions = BTreeMap::new();
    for (index, id) in implicit {
        let token = &tokens[index];
        if matches!(token.text.as_str(), "<eof>" | "<indent>" | "<dedent>") {
            return Err("synthetic entry identities need an explicit root space before source can be pinned".into());
        }
        let offset = if token.text == "\n" {
            // A wire identity belongs before its trailing comment, not after it.
            let previous = index
                .checked_sub(1)
                .and_then(|at| tokens.get(at))
                .filter(|previous| previous.line == token.line)
                .ok_or("implicit identity has no source declaration")?;
            let line = lines[token.line - 1];
            let mut scanner = crate::bootstrap_lexer::Scanner::new(line)?;
            offsets[token.line - 1] + scanner.scan(previous.column - 1)?.end
        } else {
            offsets[token.line - 1] + token.column - 1
        };
        if let Some(previous) = insertions.insert(offset, id) {
            if previous != id {
                return Err("multiple implicit identities share one source insertion site".into());
            }
        }
    }
    for (offset, id) in insertions.into_iter().rev() {
        expanded.insert_str(offset, &format!(" @{id} "));
    }
    if tokens
        .first()
        .is_none_or(|token| token.text != "identities")
    {
        expanded.insert_str(0, &format!("identities {}\n", program.next_identity));
    }
    if parse(&expanded)? != original {
        return Err("pinning source identities changed the program".into());
    }
    Ok(expanded)
}

fn source_line_offsets(source: &str) -> Vec<usize> {
    std::iter::once(0)
        .chain(source.match_indices('\n').map(|(index, _)| index + 1))
        .collect()
}

/// Byte ranges of explicitly identified declaration lines and their indented
/// bodies. Layout tokens and comments cannot create declaration boundaries.
pub(crate) fn source_spans(source: &str) -> Result<BTreeMap<Id, (usize, usize)>> {
    source_layout(source).map(|(_, spans)| spans)
}

pub(crate) fn source_layout(source: &str) -> Result<(Program, BTreeMap<Id, (usize, usize)>)> {
    let (program, tokens, implicit) = parse_with_sites(source, true, false)?;
    if !implicit.is_empty() {
        return Err("source spans require explicitly pinned identities".into());
    }
    if tokens.windows(2).any(|pair| {
        pair[0].column == 1
            && matches!(pair[0].text.as_str(), "use" | "expand")
            && pair[1].text != "("
    }) {
        return Err("source spans require materialized source declarations".into());
    }
    let offsets = source_line_offsets(source);
    let mut lines = Vec::new();
    let mut logical_line = Vec::new();
    for token in &tokens {
        if token.text == "\n" {
            if !logical_line.is_empty() {
                lines.push(std::mem::take(&mut logical_line));
            }
        } else if !matches!(token.text.as_str(), "<indent>" | "<dedent>" | "<eof>") {
            logical_line.push(token);
        }
    }
    let mut spans = BTreeMap::new();
    let mut open: Vec<(usize, Id)> = Vec::new();
    for header in &lines {
        let line = header[0].line;
        let indent = header[0].column;
        while open.last().is_some_and(|(column, _)| *column >= indent) {
            let (_, id) = open.pop().unwrap();
            spans
                .get_mut(&id)
                .map(|range: &mut (usize, usize)| range.1 = offsets[line - 1]);
        }
        // Graph wires can carry several IDs on one line; they are not source
        // declarations and cannot have independent, nonoverlapping line ranges.
        if header.iter().any(|token| token.text == "->")
            && !header.iter().any(|token| token.text == "=")
        {
            continue;
        }
        for pair in header.windows(2).filter(|pair| pair[0].text == "@") {
            let id = natural_value(&pair[1].text, false)?;
            if spans
                .insert(id, (offsets[line - 1], source.len()))
                .is_some()
            {
                return Err("duplicate source identity span".into());
            }
            open.push((indent, id));
        }
    }
    Ok((program, spans))
}

fn is_float(token: &str) -> bool {
    !token.starts_with("0x")
        && !token.starts_with("0b")
        && (token.contains('.') || token.contains('e') || token.contains('E'))
}

/// Attach the source declaration location when semantic diagnostics know its
/// original name. This is a declaration span, not a fabricated expression span.
fn contextualize(source: &str, error: &str, seed_frontend: bool) -> String {
    let mut head = error.split(':');
    if head.next().is_some_and(|s| s.parse::<usize>().is_ok())
        && head.next().is_some_and(|s| s.parse::<usize>().is_ok())
    {
        return error.into();
    }
    if let Ok(tokens) = lex(source, seed_frontend) {
        let first = error.split(':').next().unwrap_or("").trim();
        if let Some(token) = tokens.iter().find(|t| t.text == first) {
            return format!(
                "{}:{}: {error} (at declaration `{first}`)",
                token.line, token.column
            );
        }
    }
    error.into()
}

#[cfg(test)]
mod source_identity_spans {
    use super::*;

    #[test]
    fn pins_parser_identities_without_losing_comments_or_nested_z() {
        let source = "// keep this heading\r\nstruct Pair:\n  value: i64 // field\n`helper-name`()->i64 = 7 // helper\nspace Main:\n  component = graph() at (1, 2, 3):\n    space Body at (4, 5, 6):\n      out result: i64\n      value = `helper-name`()\n      value.out -> result // wire comment\n  sibling = 9 // sibling\n";
        let original = parse(source).unwrap();
        let pinned = pin_source(source).unwrap();
        assert_eq!(parse(&pinned).unwrap(), original);
        assert_eq!(pin_source(&pinned).unwrap(), pinned);
        for comment in [
            "// keep this heading\r\n",
            "// field",
            "// helper",
            "// wire comment",
            "// sibling",
        ] {
            assert!(pinned.contains(comment), "{comment}");
        }
        assert!(pinned.contains("at (1, 2, 3)"));
        assert!(pinned.contains("at (4, 5, 6)"));
        let spans = source_spans(&pinned).unwrap();
        let component = &original.root.cells[0];
        let sibling = &original.root.cells[1];
        let body = component.space.as_ref().unwrap();
        let component_span = spans[&component.id];
        let body_span = spans[&body.id];
        assert!(component_span.0 < body_span.0 && body_span.1 <= component_span.1);
        assert_eq!(component_span.1, spans[&sibling.id].0);
        assert!(pinned[component_span.0..component_span.1].contains("// wire comment"));
        assert!(
            pinned[spans[&original.code_modules[0].id].0..spans[&original.code_modules[0].id].1]
                .contains("`helper-name`")
        );
        assert!(!spans.contains_key(&body.links[0].id));
    }

    #[test]
    fn spans_multiline_headers_as_one_logical_declaration() {
        let source =
            "identities 100\nhelper @1(\nx:i64\n)->i64 = x\nspace Main @2:\n  result @3 = 7\n";
        let spans = source_spans(source).unwrap();
        assert!(source[spans[&1].0..spans[&1].1].contains(")->i64 = x"));
        assert_eq!(spans[&1].1, spans[&2].0);
        assert_eq!(pin_source(source).unwrap(), source);
    }

    #[test]
    fn rejects_unsafe_synthetic_entries_and_malformed_sources() {
        assert!(
            pin_source("main()->i64 = 1\n")
                .unwrap_err()
                .contains("synthetic")
        );
        for source in [
            "space Broken:\n  value = (\n",
            "use compiler.missing\n",
            "space Main @1:\n  value @1 = 7\n",
        ] {
            assert!(pin_source(source).is_err());
            assert!(source_spans(source).is_err());
        }
        let source = "macro unchanged()->unit = ()\nspace Main:\n  value = 7\nexpand unchanged()\n";
        assert!(pin_source(source).unwrap_err().contains("templates"));
    }
}

#[cfg(test)]
mod lexical_seed_contract {
    use super::*;

    #[test]
    fn source_scanner_matches_seed_at_window_boundaries() {
        let mut sources = vec![crate::compiler_source().unwrap().to_owned()];
        for tail in [
            "-> next",
            "0xFF+1",
            "12.5e-2",
            "1..3",
            "\"é\\\"//\"",
            "`long-name`",
        ] {
            sources.push(format!("value{}{tail}\n", " ".repeat(4090)));
        }
        sources.push(format!("value{}12.5e-2\n", " ".repeat(4088)));
        sources.push(format!("value \"{}é\\\"\"\n", "a".repeat(4087)));
        sources.push("space Main:\n  value = (\n    1 + 2\n  )\n  // comment\n".into());
        sources.push("space Main:\n  value = `unterminated\n".into());
        sources.push("space Main:\n  value = \"unterminated\\\n".into());
        for source in sources {
            let signature = |result: Result<Vec<Token>>| {
                result.map(|tokens| {
                    tokens
                        .into_iter()
                        .map(|t| (t.text, t.line, t.column))
                        .collect::<Vec<_>>()
                })
            };
            assert_eq!(
                signature(lex(&source, false)),
                signature(lex(&source, true))
            );
        }
    }
}
