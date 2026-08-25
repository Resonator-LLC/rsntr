//! The teletype rendering of a projection, and the invocation builder
//! that turns a chosen point plus field values into a request.
//!
//! This is the client half of the projection contract: the server sends
//! semantics; numbering, prompting, and presentation are decided here.

use std::collections::BTreeMap;

use anyhow::{Result, bail};

use resonator_protocol::{Point, PointField, PointKind, Projection, RequestKind, Value};

/// Renders the numbered teletype menu for one projection.
///
/// ```text
/// b : projection ""
///
///   [1] browse notes
///   [2] add a note        (title, body?)
///   [3] a note was added  (vibrates)
///   [4] admin >
/// ```
pub fn render_projection(title: &str, path: &str, p: &Projection) -> String {
    let mut out = format!("{title} : projection {path:?}\n\n");
    if p.offers.is_empty() {
        out.push_str("  (nothing projected to you)\n");
        return out;
    }
    let labels: Vec<String> = p.offers.iter().map(point_label).collect();
    let width = labels.iter().map(|l| l.chars().count()).max().unwrap_or(0);
    for (i, (point, label)) in p.offers.iter().zip(&labels).enumerate() {
        match point_annotation(point) {
            Some(a) => {
                let pad = " ".repeat(width - label.chars().count());
                out.push_str(&format!("  [{}] {label}{pad}  {a}\n", i + 1));
            }
            None => out.push_str(&format!("  [{}] {label}\n", i + 1)),
        }
    }
    out
}

/// The display label of a point: its rdfs:label, or its IRI as a fallback.
pub fn point_label(point: &Point) -> String {
    match &point.label {
        Some(l) if !l.trim().is_empty() => l.clone(),
        _ => format!("<{}>", point.iri),
    }
}

/// The teletype annotation after a label: `>` for a point that projects
/// deeper, `(vibrates)` for a Sympathetic point, the field list for a
/// point with a coupling, nothing otherwise.
fn point_annotation(point: &Point) -> Option<String> {
    if point.projects.is_some() {
        return Some(">".to_string());
    }
    if point.kind == PointKind::Sympathetic {
        return Some("(vibrates)".to_string());
    }
    if !point.coupling.is_empty() {
        let fields: Vec<String> = point
            .coupling
            .iter()
            .map(|f| {
                if f.required {
                    f.name.clone()
                } else {
                    format!("{}?", f.name)
                }
            })
            .collect();
        return Some(format!("({})", fields.join(", ")));
    }
    None
}

/// What choosing a point means.
#[derive(Debug, PartialEq)]
pub enum Invocation {
    /// Send this request (the envelope binding, positional or template).
    Statement {
        kind: RequestKind,
        modulation: String,
        text: String,
        params: Vec<Value>,
    },
    /// Fetch the deeper projection at this path.
    Zoom { path: String },
    /// A Sympathetic point: subscribe with `rsntr entrain`.
    Entrainable,
}

/// Builds the invocation for a chosen point from raw field values (as
/// typed by the user), validating required fields and enumerations and
/// converting values per the field datatypes.
pub fn build_invocation(point: &Point, values: &BTreeMap<String, String>) -> Result<Invocation> {
    if let Some(path) = &point.projects {
        return Ok(Invocation::Zoom { path: path.clone() });
    }
    if point.kind == PointKind::Sympathetic {
        return Ok(Invocation::Entrainable);
    }
    let kind = match point.kind {
        PointKind::Excitable => RequestKind::Execute,
        _ => RequestKind::Query,
    };

    // Preferred: positional binding (rsntr:signal + rsntr:paramsOrder).
    if let (Some(modulation), Some(text)) = (&point.modulation, &point.signal) {
        let mut params = Vec::new();
        for name in &point.params_order {
            let field = point.coupling.iter().find(|f| &f.name == name);
            match values.get(name) {
                Some(raw) => params.push(convert(field, name, raw)?),
                None => match field {
                    Some(f) if f.required => bail!("missing required field {name:?}"),
                    Some(f) if f.default.is_some() => {
                        params.push(f.default.clone().expect("checked"))
                    }
                    _ => params.push(Value::Null),
                },
            }
        }
        return Ok(Invocation::Statement {
            kind,
            modulation: modulation.clone(),
            text: text.clone(),
            params,
        });
    }

    // Fallback: the {name} template binding.
    if let (Some(modulation), Some(template)) = (&point.modulation, &point.signal_template) {
        let mut text = template.clone();
        for f in &point.coupling {
            let placeholder = format!("{{{}}}", f.name);
            let raw = match values.get(&f.name) {
                Some(v) => v.clone(),
                None if f.required => bail!("missing required field {:?}", f.name),
                None => String::new(),
            };
            text = text.replace(&placeholder, &raw);
        }
        return Ok(Invocation::Statement {
            kind,
            modulation: modulation.clone(),
            text,
            params: Vec::new(),
        });
    }

    if point.fires.is_some() {
        bail!(
            "point <{}> uses the fire binding (antenna-hosted); this client cannot fire URNs",
            point.iri
        );
    }
    bail!("point <{}> carries no invocation binding", point.iri)
}

