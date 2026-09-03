//! `inspect`: state out as text or JSON.
//!
//! Every topic reuses the projection its clients already read — [`Flat::report`]
//! for rows, [`Document::report`](gitten_core::markdown::Document::report) for
//! the rendered-Markdown half, [`Keymap::help`](gitten_core::command::Keymap::help)
//! and `live_keys_for` for keys, `Themes::names`, `Wraps::names`,
//! `Differs::selected` for the registries — so an agent sees the same answer a
//! status line or a picker would. Layouts are the deliberate exception: the
//! registry is frontend-owned, so `inspect layouts` reports the configured name
//! plus the names every client agrees on, and says so.

use crate::json::{bool_field, num_field, str_field, str_list};
use gitten_core::command::{HelpRow, Modes};
use gitten_core::host::Host;
use gitten_core::markdown::Document;
use gitten_core::prepared::{prepare, File};
use gitten_core::rows::{Flat, Present};
use gitten_core::{assign_lanes, Commit};

/// Machine envelope every `--json` answer arrives in.
pub const SCHEMA: &str = "gitten.inspect/1";

/// The eight topics `inspect` answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Topic {
    Rows,
    Meta,
    Keys,
    Themes,
    Wraps,
    Layouts,
    Config,
    Graph,
}

impl Topic {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "rows" => Topic::Rows,
            "meta" => Topic::Meta,
            "keys" => Topic::Keys,
            "themes" => Topic::Themes,
            "wraps" => Topic::Wraps,
            "layouts" => Topic::Layouts,
            "config" => Topic::Config,
            "graph" => Topic::Graph,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Topic::Rows => "rows",
            Topic::Meta => "meta",
            Topic::Keys => "keys",
            Topic::Themes => "themes",
            Topic::Wraps => "wraps",
            Topic::Layouts => "layouts",
            Topic::Config => "config",
            Topic::Graph => "graph",
        }
    }
}

/// What `inspect TOPIC [REPO] [ARG]` was asked, parsed from the command line.
///
/// `keys` reads its positional as a mode rather than a repository — it needs no
/// data, only the keymap — and every other topic reads `[REPO] [ARG]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InspectReq {
    pub topic: Topic,
    pub repo: std::path::PathBuf,
    pub arg: String,
    pub mode: Option<String>,
    pub json: bool,
}

pub fn parse_inspect(args: &[String]) -> Result<InspectReq, String> {
    let mut rest = args.to_vec();
    let json = gitten_app::cli::take_switch(&mut rest, "--json");
    let topic = rest.first().and_then(|s| Topic::parse(s)).ok_or_else(|| {
        "inspect wants a topic: rows, meta, keys, themes, wraps, layouts, config, graph".to_string()
    })?;
    let positional = rest[1..].to_vec();
    if topic == Topic::Keys {
        if positional.len() > 1 {
            return Err("inspect keys wants at most one mode".to_string());
        }
        return Ok(InspectReq {
            topic,
            repo: std::path::PathBuf::from("."),
            arg: String::new(),
            mode: positional.into_iter().next(),
            json,
        });
    }
    if matches!(
        topic,
        Topic::Themes | Topic::Wraps | Topic::Layouts | Topic::Config
    ) && !positional.is_empty()
    {
        return Err(format!(
            "inspect {} takes no repository — it describes the configuration",
            topic.name()
        ));
    }
    if positional.len() > 2 {
        return Err(format!(
            "inspect {} wants [REPO] [ARG], got: {}",
            topic.name(),
            positional.join(" ")
        ));
    }
    let mut positional = positional;
    let arg = match positional.len() {
        2 => positional.pop().unwrap_or_default(),
        _ => String::new(),
    };
    let repo = positional
        .pop()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    Ok(InspectReq {
        topic,
        repo,
        arg,
        mode: None,
        json,
    })
}

/// The half of a presentation that does not draw: claims every file into one
/// [`Flat`], exactly as the unified views do before they draw it.
#[derive(Default)]
pub struct FlatPresent {
    pub flat: Flat,
}

impl Present for FlatPresent {
    fn claims(&self, _path: &str) -> bool {
        true
    }

    fn len(&self) -> usize {
        self.flat.len()
    }

    fn build(&mut self, file: File) {
        self.flat.push(file);
    }
}

