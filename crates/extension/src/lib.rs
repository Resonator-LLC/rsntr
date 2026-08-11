//! Loadable SQLite extension packaging of the SPARQL engine.
//!
//! Compiled as a cdylib in rusqlite loadable_extension mode: every sqlite
//! call is routed through the host process's sqlite3_api_routines, so the
//! produced library loads into any stock sqlite3 (CLI `.load`, Python
//! `enable_load_extension`, C `sqlite3_load_extension`). It registers the
//! full resonator-sparql SQL surface on the calling connection: rdf_init,
//! rdf_load_turtle(_file), rdf_query, rdf_update, rdf_dump_turtle,
//! rdf_regexp and the sparql() table-valued function.

use std::os::raw::{c_char, c_int};

use rusqlite::{Connection, Result, ffi};

/// Entry point sqlite3 resolves when no explicit entry point is given to
/// `.load` / load_extension().
///
/// # Safety
/// Called by SQLite with a valid db handle and api-routines pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sqlite3_extension_init(
    db: *mut ffi::sqlite3,
    pz_err_msg: *mut *mut c_char,
    p_api: *mut ffi::sqlite3_api_routines,
) -> c_int {
    unsafe { Connection::extension_init2(db, pz_err_msg, p_api, init) }
}

fn init(db: Connection) -> Result<bool> {
    resonator_sparql::register(&db)?;
    // true -> SQLITE_OK_LOAD_PERMANENTLY: the registered function and vtab
    // callbacks live in this dylib and must survive past load_extension().
    Ok(true)
}
