//! Compile a parsed query into ONE SQL SELECT over rdf_terms/rdf_triples.
//!
//! Layer 1: UNION ALL of one SELECT per flattened alternative (UNION is
//! distributed over the enclosing group into conjunctive "flats").
//! Layer 2: aggregation (GROUP BY / COUNT / SUM...) or DISTINCT projection.
//! Layer 3: ORDER BY / LIMIT / OFFSET wrapper when needed.
//!
//! Each projected variable yields 5 result columns: id, kind, lex, dtype,
//! lang (aliases i<n>, k<n>, x<n>, d<n>, g<n>). All patterns are constrained
//! to the default graph (g = 0) until GRAPH support lands.

use crate::ast::*;
use crate::term::{K_LIT, Term, XSD, is_numeric_dtype};

/// Resolves a ground term to its rdf_terms id (0 when absent, which can
/// never match a stored triple).
pub trait TermIds {
    fn term_id(&mut self, t: &Term) -> i64;
}

impl<F: FnMut(&Term) -> i64> TermIds for F {
    fn term_id(&mut self, t: &Term) -> i64 {
        self(t)
    }
}

const MAX_FLATS: usize = 128;

#[derive(Clone, Default)]
struct Flat<'a> {
    tps: Vec<&'a TriplePattern>,
    filts: Vec<&'a Expr>,
    opts: Vec<&'a Group>,
}

impl<'a> Flat<'a> {
    fn add_group(&mut self, g: &'a Group) {
        self.tps.extend(g.triples.iter());
        self.filts.extend(g.filters.iter());
        self.opts.extend(g.optionals.iter());
    }
}

fn flatten_group<'a>(g: &'a Group, mut list: Vec<Flat<'a>>) -> Result<Vec<Flat<'a>>, String> {
    for f in &mut list {
        f.add_group(g);
    }
    for branches in &g.unions {
        let mut expanded: Vec<Flat<'a>> = Vec::new();
        for f in &list {
            for br in branches {
                let seeded = flatten_group(br, vec![f.clone()])?;
                expanded.extend(seeded);
                if expanded.len() > MAX_FLATS {
                    return Err(format!(
                        "UNION expansion too large (max {MAX_FLATS} branches)"
                    ));
                }
            }
        }
        list = expanded;
    }
    Ok(list)
}

struct VInfo {
    name: String,
    idref: String,
    alias: String,
    optional: bool,
}

#[derive(Default)]
struct VList {
    v: Vec<VInfo>,
}

impl VList {
    fn find(&self, name: &str) -> Option<&VInfo> {
        self.v.iter().find(|vi| vi.name == name)
    }

    fn add(&mut self, name: &str, idref: &str, optional: bool, alias_prefix: &str) {
        let alias = format!("{}{}", alias_prefix, self.v.len());
        self.v.push(VInfo {
            name: name.to_string(),
            idref: idref.to_string(),
            alias,
            optional,
        });
    }
}

fn add_conj(w: &mut String, cond: &str) {
    if !w.is_empty() {
        w.push_str(" AND ");
    }
    w.push_str(cond);
}

fn expr_collect_vars(e: &Expr, out: &mut Vec<String>) {
    match e {
        Expr::Or(a, b) | Expr::And(a, b) => {
            expr_collect_vars(a, out);
            expr_collect_vars(b, out);
        }
        Expr::Not(a) => expr_collect_vars(a, out),
        Expr::Bound(v) => {
            if !out.iter().any(|x| x == v) {
                out.push(v.clone());
            }
        }
        Expr::Regex {
            text,
            pattern,
            flags,
        } => {
            expr_collect_vars(text, out);
            expr_collect_vars(pattern, out);
            if let Some(f) = flags {
                expr_collect_vars(f, out);
            }
        }
        Expr::Cmp { a, b, .. } => {
            expr_collect_vars(a, out);
            expr_collect_vars(b, out);
        }
        Expr::Val(val) => {
            if let Some(v) = &val.var
                && !out.iter().any(|x| x == v)
            {
                out.push(v.clone());
            }
        }
    }
}

fn sql_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

fn operand_is_numeric_lit(v: &Val) -> bool {
    v.term
        .as_ref()
        .is_some_and(|t| t.kind == K_LIT && is_numeric_dtype(&t.dtype))
}

