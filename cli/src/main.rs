//! The agent door: `inspect` reads state out, `dispatch` steps commands headlessly.
//!
//! ```text
//!   gitten inspect rows [REPO] [REVSPEC] [--json]
//!   gitten inspect meta [REPO] [LIMIT] [--json]
//!   gitten inspect keys [MODE] [--json]
//!   gitten inspect themes|wraps|layouts|config [--json]
//!   gitten inspect graph [REPO] [LIMIT] [--json]
//!   gitten dispatch [VIEW] [REPO] [ARG] --run cmd,cmd,... [--height N] [--json]
//! ```
//!
//! Everything above drawing is shared: `gitten.toml` via
//! [`gitten_app::config`], acquisition via [`gitten_app::acquire`] over a
//! [`gitten_git`] handle, and the same [`gitten_core`] projections the window
//! and the terminal read. What is here is argument shapes and output shapes.

mod dispatch;
mod inspect;
mod json;

use dispatch::{DispView, Harness};
use gitten_app::acquire::{acquire, Data};
use gitten_app::cli::Source;
use gitten_core::host::Host;
use inspect::{format_config, format_graph, format_keys, format_meta, format_names, format_rows};

fn usage() -> String {
    "\
gitten — the agent door: state out, commands stepped headlessly

  gitten inspect rows [REPO] [REVSPEC] [--json]   the diff as counts and file lines
  gitten inspect meta [REPO] [LIMIT] [--json]     the repository and the host behind it
  gitten inspect keys [MODE] [--json]             what each key runs, now
  gitten inspect themes|wraps|layouts [--json]    the registries and what is selected
  gitten inspect config [--json]                  gitten.toml as it reads back
  gitten inspect graph [REPO] [LIMIT] [--json]    commits with lanes, hues and merges
  gitten dispatch [VIEW] [REPO] [ARG] --run cmd,cmd,... [--height N] [--json]
                                                  step named commands through a viewport

  VIEW is diff or commits (default commits). Each --run entry is a command name
  (view.down) or a key spelling (j, down, ctrl-d); unknown names are reported,
  never run. Write verbs are always refused.
"
    .to_string()
}

/// `gitten.toml` into a host, warnings to stderr as the shared startup prints them.
fn load_host() -> (Host, std::path::PathBuf) {
    let path = gitten_app::config::path();
    let mut host = Host::new();
    for w in gitten_app::config::load(&mut host, &path) {
        eprintln!("gitten: {w}");
    }
    (host, path)
}

fn repo_source(req_repo: &std::path::Path, arg: &str) -> Source {
    Source::Repo {
        path: req_repo.to_path_buf(),
        arg: arg.to_string(),
    }
}

fn run_inspect(req: &inspect::InspectReq) -> Result<String, String> {
    let (host, _) = load_host();
    match req.topic {
        inspect::Topic::Rows => {
            let source = repo_source(&req.repo, &req.arg);
            let handle = gitten_git::open(&req.repo);
            let loaded = acquire(
                gitten_app::cli::View::Diff,
                &source,
                &host,
                Some(handle.as_ref()),
            )
            .map_err(|e| format!("gitten: {e}"))?;
            let Data::Diff(diffs) = loaded.data else {
                return Err("gitten: a diff view loads files".to_string());
            };
            let (built, _) = inspect::build_rows(&host, loaded.label, &diffs);
            Ok(format_rows(&built, req.json))
        }
        inspect::Topic::Meta | inspect::Topic::Graph => {
            let source = repo_source(&req.repo, &req.arg);
            let handle = gitten_git::open(&req.repo);
            let loaded = acquire(
                gitten_app::cli::View::Commits,
                &source,
                &host,
                Some(handle.as_ref()),
            )
            .map_err(|e| format!("gitten: {e}"))?;
            let Data::Commits(commits) = loaded.data else {
                return Err("gitten: a commits view loads commits".to_string());
            };
            match req.topic {
                inspect::Topic::Meta => Ok(format_meta(&loaded.label, &commits, &host, req.json)),
                _ => Ok(format_graph(&loaded.label, &commits, req.json)),
            }
        }
        inspect::Topic::Keys => Ok(format_keys(&host, req.mode.as_deref(), req.json)),
        inspect::Topic::Themes => {
            let names: Vec<String> = host.themes.names().into_iter().map(String::from).collect();
            Ok(format_names(
                "themes",
                &names,
                &host.theme.name.clone(),
                None,
                req.json,
            ))
        }
        inspect::Topic::Wraps => {
            let names: Vec<String> = host.wrap.names().into_iter().map(String::from).collect();
            Ok(format_names(
                "wraps",
                &names,
                host.wrap.selected(),
                None,
                req.json,
            ))
        }
        inspect::Topic::Layouts => {
            let names = vec!["unified".to_string(), "split".to_string()];
            Ok(format_names(
                "layouts",
                &names,
                &host.layout,
                Some("the registry is frontend-owned; every client agrees on these names"),
                req.json,
            ))
        }
        inspect::Topic::Config => Ok(format_config(&host, req.json)),
    }
}

