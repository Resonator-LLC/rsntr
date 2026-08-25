//! Hand-rolled recursive-descent parser for the supported SPARQL subset.

use crate::ast::*;
use crate::term::{K_IRI, K_LIT, RDF_TYPE, Term, XSD};

pub struct ParseError(pub String);

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::fmt::Debug for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

type PResult<T> = Result<T, ParseError>;

struct Cur<'a> {
    s: &'a [u8],
    pos: usize,
    text: &'a str,
}

impl<'a> Cur<'a> {
    fn new(text: &'a str) -> Self {
        Cur {
            s: text.as_bytes(),
            pos: 0,
            text,
        }
    }

    fn err<T>(&self, msg: &str) -> PResult<T> {
        let mut line = 1;
        let mut col = 1;
        for &b in &self.s[..self.pos.min(self.s.len())] {
            if b == b'\n' {
                line += 1;
                col = 1;
            } else {
                col += 1;
            }
        }
        Err(ParseError(format!("{msg} (line {line}, col {col})")))
    }

    fn peek(&self) -> u8 {
        *self.s.get(self.pos).unwrap_or(&0)
    }

    fn peek_at(&self, off: usize) -> u8 {
        *self.s.get(self.pos + off).unwrap_or(&0)
    }

    fn bump(&mut self) {
        self.pos += 1;
    }

    fn eof(&self) -> bool {
        self.pos >= self.s.len()
    }

    fn skipws(&mut self) {
        loop {
            match self.peek() {
                b' ' | b'\t' | b'\r' | b'\n' => self.bump(),
                b'#' => {
                    while !self.eof() && self.peek() != b'\n' {
                        self.bump();
                    }
                }
                _ => return,
            }
        }
    }

    /// Case-insensitive keyword match with a word boundary; consumes on match.
    fn kw(&mut self, word: &str) -> bool {
        let w = word.as_bytes();
        if self.pos + w.len() > self.s.len() {
            return false;
        }
        for (i, &b) in w.iter().enumerate() {
            if !self.s[self.pos + i].eq_ignore_ascii_case(&b) {
                return false;
            }
        }
        let next = *self.s.get(self.pos + w.len()).unwrap_or(&0);
        if next.is_ascii_alphanumeric() || next == b'_' {
            return false;
        }
        self.pos += w.len();
        true
    }

    fn take_while(&mut self, f: impl Fn(u8) -> bool) -> &'a str {
        let start = self.pos;
        while !self.eof() && f(self.peek()) {
            self.bump();
        }
        &self.text[start..self.pos]
    }
}

fn is_name_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Resolve `iri` against `base`. Absolute IRIs pass through. Dot-segment
/// normalization is skipped (documented simplification).
pub fn resolve_iri(base: Option<&str>, iri: &str) -> String {
    fn has_scheme(s: &str) -> bool {
        let b = s.as_bytes();
        if b.is_empty() || !b[0].is_ascii_alphabetic() {
            return false;
        }
        for (i, &c) in b.iter().enumerate() {
            if c == b':' {
                return i > 0;
            }
            if !(c.is_ascii_alphanumeric() || c == b'+' || c == b'-' || c == b'.') {
                return false;
            }
        }
        false
    }
    let Some(base) = base else {
        return iri.to_string();
    };
    if has_scheme(iri) || base.is_empty() {
        return iri.to_string();
    }
    if let Some(rest) = iri.strip_prefix("//") {
        let scheme_end = base.find(':').map_or(0, |i| i + 1);
        return format!("{}//{}", &base[..scheme_end], rest);
    }
    if iri.starts_with('/') {
        // scheme://authority + absolute path
        let after_scheme = base.find("://").map(|i| i + 3).unwrap_or(0);
        let auth_end = base[after_scheme..]
            .find('/')
            .map(|i| after_scheme + i)
            .unwrap_or(base.len());
        return format!("{}{}", &base[..auth_end], iri);
    }
    if iri.starts_with('#') {
        let frag = base.find('#').unwrap_or(base.len());
        return format!("{}{}", &base[..frag], iri);
    }
    match base.rfind('/') {
        Some(i) => format!("{}{}", &base[..=i], iri),
        None => iri.to_string(),
    }
}

