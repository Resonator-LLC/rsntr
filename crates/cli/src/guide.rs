//! `rsntr guide`: the agent operating manual, compiled into the binary
//! so a bare `rsntr` install is self-explanatory — no docs checkout, no
//! network. The text is docs/agents.md verbatim (one source of truth);
//! topics address its `## ` sections.

/// The manual, embedded at build time.
pub const GUIDE: &str = include_str!("../../../docs/agents.md");

/// One `## ` section of the manual.
#[derive(Debug)]
pub struct Section {
    /// The heading line, without the `## ` marker.
    pub title: String,
    /// The section body, heading included.
    pub text: String,
}

/// Splits the manual into its intro (everything before the first `## `)
/// and sections.
pub fn sections() -> (String, Vec<Section>) {
    let mut intro = String::new();
    let mut out: Vec<Section> = Vec::new();
    for line in GUIDE.lines() {
        if let Some(title) = line.strip_prefix("## ") {
            out.push(Section {
                title: title.to_string(),
                text: format!("{line}\n"),
            });
        } else if let Some(cur) = out.last_mut() {
            cur.text.push_str(line);
            cur.text.push('\n');
        } else {
            intro.push_str(line);
            intro.push('\n');
        }
    }
    (intro, out)
}

/// The topic key of a section: its heading lowered, with the leading
/// numbering stripped ("4. Hooks: waking an idle agent" -> "hooks:
/// waking an idle agent").
fn key(title: &str) -> String {
    title
        .trim_start_matches(|c: char| c.is_ascii_digit() || c == '.' || c == ' ')
        .to_lowercase()
}

/// Sections matching `topic` (case-insensitive substring of the
/// heading). "intro" and "exit" match the preamble.
pub fn lookup(topic: &str) -> Option<String> {
    let want = topic.to_lowercase();
    let (intro, secs) = sections();
    if "intro".contains(&want) || want.contains("exit") || want.contains("json") {
        return Some(intro);
    }
    let hits: Vec<&Section> = secs
        .iter()
        .filter(|s| key(&s.title).contains(&want))
        .collect();
    if hits.is_empty() {
        return None;
    }
    Some(
        hits.iter()
            .map(|s| s.text.as_str())
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

/// The topic list shown for a miss and under `--json`.
pub fn topics() -> Vec<String> {
    let (_intro, secs) = sections();
    let mut out = vec!["intro (json contract, exit codes)".to_string()];
    out.extend(secs.iter().map(|s| key(&s.title)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guide_embeds_the_manual_with_all_topics() {
        let (intro, secs) = sections();
        assert!(intro.contains("exit codes"), "intro carries the contract");
        assert!(secs.len() >= 7, "got {} sections", secs.len());
        for topic in [
            "lifecycle",
            "pairing",
            "chat",
            "hooks",
            "rdf",
            "pipe",
            "security",
        ] {
            let text = lookup(topic).unwrap_or_else(|| panic!("topic {topic} missing"));
            assert!(!text.is_empty());
        }
    }

    #[test]
    fn lookup_misses_report_none_and_topics_list() {
        assert!(lookup("no-such-topic").is_none());
        assert!(topics().len() >= 8);
    }

    #[test]
    fn key_strips_numbering() {
        assert_eq!(
            key("4. Hooks: waking an idle agent"),
            "hooks: waking an idle agent"
        );
    }
}
