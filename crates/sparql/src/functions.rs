//! SQL surface: rdf_init, rdf_load_turtle, rdf_load_turtle_file, rdf_query,
//! rdf_update, rdf_dump_turtle, rdf_regexp plus the sparql() table function.

use rusqlite::functions::{Context, FunctionFlags};
use rusqlite::types::ValueRef;
use rusqlite::{Connection, Result};

use crate::store;

fn uf_err(msg: String) -> rusqlite::Error {
    rusqlite::Error::UserFunctionError(Box::new(store::Error(msg)))
}

fn text_arg(ctx: &Context, idx: usize, what: &str) -> Result<String> {
    match ctx.get_raw(idx) {
        ValueRef::Null => Err(uf_err(what.to_string())),
        v => Ok(String::from_utf8_lossy(v.as_bytes_or_null()?.unwrap_or(&[])).into_owned()),
    }
}

fn opt_text_arg(ctx: &Context, idx: usize) -> Result<Option<String>> {
    if ctx.len() <= idx {
        return Ok(None);
    }
    match ctx.get_raw(idx) {
        ValueRef::Null => Ok(None),
        ValueRef::Text(t) => Ok(Some(String::from_utf8_lossy(t).into_owned())),
        v => Ok(Some(
            String::from_utf8_lossy(v.as_bytes_or_null()?.unwrap_or(&[])).into_owned(),
        )),
    }
}

fn regexp_match(pattern: &str, text: &str, flags: &str) -> Result<bool> {
    let pat = if flags.contains('i') {
        format!("(?i){pattern}")
    } else {
        pattern.to_string()
    };
    let re =
        regex::Regex::new(&pat).map_err(|e| uf_err(format!("rdf_regexp: bad pattern: {e}")))?;
    Ok(re.is_match(text))
}

fn fn_regexp(ctx: &Context) -> Result<Option<i64>> {
    if ctx.get_raw(0) == ValueRef::Null || ctx.get_raw(1) == ValueRef::Null {
        return Ok(None);
    }
    let pattern: String = ctx.get(0)?;
    let text: String = ctx.get(1)?;
    let flags = opt_text_arg(ctx, 2)?.unwrap_or_default();
    Ok(Some(regexp_match(&pattern, &text, &flags)? as i64))
}

fn fn_load_turtle(ctx: &Context) -> Result<i64> {
    let text = text_arg(ctx, 0, "rdf_load_turtle: text is NULL")?;
    let base = opt_text_arg(ctx, 1)?;
    let conn = unsafe { ctx.get_connection()? };
    store::load_turtle(&conn, &text, base.as_deref()).map_err(|e| uf_err(e.0))
}

fn fn_load_turtle_file(ctx: &Context) -> Result<i64> {
    let path = text_arg(ctx, 0, "rdf_load_turtle_file: path is NULL")?;
    let base = opt_text_arg(ctx, 1)?;
    let conn = unsafe { ctx.get_connection()? };
    store::load_turtle_file(&conn, &path, base.as_deref()).map_err(|e| uf_err(e.0))
}

fn fn_query(ctx: &Context) -> Result<String> {
    let sparql = text_arg(ctx, 0, "rdf_query: query is NULL")?;
    let format = opt_text_arg(ctx, 1)?;
    let conn = unsafe { ctx.get_connection()? };
    store::run_query(&conn, &sparql, format.as_deref()).map_err(|e| uf_err(e.0))
}

fn fn_update(ctx: &Context) -> Result<i64> {
    let sparql = text_arg(ctx, 0, "rdf_update: update is NULL")?;
    let conn = unsafe { ctx.get_connection()? };
    store::update(&conn, &sparql).map_err(|e| uf_err(e.0))
}

fn fn_dump_turtle(ctx: &Context) -> Result<String> {
    let conn = unsafe { ctx.get_connection()? };
    store::dump_turtle(&conn).map_err(|e| uf_err(e.0))
}

fn fn_init(ctx: &Context) -> Result<String> {
    let conn = unsafe { ctx.get_connection()? };
    store::ensure_schema(&conn).map_err(|e| uf_err(e.0))?;
    Ok("ok".to_string())
}

/// Register the SQL functions and the sparql() table-valued function on a
/// connection. Idempotent.
pub fn register(conn: &Connection) -> Result<()> {
    let utf8 = FunctionFlags::SQLITE_UTF8;
    let direct = FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DIRECTONLY;
    conn.create_scalar_function("rdf_init", 0, utf8, fn_init)?;
    conn.create_scalar_function("rdf_load_turtle", 1, direct, fn_load_turtle)?;
    conn.create_scalar_function("rdf_load_turtle", 2, direct, fn_load_turtle)?;
    conn.create_scalar_function("rdf_load_turtle_file", 1, direct, fn_load_turtle_file)?;
    conn.create_scalar_function("rdf_load_turtle_file", 2, direct, fn_load_turtle_file)?;
    conn.create_scalar_function("rdf_query", 1, utf8, fn_query)?;
    conn.create_scalar_function("rdf_query", 2, utf8, fn_query)?;
    conn.create_scalar_function("rdf_update", 1, direct, fn_update)?;
    conn.create_scalar_function("rdf_dump_turtle", 0, utf8, fn_dump_turtle)?;
    conn.create_scalar_function("rdf_regexp", 2, utf8, fn_regexp)?;
    conn.create_scalar_function("rdf_regexp", 3, utf8, fn_regexp)?;
    crate::vtab::register(conn)?;
    Ok(())
}