fn read_iriref(c: &mut Cur) -> PResult<String> {
    // cursor at '<'
    c.bump();
    let mut out = String::new();
    loop {
        if c.eof() {
            return c.err("unterminated IRI");
        }
        let b = c.peek();
        if b == b'>' {
            c.bump();
            return Ok(out);
        }
        if b == b'\\' {
            c.bump();
            match c.peek() {
                b'u' | b'U' => {
                    let n = if c.peek() == b'u' { 4 } else { 8 };
                    c.bump();
                    let mut v: u32 = 0;
                    for _ in 0..n {
                        let h = c.peek() as char;
                        let Some(d) = h.to_digit(16) else {
                            return c.err("bad \\u escape in IRI");
                        };
                        v = v * 16 + d;
                        c.bump();
                    }
                    match char::from_u32(v) {
                        Some(ch) => out.push(ch),
                        None => return c.err("bad \\u escape in IRI"),
                    }
                }
                _ => return c.err("bad escape in IRI"),
            }
        } else {
            // copy the full UTF-8 sequence byte-wise; input is valid UTF-8
            out.push_str(&c.text[c.pos..c.pos + 1.max(utf8_len(b))]);
            for _ in 0..1.max(utf8_len(b)) {
                c.bump();
            }
        }
    }
}

fn utf8_len(b: u8) -> usize {
    if b < 0x80 {
        1
    } else if b >> 5 == 0b110 {
        2
    } else if b >> 4 == 0b1110 {
        3
    } else if b >> 3 == 0b11110 {
        4
    } else {
        1
    }
}

fn local_name_byte(c: &Cur, off: usize) -> bool {
    let b = c.peek_at(off);
    b.is_ascii_alphanumeric()
        || b == b'_'
        || b == b'-'
        || (b == b'.' && {
            let n = c.peek_at(off + 1);
            n.is_ascii_alphanumeric() || n == b'_' || n == b'-' || n == b'.'
        })
}

fn read_pname(c: &mut Cur, prefixes: &[(String, String)]) -> PResult<String> {
    let start = c.pos;
    while {
        let b = c.peek();
        b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b == b'.'
    } {
        c.bump();
    }
    let prefix = c.text[start..c.pos].to_string();
    if c.peek() != b':' {
        return c.err("expected ':' in prefixed name");
    }
    c.bump();
    let lstart = c.pos;
    while local_name_byte(c, 0) {
        c.bump();
    }
    let local = &c.text[lstart..c.pos];
    for (name, iri) in prefixes {
        if *name == prefix {
            return Ok(format!("{iri}{local}"));
        }
    }
    c.err(&format!("unknown prefix '{prefix}'"))
}

fn read_string(c: &mut Cur) -> PResult<String> {
    let quote = c.peek();
    let long = c.peek_at(1) == quote && c.peek_at(2) == quote;
    if long {
        c.bump();
        c.bump();
        c.bump();
    } else {
        c.bump();
    }
    let mut out = String::new();
    loop {
        if c.eof() {
            return c.err("unterminated string");
        }
        let b = c.peek();
        if b == quote {
            if long {
                if c.peek_at(1) == quote && c.peek_at(2) == quote {
                    c.bump();
                    c.bump();
                    c.bump();
                    return Ok(out);
                }
                out.push(b as char);
                c.bump();
                continue;
            }
            c.bump();
            return Ok(out);
        }
        if !long && (b == b'\n' || b == b'\r') {
            return c.err("newline in short string");
        }
        if b == b'\\' {
            c.bump();
            let e = c.peek();
            c.bump();
            match e {
                b't' => out.push('\t'),
                b'b' => out.push('\u{8}'),
                b'n' => out.push('\n'),
                b'r' => out.push('\r'),
                b'f' => out.push('\u{c}'),
                b'"' => out.push('"'),
                b'\'' => out.push('\''),
                b'\\' => out.push('\\'),
                b'u' | b'U' => {
                    let n = if e == b'u' { 4 } else { 8 };
                    let mut v: u32 = 0;
                    for _ in 0..n {
                        let Some(d) = (c.peek() as char).to_digit(16) else {
                            return c.err("bad \\u escape in string");
                        };
                        v = v * 16 + d;
                        c.bump();
                    }
                    match char::from_u32(v) {
                        Some(ch) => out.push(ch),
                        None => return c.err("bad \\u escape in string"),
                    }
                }
                _ => return c.err("unknown string escape"),
            }
            continue;
        }
        let n = 1.max(utf8_len(b));
        out.push_str(&c.text[c.pos..c.pos + n]);
        for _ in 0..n {
            c.bump();
        }
    }
}