/// Emit the SQL value of a Val operand.
fn compile_value(v: &Val, ctx: &VList, numeric: bool, lowered: bool, out: &mut String) {
    if let Some(var) = &v.var {
        let Some(vi) = ctx.find(var) else {
            // Variable unbound in this branch: SPARQL treats comparing an
            // unbound value as an error that filters the row; SQL NULL
            // propagates the same way.
            out.push_str("NULL");
            return;
        };
        let field = match v.func {
            Func::Lang => "lang",
            Func::Datatype => "dtype",
            _ => "lex",
        };
        if lowered {
            out.push_str("lower(");
        }
        if numeric && v.func == Func::None {
            out.push_str(&format!("CAST({}.lex AS REAL)", vi.alias));
        } else {
            out.push_str(&format!("{}.{}", vi.alias, field));
        }
        if lowered {
            out.push(')');
        }
        return;
    }
    let term = v.term.as_ref().expect("ground Val");
    if term.kind == K_LIT && is_numeric_dtype(&term.dtype) && numeric {
        out.push_str(&term.lex);
        return;
    }
    if lowered {
        out.push_str("lower(");
    }
    out.push_str(&sql_quote(&term.lex));
    if lowered {
        out.push(')');
    }
}

fn as_val(e: &Expr) -> Result<&Val, String> {
    match e {
        Expr::Val(v) => Ok(v),
        _ => Err("unsupported REGEX operand".to_string()),
    }
}

fn compile_expr(e: &Expr, ctx: &VList, out: &mut String) -> Result<(), String> {
    match e {
        Expr::Or(a, b) | Expr::And(a, b) => {
            out.push('(');
            compile_expr(a, ctx, out)?;
            out.push_str(if matches!(e, Expr::Or(..)) {
                " OR "
            } else {
                " AND "
            });
            compile_expr(b, ctx, out)?;
            out.push(')');
            Ok(())
        }
        Expr::Not(a) => {
            out.push_str("(NOT ");
            compile_expr(a, ctx, out)?;
            out.push(')');
            Ok(())
        }
        Expr::Bound(var) => {
            match ctx.find(var) {
                Some(vi) => out.push_str(&format!("({}.id IS NOT NULL)", vi.alias)),
                None => out.push_str("(0)"), // never bound in this branch
            }
            Ok(())
        }
        Expr::Regex {
            text,
            pattern,
            flags,
        } => {
            out.push_str("rdf_regexp(");
            compile_value(as_val(pattern)?, ctx, false, false, out);
            out.push(',');
            compile_value(as_val(text)?, ctx, false, false, out);
            out.push(',');
            match flags {
                Some(f) => compile_value(as_val(f)?, ctx, false, false, out),
                None => out.push_str("''"),
            }
            out.push(')');
            Ok(())
        }
        Expr::Cmp { op, a, b } => {
            let (Expr::Val(av), Expr::Val(bv)) = (a.as_ref(), b.as_ref()) else {
                return Err("unsupported comparison operand".to_string());
            };
            let numeric = operand_is_numeric_lit(av) || operand_is_numeric_lit(bv);
            let lowered = av.func == Func::Lang || bv.func == Func::Lang;
            if matches!(op, CmpOp::Eq | CmpOp::Ne)
                && av.var.is_some()
                && bv.var.is_some()
                && av.func == Func::None
                && bv.func == Func::None
            {
                let va = ctx.find(av.var.as_deref().unwrap());
                let vb = ctx.find(bv.var.as_deref().unwrap());
                match (va, vb) {
                    (Some(va), Some(vb)) => {
                        out.push_str(&format!("({}.id {} {}.id)", va.alias, op.sql(), vb.alias));
                    }
                    _ => out.push('0'), // comparison with an unbound variable
                }
                return Ok(());
            }
            out.push('(');
            compile_value(av, ctx, numeric, lowered, out);
            out.push_str(&format!(" {} ", op.sql()));
            compile_value(bv, ctx, numeric, lowered, out);
            out.push(')');
            Ok(())
        }
        Expr::Val(v) => {
            if v.var.is_none()
                && let Some(t) = &v.term
                && t.kind == K_LIT
                && t.dtype == format!("{XSD}boolean")
            {
                out.push_str(if t.lex == "true" { "1" } else { "0" });
                return Ok(());
            }
            Err("expression form not supported in FILTER".to_string())
        }
    }
}

/// Variables of a flat, in first-appearance order (triples, then OPTIONALs).
fn flat_collect_vars(fl: &Flat, out: &mut Vec<String>) {
    for tp in &fl.tps {
        for slot in tp.slots() {
            if let Some(v) = slot.var()
                && !out.iter().any(|x| x == v)
            {
                out.push(v.to_string());
            }
        }
    }
    for og in &fl.opts {
        for tp in &og.triples {
            for slot in tp.slots() {
                if let Some(v) = slot.var()
                    && !out.iter().any(|x| x == v)
                {
                    out.push(v.to_string());
                }
            }
        }
    }
}