fn run_dispatch(req: &dispatch::DispatchReq) -> Result<String, String> {
    let (host, _) = load_host();
    let source = repo_source(&req.repo, &req.arg);
    let handle = gitten_git::open(&req.repo);
    let view = match req.view {
        DispView::Diff => gitten_app::cli::View::Diff,
        DispView::Commits => gitten_app::cli::View::Commits,
    };
    let loaded =
        acquire(view, &source, &host, Some(handle.as_ref())).map_err(|e| format!("gitten: {e}"))?;
    let mut harness = match (&loaded.data, req.view) {
        (Data::Diff(diffs), DispView::Diff) => {
            let (built_rows, flat) = inspect::build_rows(&host, loaded.label.clone(), diffs);
            let _ = built_rows;
            Harness::for_diff(host, flat, req.height)
        }
        (Data::Commits(commits), DispView::Commits) => {
            Harness::for_commits(host, commits, req.height)
        }
        _ => return Err("gitten: view and data disagree".to_string()),
    };
    let steps: Vec<dispatch::StepOut> = req
        .cmds
        .iter()
        .enumerate()
        .map(|(i, cmd)| harness.step(i + 1, cmd))
        .collect();
    if req.json {
        let items: Vec<String> = steps
            .iter()
            .map(|s| {
                format!(
                    "{{\n      {},\n      {},\n      {},\n      {},\n      {},\n      {},\n      {}\n    }}",
                    json::num_field("step", s.step),
                    json::str_field("input", &s.input),
                    json::str_field("command", &s.command),
                    json::num_field("cursor", s.cursor),
                    json::num_field("top", s.top),
                    json::str_field("selection", &s.selection),
                    json::str_field("status", &s.status)
                )
            })
            .collect();
        return Ok(format!(
            "{{\n  {},\n  {},\n  {},\n  {},\n  {},\n  \"steps\": [\n    {}\n  ]\n}}",
            json::str_field("schema", inspect::SCHEMA),
            json::str_field("kind", "dispatch"),
            json::str_field("view", req.view.name()),
            json::str_field("label", &loaded.label),
            json::num_field("rows", harness.len()),
            items.join(",\n    ")
        ));
    }
    let mut out = String::new();
    for s in &steps {
        let what = match s.command.is_empty() {
            true => s.input.clone(),
            false => s.command.clone(),
        };
        out.push_str(&format!(
            "{} {what} cursor={} top={} [{}] {}\n",
            s.step, s.cursor, s.top, s.selection, s.status
        ));
    }
    Ok(out)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "-h" || a == "--help") || args.is_empty() {
        print!("{}", usage());
        return;
    }
    let (verb, tail) = args.split_at(1);
    let result = match verb[0].as_str() {
        "inspect" => inspect::parse_inspect(tail).and_then(|req| run_inspect(&req)),
        "dispatch" => dispatch::parse_dispatch(tail).and_then(|req| run_dispatch(&req)),
        other => Err(format!(
            "gitten: {other:?} is not a door — want `inspect` or `dispatch`\n\n{}",
            usage()
        )),
    };
    match result {
        Ok(text) => print!("{text}"),
        Err(e) => {
            eprint!("{e}\n\n{}", usage());
            std::process::exit(1);
        }
    }
}