fn read_number(c: &mut Cur) -> PResult<Term> {
    let start = c.pos;
    if c.peek() == b'+' || c.peek() == b'-' {
        c.bump();
    }
    while c.peek().is_ascii_digit() {
        c.bump();
    }
    let mut is_decimal = false;
    if c.peek() == b'.' && c.peek_at(1).is_ascii_digit() {
        is_decimal = true;
        c.bump();
        while c.peek().is_ascii_digit() {
            c.bump();
        }
    }
    let mut is_double = false;
    if c.peek() == b'e' || c.peek() == b'E' {
        let mut off = 1;
        if c.peek_at(1) == b'+' || c.peek_at(1) == b'-' {
            off = 2;
        }
        if c.peek_at(off).is_ascii_digit() {
            is_double = true;
            for _ in 0..off {
                c.bump();
            }
            while c.peek().is_ascii_digit() {
                c.bump();
            }
        }
    }
    let lex = &c.text[start..c.pos];
    let dtype = if is_double {
        format!("{XSD}double")
    } else if is_decimal {
        format!("{XSD}decimal")
    } else {
        format!("{XSD}integer")
    };
    Ok(Term::lit_dt(lex, dtype))
}

/// Parse a term shared between positions: IRI, prefixed name, literal,
/// number, boolean. Returns Ok(None) when the input does not start a term
/// (including "_:" bnode labels, which the caller handles).
fn parse_term_common(
    c: &mut Cur,
    prefixes: &[(String, String)],
    base: Option<&str>,
) -> PResult<Option<Term>> {
    c.skipws();
    let b = c.peek();
    if b == b'<' {
        let raw = read_iriref(c)?;
        return Ok(Some(Term::iri(resolve_iri(base, &raw))));
    }
    if b == b'"' || b == b'\'' {
        let lex = read_string(c)?;
        if c.peek() == b'@' {
            c.bump();
            let tag = c
                .take_while(|b| b.is_ascii_alphanumeric() || b == b'-')
                .to_string();
            if tag.is_empty() {
                return c.err("empty language tag");
            }
            return Ok(Some(Term::lit_lang(lex, tag)));
        }
        if c.peek() == b'^' && c.peek_at(1) == b'^' {
            c.bump();
            c.bump();
            c.skipws();
            let dt = if c.peek() == b'<' {
                resolve_iri(base, &read_iriref(c)?)
            } else {
                read_pname(c, prefixes)?
            };
            return Ok(Some(Term::lit_dt(lex, dt)));
        }
        return Ok(Some(Term::lit(lex)));
    }
    if b.is_ascii_digit()
        || ((b == b'+' || b == b'-')
            && (c.peek_at(1).is_ascii_digit()
                || (c.peek_at(1) == b'.' && c.peek_at(2).is_ascii_digit())))
        || (b == b'.' && c.peek_at(1).is_ascii_digit())
    {
        return read_number(c).map(Some);
    }
    if c.kw("true") {
        return Ok(Some(Term::lit_dt("true", format!("{XSD}boolean"))));
    }
    if c.kw("false") {
        return Ok(Some(Term::lit_dt("false", format!("{XSD}boolean"))));
    }
    if b.is_ascii_alphabetic() || b == b'_' || b == b':' {
        if b == b'_' && c.peek_at(1) == b':' {
            return Ok(None); // bnode label: caller's job
        }
        let iri = read_pname(c, prefixes)?;
        return Ok(Some(Term::iri(iri)));
    }
    Ok(None)
}

pub struct Parser<'a> {
    c: Cur<'a>,
    prefixes: Vec<(String, String)>,
    base: Option<String>,
}

impl<'a> Parser<'a> {
    fn varname(&mut self) -> PResult<String> {
        self.c.bump(); // '?' or '$'
        let name = self.c.take_while(is_name_byte).to_string();
        if name.is_empty() {
            return self.c.err("empty variable name");
        }
        Ok(name)
    }