/// A diff assembled into a flat table plus, when a `.md` file is in it, the
/// rendered-Markdown report over the same prepared files.
///
/// Returns the flat table beside the counts, so `dispatch` steps the same rows
/// `inspect rows` reports rather than preparing the diff twice.
pub struct RowsBuilt {
    pub label: String,
    pub files: usize,
    pub rows: usize,
    pub file_headers: usize,
    pub hunk_headers: usize,
    pub lines: usize,
    pub moved: usize,
    pub report: String,
    pub markdown: String,
    pub entries: Vec<(String, usize, usize, usize)>,
}

pub fn build_rows(
    host: &Host,
    label: String,
    diffs: &[gitten_core::FileDiff],
) -> (RowsBuilt, Flat) {
    let prepared = prepare(diffs, &host.syntax, gitten_app::MAX_LINE_CHARS);
    let want_doc = diffs.iter().any(|f| f.path.ends_with(".md"));
    let mut doc = Document::default();
    let mut present = FlatPresent::default();
    for f in prepared.files {
        if want_doc {
            doc.push(f.clone());
        }
        present.build(f);
    }
    let flat = present.flat;
    let (mut file_headers, mut hunk_headers, mut lines) = (0, 0, 0);
    for row in flat.rows() {
        match row {
            gitten_core::rows::Row::File { .. } => file_headers += 1,
            gitten_core::rows::Row::Hunk(_) => hunk_headers += 1,
            gitten_core::rows::Row::Line(_) => lines += 1,
        }
    }
    let built = RowsBuilt {
        label,
        files: flat.files().len(),
        rows: flat.len(),
        file_headers,
        hunk_headers,
        lines,
        moved: flat.moved(),
        report: flat.report(),
        markdown: doc.report(),
        entries: flat
            .files()
            .iter()
            .map(|e| (e.path.clone(), e.adds, e.dels, e.row))
            .collect(),
    };
    (built, flat)
}

/// Which modes `inspect keys` projects: the one asked for over global, or every
/// mode the keymap binds in first-seen order.
pub fn key_modes(host: &Host, mode: Option<&str>) -> Vec<String> {
    match mode {
        Some(m) => vec![gitten_core::command::GLOBAL.to_string(), m.to_string()],
        None => {
            let mut modes = vec![gitten_core::command::GLOBAL.to_string()];
            for b in host.keys.bindings() {
                if !modes.contains(&b.mode) {
                    modes.push(b.mode.clone());
                }
            }
            modes
        }
    }
}

fn modes_for(host: &Host, mode: Option<&str>) -> Modes {
    let mut modes = Modes::new();
    for m in key_modes(host, mode) {
        if m != gitten_core::command::GLOBAL {
            modes.push(m);
        }
    }
    modes
}

fn help_text(host: &Host, mode: Option<&str>) -> String {
    let mut out = String::new();
    for row in host.keys.help(&host.commands, &modes_for(host, mode)) {
        match row {
            HelpRow::Mode(m) => out.push_str(&format!("[{m}]\n")),
            HelpRow::Command { name, keys, doc } => {
                out.push_str(&format!("  {keys:>22}  {name:<24}  {doc}\n"));
            }
            HelpRow::Blank => out.push('\n'),
        }
    }
    out
}

fn help_json(host: &Host, mode: Option<&str>) -> String {
    let mut items = Vec::new();
    let mut current = String::new();
    for row in host.keys.help(&host.commands, &modes_for(host, mode)) {
        match row {
            HelpRow::Mode(m) => current = m,
            HelpRow::Command { name, keys, doc } => items.push(format!(
                "{{\n      {},\n      {},\n      {},\n      {}\n    }}",
                str_field("mode", &current),
                str_field("name", &name),
                str_field("keys", &keys),
                str_field("doc", &doc)
            )),
            HelpRow::Blank => {}
        }
    }
    format!("[\n    {}\n  ]", items.join(",\n    "))
}

/// Formats the `rows` answer in both shapes.
pub fn format_rows(built: &RowsBuilt, json: bool) -> String {
    if json {
        let entries: Vec<String> = built
            .entries
            .iter()
            .map(|(path, adds, dels, row)| {
                format!(
                    "{{\n      {},\n      {},\n      {},\n      {}\n    }}",
                    str_field("path", path),
                    num_field("adds", *adds),
                    num_field("dels", *dels),
                    num_field("row", *row)
                )
            })
            .collect();
        return format!(
            "{{\n  {},\n  {},\n  {},\n  {},\n  {},\n  {},\n  {},\n  {},\n  {},\n  {},\n  {},\n  \"files\": [\n    {}\n  ]\n}}",
            str_field("schema", SCHEMA),
            str_field("kind", "rows"),
            str_field("label", &built.label),
            num_field("files", built.files),
            num_field("rows", built.rows),
            num_field("file_headers", built.file_headers),
            num_field("hunk_headers", built.hunk_headers),
            num_field("lines", built.lines),
            str_field("report", &built.report),
            str_field("markdown", &built.markdown),
            num_field("moved", built.moved),
            entries.join(",\n    ")
        );
    }
    let mut out = format!(
        "{} files · {} rows ({} file, {} hunk, {} line)",
        built.files, built.rows, built.file_headers, built.hunk_headers, built.lines
    );
    if !built.report.is_empty() {
        out.push_str(&format!(" · {}", built.report));
    }
    if !built.markdown.is_empty() {
        out.push_str(&format!(" · {}", built.markdown));
    }
    out.push('\n');
    for (path, adds, dels, _) in &built.entries {
        out.push_str(&format!("{path} +{adds} -{dels}\n"));
    }
    out
}

