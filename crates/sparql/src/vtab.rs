//! The eponymous sparql() table-valued function: one row per solution,
//! column `binding` is a {"var": "value"} JSON object, column `query` is
//! the hidden input. SELECT queries only.

use std::borrow::Cow;
use std::ffi::{CStr, c_int};

use rusqlite::ffi;
use rusqlite::vtab::{
    Context, Filters, IndexConstraintOp, IndexInfo, Module, VTab, VTabConnection, VTabCursor,
};
use rusqlite::{Connection, Error, Result};

use crate::ast::{Form, Parsed};
use crate::parser::parse;
use crate::serialize::binding_object;
use crate::store;

pub fn register(conn: &Connection) -> Result<()> {
    const MODULE: Module<SparqlTab> = Module::eponymous_only_module();
    conn.create_module(c"sparql", &MODULE, None::<()>)
}

#[repr(C)]
pub struct SparqlTab {
    base: ffi::sqlite3_vtab,
    db: *mut ffi::sqlite3,
}

unsafe impl<'vtab> VTab<'vtab> for SparqlTab {
    type Aux = ();
    type Cursor = SparqlCursor;

    fn connect(
        db: &mut VTabConnection,
        _aux: Option<&()>,
        _module_name: &[u8],
        _database_name: &[u8],
        _table_name: &[u8],
        _args: &[&[u8]],
    ) -> Result<(Cow<'static, CStr>, Self)> {
        let handle = unsafe { db.handle() };
        Ok((
            Cow::Borrowed(c"CREATE TABLE x(binding TEXT, query TEXT HIDDEN)"),
            SparqlTab {
                base: unsafe { std::mem::zeroed() },
                db: handle,
            },
        ))
    }

    fn best_index(&self, info: &mut IndexInfo) -> Result<bool> {
        let mut idx_num = 0;
        let mut query_arg = None;
        for (i, (constraint, _)) in info.constraints_and_usages().enumerate() {
            if constraint.column() == 1
                && matches!(
                    constraint.operator(),
                    IndexConstraintOp::SQLITE_INDEX_CONSTRAINT_EQ
                )
                && constraint.is_usable()
            {
                query_arg = Some(i);
                idx_num = 1;
                break;
            }
        }
        if let Some(i) = query_arg {
            let mut usage = info.constraint_usage(i);
            usage.set_argv_index(1);
            usage.set_omit(true);
        }
        info.set_idx_num(idx_num);
        info.set_estimated_cost(1000.0);
        Ok(true)
    }

    fn open(&'_ mut self) -> Result<SparqlCursor> {
        Ok(SparqlCursor {
            base: unsafe { std::mem::zeroed() },
            db: self.db,
            qtext: String::new(),
            rows: Vec::new(),
            row: 0,
        })
    }
}

#[repr(C)]
pub struct SparqlCursor {
    base: ffi::sqlite3_vtab_cursor,
    db: *mut ffi::sqlite3,
    qtext: String,
    rows: Vec<String>,
    row: usize,
}

unsafe impl VTabCursor for SparqlCursor {
    fn filter(&mut self, idx_num: c_int, _idx_str: Option<&str>, args: &Filters<'_>) -> Result<()> {
        self.rows.clear();
        self.row = 0;
        if idx_num != 1 || args.is_empty() {
            return Err(Error::ModuleError(
                "sparql: usage SELECT binding FROM sparql('SELECT ...')".to_string(),
            ));
        }
        self.qtext = args.get::<String>(0)?;
        let conn = unsafe { Connection::from_handle(self.db)? };
        store::ensure_schema(&conn).map_err(|e| Error::ModuleError(e.0))?;
        let q = match parse(&self.qtext).map_err(|e| Error::ModuleError(e.0))? {
            Parsed::Query(q) if q.form == Form::Select => q,
            _ => {
                return Err(Error::ModuleError(
                    "sparql: only SELECT here; use rdf_query() for CONSTRUCT".to_string(),
                ));
            }
        };
        let (vars, rows) = store::exec_select(&conn, &q).map_err(|e| Error::ModuleError(e.0))?;
        self.rows = rows.iter().map(|r| binding_object(&vars, r)).collect();
        Ok(())
    }

    fn next(&mut self) -> Result<()> {
        self.row += 1;
        Ok(())
    }

    fn eof(&self) -> bool {
        self.row >= self.rows.len()
    }

    fn column(&self, ctx: &mut Context, i: c_int) -> Result<()> {
        if i == 1 {
            ctx.set_result(&self.qtext)
        } else {
            ctx.set_result(&self.rows[self.row])
        }
    }

    fn rowid(&self) -> Result<i64> {
        Ok(self.row as i64 + 1)
    }
}