    /// pos: 0 = subject, 1 = predicate, 2 = object
    fn qt(&mut self, pos: usize) -> PResult<Slot> {
        self.c.skipws();
        let b = self.c.peek();
        if b == b'?' || b == b'$' {
            return Ok(Slot::Var(self.varname()?));
        }
        if b == b'_' && self.c.peek_at(1) == b':' {
            // blank node label in a query acts as a (non-projectable) variable
            self.c.bump();
            self.c.bump();
            let label = self.c.take_while(is_name_byte).to_string();
            if label.is_empty() {
                return self.c.err("empty blank node label");
            }
            return Ok(Slot::Var(format!("~bn~{label}")));
        }
        if pos == 1 {
            if b == b'^' {
                return self
                    .c
                    .err("property paths are not supported in this subset");
            }
            if self.c.kw("a") {
                return Ok(Slot::Ground(Term::iri(RDF_TYPE)));
            }
        }
        let t = match parse_term_common(&mut self.c, &self.prefixes, self.base.as_deref())? {
            Some(t) => t,
            None => return self.c.err("expected a term"),
        };
        if pos == 0 && t.kind == K_LIT {
            return self.c.err("literal cannot be a subject");
        }
        if pos == 1 {
            if t.kind != K_IRI {
                return self.c.err("predicate must be an IRI or variable");
            }
            // path operator glued to the predicate IRI
            let nb = self.c.peek();
            if nb == b'+' || nb == b'*' || nb == b'/' || nb == b'^' {
                return self
                    .c
                    .err("property paths are not supported in this subset");
            }
        }
        Ok(Slot::Ground(t))
    }

    /// triples-same-subject with ; and , shorthand; pushes into `list`.
    fn triples_block(&mut self, list: &mut Vec<TriplePattern>) -> PResult<()> {
        let subj = self.qt(0)?;
        loop {
            let pred = self.qt(1)?;
            loop {
                let obj = self.qt(2)?;
                list.push(TriplePattern {
                    s: subj.clone(),
                    p: pred.clone(),
                    o: obj,
                });
                self.c.skipws();
                if self.c.peek() == b',' {
                    self.c.bump();
                    continue;
                }
                break;
            }
            self.c.skipws();
            if self.c.peek() == b';' {
                self.c.bump();
                self.c.skipws();
                while self.c.peek() == b';' {
                    self.c.bump();
                    self.c.skipws();
                }
                if matches!(self.c.peek(), b'.' | b'}' | 0) {
                    break;
                }
                continue;
            }
            break;
        }
        Ok(())
    }

    // ------------------------------------------------------------- filters --

    fn primary(&mut self) -> PResult<Expr> {
        self.c.skipws();
        let b = self.c.peek();
        if b == b'(' {
            self.c.bump();
            let e = self.or_expr()?;
            self.c.skipws();
            if self.c.peek() != b')' {
                return self.c.err("expected ')'");
            }
            self.c.bump();
            return Ok(e);
        }
        if b == b'!' && self.c.peek_at(1) != b'=' {
            self.c.bump();
            let inner = self.primary()?;
            return Ok(Expr::Not(Box::new(inner)));
        }
        if b == b'?' || b == b'$' {
            let var = self.varname()?;
            return Ok(Expr::Val(Val {
                func: Func::None,
                var: Some(var),
                term: None,
            }));
        }
        if self.c.kw("BOUND") {
            self.c.skipws();
            if self.c.peek() != b'(' {
                return self.c.err("expected '(' after BOUND");
            }
            self.c.bump();
            self.c.skipws();
            if self.c.peek() != b'?' && self.c.peek() != b'$' {
                return self.c.err("BOUND takes a variable");
            }
            let var = self.varname()?;
            self.c.skipws();
            if self.c.peek() != b')' {
                return self.c.err("expected ')' after BOUND variable");
            }
            self.c.bump();
            return Ok(Expr::Bound(var));
        }
        if self.c.kw("REGEX") {
            self.c.skipws();
            if self.c.peek() != b'(' {
                return self.c.err("expected '(' after REGEX");
            }
            self.c.bump();
            let text = self.unary()?;
            self.c.skipws();
            if self.c.peek() != b',' {
                return self.c.err("REGEX needs a pattern argument");
            }
            self.c.bump();
            let pattern = self.unary()?;
            self.c.skipws();
            let mut flags = None;
            if self.c.peek() == b',' {
                self.c.bump();
                flags = Some(Box::new(self.unary()?));
                self.c.skipws();
            }
            if self.c.peek() != b')' {
                return self.c.err("expected ')' after REGEX");
            }
            self.c.bump();
            return Ok(Expr::Regex {
                text: Box::new(text),
                pattern: Box::new(pattern),
                flags,
            });
        }
        let t = match parse_term_common(&mut self.c, &self.prefixes, self.base.as_deref())? {
            Some(t) => t,
            None => return self.c.err("expected expression"),
        };
        Ok(Expr::Val(Val {
            func: Func::None,
            var: None,
            term: Some(t),
        }))
    }