/// Compile one flat into "SELECT <5 cols per proj var> FROM ... WHERE ...".
/// Projected vars unknown to this flat come out as NULL columns (unbound).
fn compile_flat(ids: &mut dyn TermIds, fl: &Flat, proj: &[String]) -> Result<String, String> {
    let mut vars = VList::default();
    let mut from = String::new();
    let mut where_ = String::new();
    let mut sel = String::new();

    // required triple patterns
    for (i, tr) in fl.tps.iter().enumerate() {
        let al = format!("t{i}");
        if i == 0 {
            from.push_str(&format!("rdf_triples {al}"));
        } else {
            from.push_str(&format!(" JOIN rdf_triples {al} ON 1=1"));
        }
        add_conj(&mut where_, &format!("{al}.g=0"));
        for (k, slot) in tr.slots().iter().enumerate() {
            let col = ["s", "p", "o"][k];
            match slot {
                Slot::Var(v) => {
                    if let Some(vi) = vars.find(v) {
                        add_conj(&mut where_, &format!("{al}.{col}={}", vi.idref));
                    } else {
                        vars.add(v, &format!("{al}.{col}"), false, "v");
                    }
                }
                Slot::Ground(t) => {
                    let id = ids.term_id(t);
                    add_conj(&mut where_, &format!("{al}.{col}={id}"));
                }
            }
        }
    }
    if fl.tps.is_empty() {
        from.push_str("(SELECT 1 AS one) one_");
    }

    // OPTIONAL groups -> LEFT JOIN (subselect)
    for (gi, og) in fl.opts.iter().enumerate() {
        if og.triples.is_empty() {
            continue;
        }
        let mut ivars = VList::default();
        let mut ifrom = String::new();
        let mut iwhere = String::new();
        let mut isel = String::new();
        let mut oncond = String::new();
        for (j, tp) in og.triples.iter().enumerate() {
            let al = format!("o{gi}_{j}");
            if j == 0 {
                ifrom.push_str(&format!("rdf_triples {al}"));
            } else {
                ifrom.push_str(&format!(" JOIN rdf_triples {al} ON 1=1"));
            }
            add_conj(&mut iwhere, &format!("{al}.g=0"));
            for (k, slot) in tp.slots().iter().enumerate() {
                let col = ["s", "p", "o"][k];
                match slot {
                    Slot::Var(v) => {
                        if let Some(vi) = ivars.find(v) {
                            add_conj(&mut iwhere, &format!("{al}.{col}={}", vi.idref));
                        } else {
                            ivars.add(v, &format!("{al}.{col}"), false, &format!("f{gi}_"));
                        }
                    }
                    Slot::Ground(t) => {
                        let id = ids.term_id(t);
                        add_conj(&mut iwhere, &format!("{al}.{col}={id}"));
                    }
                }
            }
        }
        // inner filters need rdf_terms joins inside the subselect
        if !og.filters.is_empty() {
            let mut used = Vec::new();
            for f in &og.filters {
                expr_collect_vars(f, &mut used);
            }
            for name in &used {
                if let Some(iv) = ivars.find(name) {
                    // vars not bound inside compile as unbound (NULL)
                    ifrom.push_str(&format!(
                        " JOIN rdf_terms {} ON {}.id={}",
                        iv.alias, iv.alias, iv.idref
                    ));
                }
            }
            for f in &og.filters {
                let mut e = String::new();
                compile_expr(f, &ivars, &mut e)?;
                add_conj(&mut iwhere, &format!("({e})"));
            }
        }
        for (j, iv) in ivars.v.iter().enumerate() {
            if j > 0 {
                isel.push(',');
            }
            isel.push_str(&format!("{} AS c{}", iv.idref, j));
        }
        // join to the outer query on shared required vars
        for (j, iv) in ivars.v.iter().enumerate() {
            match vars.find(&iv.name) {
                Some(outer) if !outer.optional => {
                    if !oncond.is_empty() {
                        oncond.push_str(" AND ");
                    }
                    oncond.push_str(&format!("opt{gi}.c{j}={}", outer.idref));
                }
                None => {
                    vars.add(&iv.name, &format!("opt{gi}.c{j}"), true, "v");
                }
                // already-optional shared var: no join (documented approximation)
                Some(_) => {}
            }
        }
        from.push_str(&format!(
            " LEFT JOIN (SELECT {} FROM {}{}{}) opt{} ON {}",
            if isel.is_empty() { "1" } else { &isel },
            ifrom,
            if iwhere.is_empty() { "" } else { " WHERE " },
            iwhere,
            gi,
            if oncond.is_empty() { "1=1" } else { &oncond }
        ));
    }

    // one rdf_terms join per variable
    for vi in &vars.v {
        from.push_str(&format!(
            "{} rdf_terms {} ON {}.id={}",
            if vi.optional { " LEFT JOIN" } else { " JOIN" },
            vi.alias,
            vi.alias,
            vi.idref
        ));
    }

    // this flat's filters
    for f in &fl.filts {
        let mut e = String::new();
        compile_expr(f, &vars, &mut e)?;
        add_conj(&mut where_, &format!("({e})"));
    }

    // projection: 5 columns (id, kind, lex, dtype, lang) per variable
    for (i, name) in proj.iter().enumerate() {
        if i > 0 {
            sel.push(',');
        }
        match vars.find(name) {
            Some(vi) => sel.push_str(&format!(
                "{a}.id AS i{i},{a}.kind AS k{i},{a}.lex AS x{i},{a}.dtype AS d{i},{a}.lang AS g{i}",
                a = vi.alias
            )),
            None => sel.push_str(&format!(
                "NULL AS i{i},NULL AS k{i},NULL AS x{i},NULL AS d{i},NULL AS g{i}"
            )),
        }
    }
    if proj.is_empty() {
        sel.push_str("1 AS one_");
    }

    let mut out = format!("SELECT {sel} FROM {from}");
    if !where_.is_empty() {
        out.push_str(&format!(" WHERE {where_}"));
    }
    Ok(out)
}