/// Converts one raw user value per the field's datatype, enforcing an
/// enumeration when the field declares one.
fn convert(field: Option<&PointField>, name: &str, raw: &str) -> Result<Value> {
    let value = match field.and_then(|f| f.datatype.as_deref()) {
        Some(dt) if dt.ends_with("#integer") || dt.ends_with("#long") || dt.ends_with("#int") => {
            Value::Integer(
                raw.trim()
                    .parse()
                    .map_err(|e| anyhow::anyhow!("field {name:?} expects an integer: {e}"))?,
            )
        }
        Some(dt)
            if dt.ends_with("#double") || dt.ends_with("#float") || dt.ends_with("#decimal") =>
        {
            Value::Real(
                raw.trim()
                    .parse()
                    .map_err(|e| anyhow::anyhow!("field {name:?} expects a number: {e}"))?,
            )
        }
        _ => Value::Text(raw.to_string()),
    };
    if let Some(f) = field
        && !f.one_of.is_empty()
        && !f.one_of.contains(&value)
    {
        bail!(
            "field {name:?} must be one of {:?}",
            f.one_of.iter().map(render_choice).collect::<Vec<_>>()
        );
    }
    Ok(value)
}

/// Renders one enumeration choice for prompts and errors.
pub fn render_choice(v: &Value) -> String {
    match v {
        Value::Text(t) => t.clone(),
        Value::Integer(i) => i.to_string(),
        Value::Real(f) => f.to_string(),
        other => format!("{other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point(kind: PointKind) -> Point {
        Point {
            iri: "urn:x:p".into(),
            kind,
            label: Some("do it".into()),
            comment: None,
            icon: None,
            role: None,
            projects: None,
            coupling: Vec::new(),
            modulation: None,
            signal: None,
            params_order: Vec::new(),
            signal_template: None,
            fires: None,
        }
    }

    fn field(name: &str, required: bool) -> PointField {
        PointField {
            name: name.into(),
            datatype: Some("http://www.w3.org/2001/XMLSchema#string".into()),
            required,
            default: None,
            one_of: Vec::new(),
            hint: None,
        }
    }

    #[test]
    fn renders_the_spec_transcript_shape() {
        let mut browse = point(PointKind::Radiant);
        browse.label = Some("browse notes".into());
        let mut add = point(PointKind::Excitable);
        add.label = Some("add a note".into());
        add.coupling = vec![field("title", true), field("body", false)];
        let mut changed = point(PointKind::Sympathetic);
        changed.label = Some("a note was added".into());
        let mut admin = point(PointKind::Bare);
        admin.label = Some("admin".into());
        admin.projects = Some("urn:x:admin".into());

        let p = Projection {
            id: None,
            offers: vec![browse, add, changed, admin],
        };
        let out = render_projection("notes node", "", &p);
        assert_eq!(
            out,
            "notes node : projection \"\"\n\n  \
             [1] browse notes\n  \
             [2] add a note        (title, body?)\n  \
             [3] a note was added  (vibrates)\n  \
             [4] admin             >\n"
        );
    }

    #[test]
    fn positional_binding_builds_params_with_nulls() {
        let mut add = point(PointKind::Excitable);
        add.coupling = vec![field("title", true), field("body", false)];
        add.modulation = Some("sql-sqlite".into());
        add.signal = Some("INSERT INTO notes (title, body) VALUES (?, ?)".into());
        add.params_order = vec!["title".into(), "body".into()];

        let values = BTreeMap::from([("title".to_string(), "groceries".to_string())]);
        let inv = build_invocation(&add, &values).expect("build");
        assert_eq!(
            inv,
            Invocation::Statement {
                kind: RequestKind::Execute,
                modulation: "sql-sqlite".into(),
                text: "INSERT INTO notes (title, body) VALUES (?, ?)".into(),
                params: vec![Value::Text("groceries".into()), Value::Null],
            }
        );

        // A missing required field refuses.
        assert!(build_invocation(&add, &BTreeMap::new()).is_err());
    }

    #[test]
    fn template_binding_substitutes_names() {
        let mut hi = point(PointKind::Radiant);
        hi.coupling = vec![field("mood", true)];
        hi.modulation = Some("sql-sqlite".into());
        hi.signal_template = Some("SELECT '{mood}'".into());

        let values = BTreeMap::from([("mood".to_string(), "calm".to_string())]);
        let inv = build_invocation(&hi, &values).expect("build");
        assert_eq!(
            inv,
            Invocation::Statement {
                kind: RequestKind::Query,
                modulation: "sql-sqlite".into(),
                text: "SELECT 'calm'".into(),
                params: vec![],
            }
        );
    }

    #[test]
    fn zoom_sympathetic_and_fire() {
        let mut zoom = point(PointKind::Bare);
        zoom.projects = Some("urn:x:deep".into());
        assert_eq!(
            build_invocation(&zoom, &BTreeMap::new()).expect("zoom"),
            Invocation::Zoom {
                path: "urn:x:deep".into()
            }
        );

        assert_eq!(
            build_invocation(&point(PointKind::Sympathetic), &BTreeMap::new()).expect("signal"),
            Invocation::Entrainable
        );

        let mut fire = point(PointKind::Excitable);
        fire.fires = Some("urn:msg:hi:{mood}".into());
        assert!(build_invocation(&fire, &BTreeMap::new()).is_err());
    }

    #[test]
    fn enum_and_datatype_conversion() {
        let mut f = field("n", true);
        f.datatype = Some("http://www.w3.org/2001/XMLSchema#integer".into());
        f.one_of = vec![Value::Integer(1), Value::Integer(2)];
        let mut p = point(PointKind::Excitable);
        p.coupling = vec![f];
        p.modulation = Some("sql-sqlite".into());
        p.signal = Some("SELECT ?".into());
        p.params_order = vec!["n".into()];

        let ok = BTreeMap::from([("n".to_string(), "2".to_string())]);
        assert!(build_invocation(&p, &ok).is_ok());
        let bad = BTreeMap::from([("n".to_string(), "3".to_string())]);
        assert!(build_invocation(&p, &bad).is_err());
        let not_int = BTreeMap::from([("n".to_string(), "two".to_string())]);
        assert!(build_invocation(&p, &not_int).is_err());
    }
}