    fn unary(&mut self) -> PResult<Expr> {
        self.c.skipws();
        let func = if self.c.kw("STR") {
            Func::Str
        } else if self.c.kw("LANG") {
            Func::Lang
        } else if self.c.kw("DATATYPE") {
            Func::Datatype
        } else {
            Func::None
        };
        if func != Func::None {
            self.c.skipws();
            if self.c.peek() != b'(' {
                return self.c.err("expected '(' after builtin");
            }
            self.c.bump();
            self.c.skipws();
            if self.c.peek() != b'?' && self.c.peek() != b'$' {
                return self.c.err("str/lang/datatype take a variable");
            }
            let var = self.varname()?;
            self.c.skipws();
            if self.c.peek() != b')' {
                return self.c.err("expected ')' after builtin argument");
            }
            self.c.bump();
            return Ok(Expr::Val(Val {
                func,
                var: Some(var),
                term: None,
            }));
        }
        self.primary()
    }

    fn rel(&mut self) -> PResult<Expr> {
        let a = self.unary()?;
        self.c.skipws();
        let op = if self.c.peek() == b'!' && self.c.peek_at(1) == b'=' {
            self.c.bump();
            self.c.bump();
            Some(CmpOp::Ne)
        } else if self.c.peek() == b'<' && self.c.peek_at(1) == b'=' {
            self.c.bump();
            self.c.bump();
            Some(CmpOp::Le)
        } else if self.c.peek() == b'>' && self.c.peek_at(1) == b'=' {
            self.c.bump();
            self.c.bump();
            Some(CmpOp::Ge)
        } else if self.c.peek() == b'=' {
            self.c.bump();
            Some(CmpOp::Eq)
        } else if self.c.peek() == b'<' {
            self.c.bump();
            Some(CmpOp::Lt)
        } else if self.c.peek() == b'>' {
            self.c.bump();
            Some(CmpOp::Gt)
        } else {
            None
        };
        let Some(op) = op else { return Ok(a) };
        let b = self.unary()?;
        if !matches!(a, Expr::Val(_)) || !matches!(b, Expr::Val(_)) {
            return self.c.err("unsupported comparison operand");
        }
        Ok(Expr::Cmp {
            op,
            a: Box::new(a),
            b: Box::new(b),
        })
    }

    fn and_expr(&mut self) -> PResult<Expr> {
        let mut a = self.rel()?;
        loop {
            self.c.skipws();
            if self.c.peek() == b'&' && self.c.peek_at(1) == b'&' {
                self.c.bump();
                self.c.bump();
                let b = self.rel()?;
                a = Expr::And(Box::new(a), Box::new(b));
            } else {
                return Ok(a);
            }
        }
    }

    fn or_expr(&mut self) -> PResult<Expr> {
        let mut a = self.and_expr()?;
        loop {
            self.c.skipws();
            if self.c.peek() == b'|' && self.c.peek_at(1) == b'|' {
                self.c.bump();
                self.c.bump();
                let b = self.and_expr()?;
                a = Expr::Or(Box::new(a), Box::new(b));
            } else {
                return Ok(a);
            }
        }
    }

    // --------------------------------------------------------------- group --