/// Formats the `meta` answer: which repository, how much history, which host.
pub fn format_meta(label: &str, commits: &[Commit], host: &Host, json: bool) -> String {
    if json {
        let newest = commits.first().map(|c| {
            format!(
                "{{\n      {},\n      {},\n      {},\n      {}\n    }}",
                str_field("sha", &c.sha),
                str_field("short", &c.short),
                str_field("subject", &c.subject),
                str_field("author", &c.author)
            )
        });
        return format!(
            "{{\n  {},\n  {},\n  {},\n  {},\n  \"newest\": {},\n  {},\n  {},\n  {},\n  {},\n  {}\n}}",
            str_field("schema", SCHEMA),
            str_field("kind", "meta"),
            str_field("label", label),
            num_field("commits", commits.len()),
            newest.unwrap_or_else(|| "null".to_string()),
            str_field("differ", host.differ.selected()),
            num_field("context", host.differ.context),
            str_field("wrap", host.wrap.selected()),
            str_field("layout", &host.layout),
            str_field("theme", &host.theme.name)
        );
    }
    let newest = commits
        .first()
        .map(|c| format!("{} {}", c.short, c.subject))
        .unwrap_or_else(|| "(no commits)".to_string());
    format!(
        "{label}\n{} commits · newest {newest} · {} · wrap {} · layout {} · theme {}\n",
        commits.len(),
        host.differ.selected(),
        host.wrap.selected(),
        host.layout,
        host.theme.name
    )
}

/// Formats the `keys` answer from the shared help projection.
pub fn format_keys(host: &Host, mode: Option<&str>, json: bool) -> String {
    if json {
        return format!(
            "{{\n  {},\n  {},\n  {},\n  \"rows\": {}\n}}",
            str_field("schema", SCHEMA),
            str_field("kind", "keys"),
            str_field("mode", mode.unwrap_or("(all)")),
            help_json(host, mode)
        );
    }
    help_text(host, mode)
}

/// Formats a name registry: the active name starred for humans.
pub fn format_names(
    kind: &str,
    names: &[String],
    active: &str,
    extra: Option<&str>,
    json: bool,
) -> String {
    if json {
        let mut out = format!(
            "{{\n  {},\n  {},\n  {},\n  \"names\": {}",
            str_field("schema", SCHEMA),
            str_field("kind", kind),
            str_field("active", active),
            str_list(names)
        );
        if let Some(note) = extra {
            out.push_str(&format!(",\n  {}", str_field("note", note)));
        }
        out.push_str("\n}");
        return out;
    }
    let mut out = String::new();
    for name in names {
        match name == active {
            true => out.push_str(&format!("* {name}\n")),
            false => out.push_str(&format!("  {name}\n")),
        }
    }
    if let Some(note) = extra {
        out.push_str(note);
        out.push('\n');
    }
    out
}