/// ORDER BY key over the positional columns x<idx>/d<idx>: numeric literals
/// sort numerically, everything else lexically.
fn order_expr(ord: &mut String, idx: usize, desc: bool) {
    let dir = if desc { " DESC" } else { "" };
    if !ord.is_empty() {
        ord.push(',');
    }
    ord.push_str(&format!(
        "CASE WHEN d{idx} IN ('{x}integer','{x}decimal','{x}double','{x}float','{x}long','{x}int') \
         THEN CAST(x{idx} AS REAL) END{dir},x{idx}{dir}",
        x = XSD
    ));
}

/// Compile a parsed query into one SQL statement plus the visible output
/// variable names (each mapping to 5 result columns).
pub fn compile_query(q: &Query, ids: &mut dyn TermIds) -> Result<(String, Vec<String>), String> {
    let mut aggmode = !q.group_by.is_empty();
    for it in &q.sel {
        if it.agg != Agg::None {
            aggmode = true;
        }
    }

    let flats = flatten_group(&q.pattern, vec![Flat::default()])?;

    // visible output names
    let mut vis: Vec<String> = Vec::new();
    match q.form {
        Form::Construct => {
            if aggmode {
                return Err("GROUP BY / aggregates cannot be used with CONSTRUCT".to_string());
            }
            for tp in &q.template {
                for slot in tp.slots() {
                    if let Some(v) = slot.var()
                        && !vis.iter().any(|x| x == v)
                    {
                        vis.push(v.to_string());
                    }
                }
            }
        }
        Form::Ask => {}
        Form::Select if aggmode => {
            if q.star {
                return Err("SELECT * cannot be combined with aggregates or GROUP BY".to_string());
            }
            for it in &q.sel {
                if it.agg == Agg::None {
                    let var = it.var.as_deref().unwrap_or("");
                    if !q.group_by.iter().any(|g| g == var) {
                        return Err(format!(
                            "?{var} must appear in GROUP BY or inside an aggregate"
                        ));
                    }
                }
                vis.push(it.alias.clone());
            }
        }
        Form::Select if q.star => {
            let mut all = Vec::new();
            for f in &flats {
                flat_collect_vars(f, &mut all);
            }
            for v in all {
                if v.starts_with("~bn~") {
                    continue;
                }
                if !vis.contains(&v) {
                    vis.push(v);
                }
            }
            if vis.is_empty() {
                return Err("SELECT * found no variables".to_string());
            }
        }
        Form::Select => {
            for it in &q.sel {
                vis.push(it.alias.clone());
            }
        }
    }

    // inner (layer-1) projection
    let mut inner: Vec<String> = Vec::new();
    if aggmode {
        for g in &q.group_by {
            if !inner.iter().any(|x| x == g) {
                inner.push(g.clone());
            }
        }
        for it in &q.sel {
            if it.agg != Agg::None
                && let Some(v) = &it.var
                && !inner.iter().any(|x| x == v)
            {
                inner.push(v.clone());
            }
        }
    } else {
        inner.extend(vis.iter().cloned());
        // internal-only sort columns for ORDER BY vars that aren't projected
        for o in &q.order {
            if !inner.contains(&o.var) {
                inner.push(o.var.clone());
            }
        }
    }

    // layer 1: UNION ALL over the flats
    let mut sub = String::new();
    for (f, fl) in flats.iter().enumerate() {
        let one = compile_flat(ids, fl, &inner)?;
        if f > 0 {
            sub.push_str(" UNION ALL ");
        }
        sub.push_str(&one);
    }

    if q.form == Form::Ask {
        return Ok((format!("SELECT 1 FROM ({sub}) sub LIMIT 1"), vis));
    }

    let mut core = String::new();
    let mut ord = String::new();
    if aggmode {
        // layer 2: aggregate select
        let mut items = String::new();
        for (m, it) in q.sel.iter().enumerate() {
            let j = it
                .var
                .as_ref()
                .and_then(|v| inner.iter().position(|x| x == v))
                .unwrap_or(usize::MAX);
            if m > 0 {
                items.push(',');
            }
            match it.agg {
                Agg::None => items.push_str(&format!(
                    "i{j} AS i{m},k{j} AS k{m},x{j} AS x{m},d{j} AS d{m},g{j} AS g{m}"
                )),
                Agg::Count => {
                    let cnt = if it.star {
                        "COUNT(*)".to_string()
                    } else {
                        format!("COUNT({}i{j})", if it.distinct { "DISTINCT " } else { "" })
                    };
                    items.push_str(&format!(
                        "1 AS i{m},2 AS k{m},{cnt} AS x{m},'{XSD}integer' AS d{m},'' AS g{m}"
                    ));
                }
                _ => {
                    let f = match it.agg {
                        Agg::Sum => "SUM",
                        Agg::Min => "MIN",
                        Agg::Max => "MAX",
                        _ => "AVG",
                    };
                    let eb = format!(
                        "{f}({}CAST(x{j} AS REAL))",
                        if it.distinct { "DISTINCT " } else { "" }
                    );
                    items.push_str(&format!(
                        "CASE WHEN {eb} IS NULL THEN NULL ELSE 1 END AS i{m},\
                         2 AS k{m},{eb} AS x{m},'{XSD}decimal' AS d{m},'' AS g{m}"
                    ));
                }
            }
        }
        let mut grpby = String::new();
        for (i, g) in q.group_by.iter().enumerate() {
            let j = inner.iter().position(|x| x == g).unwrap_or(usize::MAX);
            if i > 0 {
                grpby.push(',');
            }
            grpby.push_str(&format!("i{j}"));
        }
        core.push_str(&format!(
            "SELECT {}{} FROM ({}) sub",
            if q.distinct { "DISTINCT " } else { "" },
            items,
            sub
        ));
        if !q.group_by.is_empty() {
            core.push_str(&format!(" GROUP BY {grpby}"));
        }
        for o in &q.order {
            let Some(m) = vis.iter().position(|x| *x == o.var) else {
                continue;
            };
            order_expr(&mut ord, m, o.desc);
        }
    } else {
        // layer 2: plain (possibly DISTINCT) projection of the visible columns
        let mut cols = String::new();
        for i in 0..vis.len() {
            if i > 0 {
                cols.push(',');
            }
            cols.push_str(&format!("i{i},k{i},x{i},d{i},g{i}"));
        }
        for o in &q.order {
            let Some(j) = inner.iter().position(|x| *x == o.var) else {
                continue;
            };
            if q.distinct && j >= vis.len() {
                return Err(format!(
                    "ORDER BY variable ?{} must be projected when DISTINCT is used",
                    o.var
                ));
            }
            order_expr(&mut ord, j, o.desc);
        }
        if q.distinct {
            core.push_str(&format!("SELECT DISTINCT {cols} FROM ({sub}) sub"));
        } else if !ord.is_empty() {
            // keep internal sort columns reachable: order at this level
            core.push_str(&format!("SELECT {cols} FROM ({sub}) sub ORDER BY {ord}"));
            ord.clear(); // consumed
        } else {
            core.push_str(&format!("SELECT {cols} FROM ({sub}) sub"));
        }
    }

    // layer 3: ORDER BY (when still pending) and LIMIT/OFFSET
    let mut sql = if ord.is_empty() {
        core
    } else {
        format!("SELECT * FROM ({core}) a ORDER BY {ord}")
    };
    if q.limit >= 0 || q.offset > 0 {
        sql.push_str(&format!(
            " LIMIT {}",
            if q.limit >= 0 { q.limit } else { -1 }
        ));
        if q.offset > 0 {
            sql.push_str(&format!(" OFFSET {}", q.offset));
        }
    }
    Ok((sql, vis))
}