    fn group(&mut self) -> PResult<Group> {
        self.c.skipws();
        if self.c.peek() != b'{' {
            return self.c.err("expected '{'");
        }
        self.c.bump();
        let mut g = Group::default();
        loop {
            self.c.skipws();
            if self.c.eof() {
                return self.c.err("unterminated group pattern");
            }
            if self.c.peek() == b'}' {
                self.c.bump();
                return Ok(g);
            }
            if self.c.peek() == b'.' {
                self.c.bump();
                continue;
            }
            if self.c.kw("OPTIONAL") {
                let ng = self.group()?;
                g.optionals.push(ng);
                continue;
            }
            if self.c.kw("FILTER") {
                let e = self.or_expr()?;
                g.filters.push(e);
                continue;
            }
            if self.c.peek() == b'{' {
                // GroupOrUnionGraphPattern: { A } UNION { B } UNION ...
                let mut branches = Vec::new();
                loop {
                    {
                        // reject subqueries with a clear error
                        let save = self.c.pos;
                        self.c.bump();
                        self.c.skipws();
                        if self.c.kw("SELECT") {
                            return self.c.err("subqueries are not supported in this subset");
                        }
                        self.c.pos = save;
                    }
                    let br = self.group()?;
                    branches.push(br);
                    self.c.skipws();
                    if self.c.kw("UNION") {
                        self.c.skipws();
                        if self.c.peek() != b'{' {
                            return self.c.err("expected '{' after UNION");
                        }
                        continue;
                    }
                    break;
                }
                g.unions.push(branches);
                continue;
            }
            if self.c.kw("GRAPH")
                || self.c.kw("MINUS")
                || self.c.kw("BIND")
                || self.c.kw("VALUES")
                || self.c.kw("SERVICE")
            {
                return self.c.err(
                    "unsupported SPARQL feature in this subset (GRAPH/MINUS/BIND/VALUES/SERVICE)",
                );
            }
            self.triples_block(&mut g.triples)?;
        }
    }

    fn prologue(&mut self) -> PResult<()> {
        loop {
            self.c.skipws();
            if self.c.kw("PREFIX") {
                self.c.skipws();
                let name = self
                    .c
                    .take_while(|b| {
                        b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b == b'.'
                    })
                    .to_string();
                if self.c.peek() != b':' {
                    return self.c.err("expected ':' in PREFIX");
                }
                self.c.bump();
                self.c.skipws();
                if self.c.peek() != b'<' {
                    return self.c.err("expected IRI in PREFIX");
                }
                let iri = read_iriref(&mut self.c)?;
                let resolved = resolve_iri(self.base.as_deref(), &iri);
                self.prefixes.retain(|(n, _)| *n != name);
                self.prefixes.push((name, resolved));
                continue;
            }
            if self.c.kw("BASE") {
                self.c.skipws();
                if self.c.peek() != b'<' {
                    return self.c.err("expected IRI in BASE");
                }
                let iri = read_iriref(&mut self.c)?;
                self.base = Some(resolve_iri(self.base.as_deref(), &iri));
                continue;
            }
            return Ok(());
        }
    }

    fn int(&mut self) -> PResult<i64> {
        self.c.skipws();
        let digits = self.c.take_while(|b| b.is_ascii_digit());
        if digits.is_empty() {
            return self.c.err("expected an integer");
        }
        Ok(digits.parse().unwrap_or(0))
    }

    /// Cursor at '(': parse "(AGG([DISTINCT] ?v|*) AS ?alias)".
    fn agg_item(&mut self) -> PResult<SelItem> {
        self.c.bump(); // '('
        self.c.skipws();
        let agg = if self.c.kw("COUNT") {
            Agg::Count
        } else if self.c.kw("SUM") {
            Agg::Sum
        } else if self.c.kw("MIN") {
            Agg::Min
        } else if self.c.kw("MAX") {
            Agg::Max
        } else if self.c.kw("AVG") {
            Agg::Avg
        } else {
            return self
                .c
                .err("expected an aggregate (COUNT/SUM/MIN/MAX/AVG) after '('");
        };
        self.c.skipws();
        if self.c.peek() != b'(' {
            return self.c.err("expected '(' after aggregate name");
        }
        self.c.bump();
        self.c.skipws();
        let mut distinct = false;
        if self.c.kw("DISTINCT") {
            distinct = true;
            self.c.skipws();
        }
        let mut star = false;
        let mut var = None;
        if self.c.peek() == b'*' {
            if agg != Agg::Count {
                return self.c.err("'*' is only valid in COUNT(*)");
            }
            star = true;
            self.c.bump();
        } else if self.c.peek() == b'?' || self.c.peek() == b'$' {
            var = Some(self.varname()?);
        } else {
            return self
                .c
                .err("expected a variable or '*' inside the aggregate");
        }
        self.c.skipws();
        if self.c.peek() != b')' {
            return self.c.err("expected ')'");
        }
        self.c.bump();
        self.c.skipws();
        if !self.c.kw("AS") {
            return self.c.err("aggregate needs an alias: (COUNT(?x) AS ?n)");
        }
        self.c.skipws();
        if self.c.peek() != b'?' && self.c.peek() != b'$' {
            return self.c.err("expected ?alias after AS");
        }
        let alias = self.varname()?;
        self.c.skipws();
        if self.c.peek() != b')' {
            return self.c.err("expected ')' to close the aggregate item");
        }
        self.c.bump();
        Ok(SelItem {
            agg,
            distinct,
            star,
            var,
            alias,
        })
    }

