//! SPARQL query AST for the supported subset.

use crate::term::Term;

/// A slot of a triple pattern: variable or ground term.
/// Blank node labels in queries are turned into variables named "~bn~<label>".
#[derive(Clone, Debug)]
pub enum Slot {
    Var(String),
    Ground(Term),
}

impl Slot {
    pub fn var(&self) -> Option<&str> {
        match self {
            Slot::Var(v) => Some(v),
            Slot::Ground(_) => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct TriplePattern {
    pub s: Slot,
    pub p: Slot,
    pub o: Slot,
}

impl TriplePattern {
    pub fn slots(&self) -> [&Slot; 3] {
        [&self.s, &self.p, &self.o]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CmpOp {
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
}

impl CmpOp {
    pub fn sql(self) -> &'static str {
        match self {
            CmpOp::Eq => "=",
            CmpOp::Ne => "<>",
            CmpOp::Lt => "<",
            CmpOp::Gt => ">",
            CmpOp::Le => "<=",
            CmpOp::Ge => ">=",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Func {
    None,
    Str,
    Lang,
    Datatype,
}

/// A value operand: a (possibly function-wrapped) variable or a ground term.
#[derive(Clone, Debug)]
pub struct Val {
    pub func: Func,
    pub var: Option<String>,
    pub term: Option<Term>,
}

#[derive(Clone, Debug)]
pub enum Expr {
    Or(Box<Expr>, Box<Expr>),
    And(Box<Expr>, Box<Expr>),
    Not(Box<Expr>),
    Bound(String),
    Regex {
        text: Box<Expr>,
        pattern: Box<Expr>,
        flags: Option<Box<Expr>>,
    },
    Cmp {
        op: CmpOp,
        a: Box<Expr>,
        b: Box<Expr>,
    },
    Val(Val),
}

/// A group graph pattern: required triples, filters, OPTIONAL subgroups and
/// UNION blocks (each block is a chain of alternative branches).
#[derive(Clone, Debug, Default)]
pub struct Group {
    pub triples: Vec<TriplePattern>,
    pub filters: Vec<Expr>,
    pub optionals: Vec<Group>,
    pub unions: Vec<Vec<Group>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Form {
    Select,
    Construct,
    Ask,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Agg {
    None,
    Count,
    Sum,
    Min,
    Max,
    Avg,
}

/// One SELECT item: a plain variable or (AGG([DISTINCT] ?v|*) AS ?alias).
#[derive(Clone, Debug)]
pub struct SelItem {
    pub agg: Agg,
    pub distinct: bool,
    pub star: bool,
    pub var: Option<String>,
    pub alias: String,
}

#[derive(Clone, Debug)]
pub struct OrdKey {
    pub var: String,
    pub desc: bool,
}

#[derive(Clone, Debug)]
pub struct Query {
    pub form: Form,
    pub distinct: bool,
    pub star: bool,
    pub sel: Vec<SelItem>,
    pub group_by: Vec<String>,
    pub template: Vec<TriplePattern>,
    pub pattern: Group,
    pub order: Vec<OrdKey>,
    pub limit: i64,
    pub offset: i64,
    pub prefixes: Vec<(String, String)>,
    pub base: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpdateKind {
    InsertData,
    DeleteData,
    DeleteWhere,
}

#[derive(Clone, Debug)]
pub struct Update {
    pub kind: UpdateKind,
    /// Ground data for INSERT DATA / DELETE DATA, pattern for DELETE WHERE.
    pub triples: Vec<TriplePattern>,
}

#[derive(Clone, Debug)]
#[allow(clippy::large_enum_variant)]
pub enum Parsed {
    Query(Query),
    Update(Update),
}
