//! The `time` mod: answers any request with one row holding the node's
//! current UTC time. The M6 validation plugin.

use extism_pdk::{FnResult, Json, plugin_fn};
use resonator_mod_pdk::{Descriptor, HandleResult, StatementIn, Value, host};

#[plugin_fn]
pub fn describe() -> FnResult<Json<Descriptor>> {
    Ok(Json(Descriptor {
        abi: 1,
        name: "time".to_string(),
        version: "0.1.0".to_string(),
        help_text: "returns the node's current UTC time as one row (column: now)".to_string(),
        topics: vec![],
        needs: vec!["clock".to_string()],
    }))
}

#[plugin_fn]
pub fn handle(Json(st): Json<StatementIn>) -> FnResult<Json<HandleResult>> {
    // Diagnostic: signal "db" runs one read through the host's gated
    // db_query path (traps unless the db_read cap was granted).
    if st.text.trim() == "db" {
        let out = host::db_query("SELECT 1 AS one", &[])?;
        host::emit_result(&["one"])?;
        for (i, row) in out.rows.iter().enumerate() {
            let cells = out
                .columns
                .iter()
                .cloned()
                .zip(row.iter().cloned())
                .collect();
            host::emit_row(i as i64 + 1, cells)?;
        }
        host::emit_done(out.rows.len() as i64)?;
        return Ok(Json(HandleResult::done()));
    }
    let now = iso8601_utc(host::now_ns()?);
    host::emit_result(&["now"])?;
    host::emit_row(1, vec![("now".to_string(), Value::Text(now))])?;
    host::emit_done(1)?;
    Ok(Json(HandleResult::done()))
}

/// Nanoseconds since the Unix epoch -> "YYYY-MM-DDThh:mm:ss.nnnnnnnnnZ".
fn iso8601_utc(ns: i64) -> String {
    let secs = ns.div_euclid(1_000_000_000);
    let nanos = ns.rem_euclid(1_000_000_000);
    let (y, m, d) = civil_from_days(secs.div_euclid(86_400));
    let sod = secs.rem_euclid(86_400);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{nanos:09}Z",
        sod / 3600,
        sod % 3600 / 60,
        sod % 60
    )
}

/// Days since 1970-01-01 -> (year, month, day). Hinnant's civil_from_days.
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}