    fn modifiers(&mut self, q: &mut Query) -> PResult<()> {
        loop {
            self.c.skipws();
            if self.c.kw("GROUP") {
                self.c.skipws();
                if !self.c.kw("BY") {
                    return self.c.err("expected BY after GROUP");
                }
                loop {
                    self.c.skipws();
                    if self.c.peek() != b'?' && self.c.peek() != b'$' {
                        break;
                    }
                    let v = self.varname()?;
                    q.group_by.push(v);
                }
                if q.group_by.is_empty() {
                    return self.c.err("GROUP BY needs at least one variable");
                }
                continue;
            }
            if self.c.kw("HAVING") {
                return self.c.err(
                    "HAVING is not supported yet (filter in a wrapping SQL query over sparql() instead)",
                );
            }
            if self.c.kw("ORDER") {
                self.c.skipws();
                if !self.c.kw("BY") {
                    return self.c.err("expected BY after ORDER");
                }
                loop {
                    self.c.skipws();
                    let mut desc = false;
                    let keyed = if self.c.kw("ASC") {
                        true
                    } else if self.c.kw("DESC") {
                        desc = true;
                        true
                    } else {
                        false
                    };
                    let v;
                    if keyed {
                        self.c.skipws();
                        if self.c.peek() != b'(' {
                            return self.c.err("expected '(' after ASC/DESC");
                        }
                        self.c.bump();
                        self.c.skipws();
                        if self.c.peek() != b'?' && self.c.peek() != b'$' {
                            return self.c.err("expected variable in ORDER BY");
                        }
                        v = self.varname()?;
                        self.c.skipws();
                        if self.c.peek() != b')' {
                            return self.c.err("expected ')'");
                        }
                        self.c.bump();
                    } else if self.c.peek() == b'?' || self.c.peek() == b'$' {
                        v = self.varname()?;
                    } else {
                        break;
                    }
                    q.order.push(OrdKey { var: v, desc });
                }
                if q.order.is_empty() {
                    return self.c.err("ORDER BY needs at least one variable");
                }
                continue;
            }
            if self.c.kw("LIMIT") {
                q.limit = self.int()?;
                continue;
            }
            if self.c.kw("OFFSET") {
                q.offset = self.int()?;
                continue;
            }
            return Ok(());
        }
    }

    fn data_block(&mut self) -> PResult<Vec<TriplePattern>> {
        self.c.skipws();
        if self.c.peek() != b'{' {
            return self.c.err("expected '{'");
        }
        self.c.bump();
        let mut list = Vec::new();
        loop {
            self.c.skipws();
            if self.c.eof() {
                return self.c.err("unterminated data block");
            }
            if self.c.peek() == b'}' {
                self.c.bump();
                return Ok(list);
            }
            if self.c.peek() == b'.' {
                self.c.bump();
                continue;
            }
            if self.c.kw("GRAPH") {
                return self.c.err("GRAPH is not supported in this subset");
            }
            self.triples_block(&mut list)?;
        }
    }