/// Formats the `graph` answer: the honest lane count and one plan row per commit.
pub fn format_graph(label: &str, commits: &[Commit], json: bool) -> String {
    let rows = assign_lanes(commits);
    let plan = gitten_core::graph::plan(commits, &rows);
    let lanes = gitten_core::graph::lane_count(&rows);
    let merges = commits.iter().filter(|c| c.parents.len() > 1).count();
    let capped = plan.iter().filter(|d| d.capped).count();
    if json {
        let items: Vec<String> = commits
            .iter()
            .zip(plan.iter())
            .map(|(c, d)| {
                format!(
                    "{{\n      {},\n      {},\n      {},\n      {},\n      {},\n      {}\n    }}",
                    str_field("sha", &c.short),
                    num_field("lane", d.lane as usize),
                    num_field("hue", d.hue as usize),
                    bool_field("merge", d.merge),
                    num_field("lanes", d.lanes),
                    bool_field("capped", d.capped)
                )
            })
            .collect();
        return format!(
            "{{\n  {},\n  {},\n  {},\n  {},\n  {},\n  {},\n  {},\n  \"rows\": [\n    {}\n  ]\n}}",
            str_field("schema", SCHEMA),
            str_field("kind", "graph"),
            str_field("label", label),
            num_field("commits", commits.len()),
            num_field("lanes", lanes),
            num_field("merges", merges),
            num_field("capped", capped),
            items.join(",\n    ")
        );
    }
    let mut out = format!(
        "{label}\n{} commits · {} lanes (drawn ≤ {}) · {} merges · {} capped rows\n",
        commits.len(),
        lanes,
        gitten_core::graph::MAX_LANES,
        merges,
        capped
    );
    for (c, d) in commits.iter().zip(plan.iter()).take(20) {
        out.push_str(&format!(
            "  lane {:>2} hue {} {} {}\n",
            d.lane, d.hue, c.short, c.subject
        ));
    }
    if commits.len() > 20 {
        out.push_str(&format!("  … {} more (use --json)\n", commits.len() - 20));
    }
    out
}

/// Formats the `config` answer: the same TOML `gitten config` prints.
pub fn format_config(host: &Host, json: bool) -> String {
    let toml = gitten_app::config::dump(host);
    if json {
        return format!(
            "{{\n  {},\n  {},\n  {},\n  {},\n  {},\n  {},\n  {},\n  {},\n  {}\n}}",
            str_field("schema", SCHEMA),
            str_field("kind", "config"),
            str_field("differ", host.differ.selected()),
            num_field("context", host.differ.context),
            str_field("wrap", host.wrap.selected()),
            str_field("layout", &host.layout),
            str_field("theme", &host.theme.name),
            str_field("font", &format!("{} {}", host.font.family, host.font.size)),
            str_field("toml", &toml)
        );
    }
    toml
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(line: &str) -> Vec<String> {
        line.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn every_topic_parses_with_defaults() {
        let req = parse_inspect(&args("rows")).unwrap();
        assert_eq!(req.topic, Topic::Rows);
        assert_eq!(req.repo.to_string_lossy(), ".");
        assert_eq!(req.arg, "");
        assert!(!req.json);
        let req = parse_inspect(&args("graph ~/src 50 --json")).unwrap();
        assert_eq!(req.topic, Topic::Graph);
        assert_eq!(req.repo.to_string_lossy(), "~/src");
        assert_eq!(req.arg, "50");
        assert!(req.json);
        // `keys` reads its positional as a mode, not a repository.
        let req = parse_inspect(&args("keys diff")).unwrap();
        assert_eq!(req.mode.as_deref(), Some("diff"));
        assert!(parse_inspect(&args("bogus")).is_err());
        assert!(parse_inspect(&args("config extra")).is_err());
        assert!(parse_inspect(&args("keys a b")).is_err());
    }

    #[test]
    fn rows_build_counts_and_reports() {
        let host = Host::new();
        let files = gitten_core::parse_unified_diff(
            "diff --git a/a.rs b/a.rs
index 1111111..2222222 100644
--- a/a.rs
+++ b/a.rs
@@ -1,3 +1,3 @@
 fn one() {}
-let x = 1;
+let x = 2;
 fn two() {}
",
        );
        let (built, _) = build_rows(&host, "test".into(), &files);
        assert_eq!(built.files, 1);
        assert_eq!(
            built.rows,
            built.file_headers + built.hunk_headers + built.lines
        );
        assert_eq!(built.lines, 4);
        let text = format_rows(&built, false);
        assert!(text.contains("a.rs +1 -1"), "{text}");
        let machine = format_rows(&built, true);
        assert!(
            machine.contains("\"schema\": \"gitten.inspect/1\""),
            "{machine}"
        );
        assert!(machine.contains("\"kind\": \"rows\""), "{machine}");
    }

    #[test]
    fn keys_project_the_keymap() {
        let host = Host::new();
        let text = format_keys(&host, Some("diff"), false);
        assert!(text.contains("view.down"), "{text}");
        assert!(text.contains("diff.next-file"), "{text}");
        let machine = format_keys(&host, None, true);
        assert!(machine.contains("\"kind\": \"keys\""), "{machine}");
        // Every shipped binding names a command that exists — the projection an
        // agent reads is the same one the help screen draws.
        for b in host.keys.bindings() {
            assert!(host.commands.known(&b.command), "{}", b.command);
        }
    }
}