    fn parse(mut self) -> PResult<Parsed> {
        self.prologue()?;
        self.c.skipws();
        let mut q = Query {
            form: Form::Select,
            distinct: false,
            star: false,
            sel: Vec::new(),
            group_by: Vec::new(),
            template: Vec::new(),
            pattern: Group::default(),
            order: Vec::new(),
            limit: -1,
            offset: 0,
            prefixes: Vec::new(),
            base: None,
        };
        if self.c.kw("SELECT") {
            q.form = Form::Select;
            self.c.skipws();
            if self.c.kw("DISTINCT") || self.c.kw("REDUCED") {
                q.distinct = true;
            }
            self.c.skipws();
            if self.c.peek() == b'*' {
                q.star = true;
                self.c.bump();
            } else {
                loop {
                    self.c.skipws();
                    if self.c.peek() == b'?' || self.c.peek() == b'$' {
                        let var = self.varname()?;
                        q.sel.push(SelItem {
                            agg: Agg::None,
                            distinct: false,
                            star: false,
                            var: Some(var.clone()),
                            alias: var,
                        });
                    } else if self.c.peek() == b'(' {
                        let it = self.agg_item()?;
                        q.sel.push(it);
                    } else {
                        break;
                    }
                }
                if q.sel.is_empty() {
                    return self
                        .c
                        .err("SELECT needs '*' or at least one variable/aggregate");
                }
            }
            self.c.skipws();
            self.c.kw("WHERE"); // optional
            q.pattern = self.group()?;
        } else if self.c.kw("CONSTRUCT") {
            q.form = Form::Construct;
            self.c.skipws();
            if self.c.peek() != b'{' {
                return self.c.err("expected '{' after CONSTRUCT");
            }
            self.c.bump();
            loop {
                self.c.skipws();
                if self.c.peek() == b'}' {
                    self.c.bump();
                    break;
                }
                if self.c.peek() == b'.' {
                    self.c.bump();
                    continue;
                }
                if self.c.eof() {
                    return self.c.err("unterminated CONSTRUCT template");
                }
                let mut tmpl = std::mem::take(&mut q.template);
                self.triples_block(&mut tmpl)?;
                q.template = tmpl;
            }
            self.c.skipws();
            self.c.kw("WHERE"); // optional
            q.pattern = self.group()?;
        } else if self.c.kw("ASK") {
            q.form = Form::Ask;
            self.c.skipws();
            self.c.kw("WHERE"); // optional
            q.pattern = self.group()?;
        } else if self.c.kw("INSERT") {
            self.c.skipws();
            if !self.c.kw("DATA") {
                return self.c.err("only INSERT DATA is supported in this subset");
            }
            let triples = self.data_block()?;
            for t in &triples {
                for s in t.slots() {
                    if let Some(v) = s.var()
                        && !v.starts_with("~bn~")
                    {
                        return self.c.err("variables are not allowed in INSERT DATA");
                    }
                }
            }
            self.c.skipws();
            if !self.c.eof() {
                return self.c.err("unexpected trailing content");
            }
            return Ok(Parsed::Update(Update {
                kind: UpdateKind::InsertData,
                triples,
            }));
        } else if self.c.kw("DELETE") {
            self.c.skipws();
            let upd = if self.c.kw("DATA") {
                let triples = self.data_block()?;
                for t in &triples {
                    for s in t.slots() {
                        if s.var().is_some() {
                            return self
                                .c
                                .err("variables and blank nodes are not allowed in DELETE DATA");
                        }
                    }
                }
                Update {
                    kind: UpdateKind::DeleteData,
                    triples,
                }
            } else if self.c.kw("WHERE") {
                let g = self.group()?;
                if !g.filters.is_empty() || !g.optionals.is_empty() || !g.unions.is_empty() {
                    return self
                        .c
                        .err("DELETE WHERE supports basic graph patterns only");
                }
                Update {
                    kind: UpdateKind::DeleteWhere,
                    triples: g.triples,
                }
            } else {
                return self
                    .c
                    .err("only DELETE DATA and DELETE WHERE are supported in this subset");
            };
            self.c.skipws();
            if !self.c.eof() {
                return self.c.err("unexpected trailing content");
            }
            return Ok(Parsed::Update(upd));
        } else if self.c.kw("DESCRIBE") {
            return self
                .c
                .err("DESCRIBE is not supported in this subset (use CONSTRUCT)");
        } else {
            return self.c.err("expected SELECT or CONSTRUCT");
        }
        self.modifiers(&mut q)?;
        self.c.skipws();
        if !self.c.eof() {
            return self.c.err("unexpected trailing content");
        }
        q.prefixes = self.prefixes;
        q.base = self.base;
        Ok(Parsed::Query(q))
    }
}

/// Parse a SPARQL query or update in the supported subset.
pub fn parse(text: &str) -> Result<Parsed, ParseError> {
    Parser {
        c: Cur::new(text),
        prefixes: Vec::new(),
        base: None,
    }
    .parse()
}
