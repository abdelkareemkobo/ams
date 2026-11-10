use std::{
    io::{self, BufRead, Write},
    path::{Path, PathBuf},
    process::Command,
};

use ams::{
    adapters,
    catalog::{self, Catalog, Entry},
    model::Format,
};
use anyhow::{Context, Result, bail};
use clap::{CommandFactory, Parser, Subcommand};
use rayon::prelude::*;
use serde_json::{Value, json};

mod display;
mod picker;

const DEFAULT_MODEL: &str = "minishlab/potion-base-8M";

#[derive(Parser)]
#[command(
    version,
    about = "أمس · Find and resume yesterday's coding-agent sessions",
    after_help = "Find = literal words in titles/history/projects; no model.\nAsk = meaning + words with Model2Vec; returns sessions, not answers.\nPick = live indexed word search; use --semantic for meaning search.\nFirst search builds a private local index. Use --details or show ID for full paths/previews."
)]
struct Cli {
    /// Private SQLite catalog (default: $XDG_STATE_HOME/ams/catalog.sqlite3)
    #[arg(long, env = "AMS_DB", global = true)]
    db: Option<PathBuf>,
    /// Machine-readable output; diagnostics remain on stderr
    #[arg(long, global = true)]
    json: bool,
    /// Show complete locations and previews instead of compact result rows
    #[arg(long, global = true)]
    details: bool,
    /// Parallel scan / batch migration workers (0: available CPUs, capped at 8)
    #[arg(long, default_value_t = 0, global = true)]
    jobs: usize,
    /// Refresh registered/default stores before searching
    #[arg(long, global = true)]
    refresh: bool,
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Incrementally index readable conversations; persist explicitly added roots
    Scan {
        /// Native store root, e.g. omp=/backup/.omp/agent (repeatable)
        #[arg(long = "root", value_parser = parse_root)]
        roots: Vec<(Format, PathBuf)>,
        /// Do not add default stores (previously registered roots remain active)
        #[arg(long)]
        no_defaults: bool,
    },
    /// List registered source roots, or forget a retired root without touching its files
    Roots {
        #[command(subcommand)]
        command: Option<RootCommands>,
    },
    /// Literal words in titles, readable history, projects, and IDs; no model
    #[command(visible_alias = "search", visible_alias = "f")]
    Find {
        query: String,
        #[arg(long, value_enum)]
        format: Option<Format>,
        #[arg(short = 'n', long, default_value_t = 20)]
        limit: usize,
        /// Match session titles only, excluding history and other metadata
        #[arg(long)]
        titles_only: bool,
        /// Browse live word-search results interactively
        #[arg(long, conflicts_with = "titles_only")]
        pick: bool,
    },
    /// List recent indexed sessions
    #[command(visible_alias = "list")]
    Ls {
        #[arg(long, value_enum)]
        format: Option<Format>,
        #[arg(short = 'n', long, default_value_t = 20)]
        limit: usize,
    },
    /// Show indexed location, native identity, status, and bounded preview
    Show { id: String },
    /// Search the full index interactively, with a separate session preview
    Pick {
        query: Option<String>,
        /// Search by meaning (hybrid Model2Vec), without filtering out unlike titles
        #[arg(long)]
        semantic: bool,
        #[arg(long, default_value = DEFAULT_MODEL)]
        model: String,
        #[arg(long, value_enum)]
        format: Option<Format>,
        #[arg(long)]
        resume: bool,
    },
    /// Resume with the original agent in the recorded project directory
    Resume {
        /// Catalog/native ID prefix; omit to select interactively with fzf
        id: Option<String>,
        /// Print a safely quoted command without launching the agent
        #[arg(long)]
        print: bool,
    },
    /// Build optional local Model2Vec vectors (explicit model download opt-in)
    Embed {
        #[arg(long, default_value = DEFAULT_MODEL)]
        model: String,
    },
    /// Meaning + word search using Model2Vec; returns sessions, not generated answers
    Ask {
        query: String,
        #[arg(long, default_value = DEFAULT_MODEL)]
        model: String,
        #[arg(long)]
        semantic_only: bool,
        #[arg(long, value_enum)]
        format: Option<Format>,
        #[arg(short = 'n', long, default_value_t = 20)]
        limit: usize,
        /// Browse live meaning-search results interactively
        #[arg(long, conflicts_with = "semantic_only")]
        pick: bool,
    },
    /// Optional portable migration; source is never modified or deleted
    #[command(visible_alias = "mv")]
    Transfer {
        /// Paths or indexed IDs; '-' reads one source per stdin line
        #[arg(required = true)]
        sources: Vec<String>,
        #[arg(long = "from", value_enum)]
        source_format: Option<Format>,
        #[arg(long, value_enum)]
        to: Format,
        /// Target native agent home (not the user's HOME)
        #[arg(long)]
        home: Option<PathBuf>,
        #[arg(long)]
        cwd: Option<PathBuf>,
        /// Write one portable artifact here instead of installing it
        #[arg(long, conflicts_with = "home")]
        output: Option<PathBuf>,
        /// Parse, convert, and validate without installing
        #[arg(long)]
        dry_run: bool,
        /// Reject every reported loss before writing
        #[arg(long)]
        strict: bool,
    },
    /// Inspect discovery roots, catalog location, and optional features
    Doctor,
    /// Generate completions for power-user shells
    Completions { shell: clap_complete::Shell },
    #[command(name = "__picker-search", hide = true)]
    PickerSearch {
        #[arg(long)]
        query: Option<String>,
        #[arg(long)]
        semantic: bool,
        #[arg(long, default_value = DEFAULT_MODEL)]
        model: String,
        #[arg(long, value_enum)]
        format: Option<Format>,
        #[arg(long)]
        preview: Option<String>,
    },
}

#[derive(Subcommand)]
enum RootCommands {
    /// Forget this root and its indexed content; native sessions are untouched
    Remove {
        #[arg(value_parser = parse_root)]
        root: (Format, PathBuf),
    },
}

fn parse_root(value: &str) -> Result<(Format, PathBuf), String> {
    let (format, path) = value.split_once('=').ok_or("use FORMAT=PATH")?;
    if path.is_empty() {
        return Err("root path is empty".into());
    }
    Ok((
        format.parse().map_err(|e: anyhow::Error| e.to_string())?,
        PathBuf::from(path),
    ))
}

fn db_path(cli: &Cli) -> Result<PathBuf> {
    if let Some(path) = &cli.db {
        return Ok(path.clone());
    }
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".local/state")))
        .context("HOME or XDG_STATE_HOME is required; alternatively pass --db")?;
    Ok(base.join("ams/catalog.sqlite3"))
}

fn workers(requested: usize) -> usize {
    if requested == 0 {
        std::thread::available_parallelism()
            .map(usize::from)
            .unwrap_or(1)
            .min(8)
    } else {
        requested
    }
}

fn open_catalog(cli: &Cli, auto_scan: bool) -> Result<Catalog> {
    let path = db_path(cli)?;
    let fresh = !path.exists();
    let mut db = Catalog::open(&path)?;
    if auto_scan && (cli.refresh || fresh) {
        let report = db.scan(&catalog::default_roots(), workers(cli.jobs))?;
        eprintln!(
            "Indexed {} files ({} changed, {} errors).",
            report.files_seen, report.scanned, report.errors
        );
        for error in report.root_errors {
            eprintln!("ams: {error}");
        }
    }
    Ok(db)
}

fn emit(value: &impl serde::Serialize) -> Result<()> {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    serde_json::to_writer_pretty(&mut out, value)?;
    writeln!(out)?;
    Ok(())
}

// Native titles and paths are untrusted terminal input, including escape sequences.
fn clean(value: &str, width: usize) -> String {
    let mut chars = value.chars().map(|c| if c.is_control() { ' ' } else { c });
    let mut out: String = chars.by_ref().take(width).collect();
    if chars.next().is_some() {
        out.push('…');
    }
    out
}

fn print_entries(entries: &[Entry], json_output: bool, details: bool) -> Result<()> {
    if json_output {
        return emit(&entries);
    }
    display::results(
        entries,
        &mut io::stdout().lock(),
        display::terminal_width(),
        details,
    )?;
    Ok(())
}

fn shell_quote(value: &str) -> String {
    if !value.is_empty()
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_./:-".contains(&b))
    {
        value.to_owned()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

fn parent_before(path: &Path, component: &str) -> Option<PathBuf> {
    path.ancestors()
        .find(|p| p.file_name().is_some_and(|n| n == component))
        .and_then(Path::parent)
        .map(Path::to_path_buf)
}

fn resume_entry(entry: &Entry, print: bool, json_output: bool) -> Result<()> {
    if entry.status != "ok" {
        bail!("cannot resume session with status {}", entry.status);
    }
    let mut env = Vec::<(String, String)>::new();
    let (program, args) = match entry.format {
        Format::Omp => (
            "omp",
            vec!["--resume".into(), entry.path.to_string_lossy().into_owned()],
        ),
        Format::Pi => (
            "pi",
            vec![
                "--session".into(),
                entry.path.to_string_lossy().into_owned(),
            ],
        ),
        Format::Codex => {
            if let Some(home) = parent_before(&entry.path, "sessions")
                .or_else(|| parent_before(&entry.path, "archived_sessions"))
            {
                env.push(("CODEX_HOME".into(), home.to_string_lossy().into_owned()));
            }
            ("codex", vec!["resume".into(), entry.native_id.clone()])
        }
        Format::Claude => {
            if let Some(home) = parent_before(&entry.path, "projects") {
                env.push((
                    "CLAUDE_CONFIG_DIR".into(),
                    home.to_string_lossy().into_owned(),
                ));
            }
            ("claude", vec!["--resume".into(), entry.native_id.clone()])
        }
        Format::Opencode => {
            if entry.path.extension().is_some_and(|e| e == "json") {
                bail!(
                    "this is an exported OpenCode bundle, not an installed session; import it with the matching OpenCode CLI first"
                );
            }
            if let Some(home) = entry.path.parent().and_then(Path::parent) {
                env.push(("XDG_DATA_HOME".into(), home.to_string_lossy().into_owned()));
            }
            (
                "opencode",
                vec!["--session".into(), entry.native_id.clone()],
            )
        }
    };
    let cwd = Path::new(&entry.cwd);
    if print {
        if json_output {
            return emit(&json!({"program": program, "args":args, "cwd":entry.cwd, "env":env}));
        }
        let assignments = env
            .iter()
            .map(|(k, v)| format!("{k}={}", shell_quote(v)))
            .collect::<Vec<_>>()
            .join(" ");
        println!(
            "cd -- {} && {}{}{}",
            shell_quote(&entry.cwd),
            if assignments.is_empty() {
                String::new()
            } else {
                format!("{assignments} ")
            },
            program,
            args.iter()
                .map(|a| format!(" {}", shell_quote(a)))
                .collect::<String>()
        );
        return Ok(());
    }
    if !cwd.is_dir() {
        bail!(
            "recorded project directory no longer exists: {}; use ams resume --print to inspect",
            clean(&entry.cwd, 200)
        );
    }
    let mut cmd = Command::new(program);
    cmd.args(args).envs(env).current_dir(cwd);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        Err(cmd.exec()).with_context(|| format!("launching {program}"))
    }
    #[cfg(not(unix))]
    {
        let status = cmd
            .status()
            .with_context(|| format!("launching {program}"))?;
        if !status.success() {
            bail!("{program} exited {status}");
        }
        Ok(())
    }
}

fn search_picker_entries(
    db: &Catalog,
    query: &str,
    semantic: bool,
    model: &str,
    format: Option<Format>,
) -> Result<Vec<Entry>> {
    if semantic {
        #[cfg(feature = "semantic")]
        {
            // Validate explicit semantic opt-in even before the user types.
            let entries = ams::semantic::search(
                db,
                if query.trim().is_empty() {
                    "session"
                } else {
                    query
                },
                model,
                format,
                if query.trim().is_empty() { 0 } else { 100 },
                true,
            )?;
            return if query.trim().is_empty() {
                db.list(format, 100)
            } else {
                Ok(entries)
            };
        }
        #[cfg(not(feature = "semantic"))]
        {
            let _ = model;
            bail!("this binary was built without semantic support");
        }
    }
    if query.trim().is_empty() {
        db.list(format, 100)
    } else {
        db.find(query, format, 100)
    }
}

fn emit_selection(entry: &Entry, json_output: bool) -> Result<()> {
    if json_output {
        emit(entry)
    } else {
        writeln!(io::stdout().lock(), "{}", entry.id)?;
        Ok(())
    }
}

fn selected_entry(
    cli: &Cli,
    db: &Catalog,
    query: Option<&str>,
    semantic: bool,
    model: &str,
    format: Option<Format>,
) -> Result<Option<Entry>> {
    picker::select(&db_path(cli)?, query, semantic, model, format)?
        .map(|id| db.resolve(&id))
        .transpose()
}

struct Source {
    path: PathBuf,
    format: Option<Format>,
}

struct TransferOptions<'a> {
    target: Format,
    home: Option<&'a Path>,
    cwd: Option<&'a Path>,
    output: Option<&'a Path>,
    dry_run: bool,
    strict: bool,
}

fn target_home(format: Format) -> Result<PathBuf> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME unavailable; pass --home")?;
    Ok(match format {
        Format::Codex => std::env::var_os("CODEX_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".codex")),
        Format::Pi => std::env::var_os("PI_CODING_AGENT_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".pi/agent")),
        Format::Omp => std::env::var_os("PI_CODING_AGENT_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                std::env::var_os("OMP_PROFILE")
                    .map(|profile| home.join(".omp/profiles").join(profile).join("agent"))
                    .unwrap_or_else(|| home.join(".omp/agent"))
            }),
        Format::Claude => bail!("Claude is searchable/resumable, but not a migration target"),
        Format::Opencode => bail!("use --output for an OpenCode public import bundle"),
    })
}

fn transfer(source: &Source, options: &TransferOptions<'_>) -> Result<Value> {
    let session = adapters::load(&source.path, source.format)?;
    let cwd = options
        .cwd
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| session.cwd.clone());
    let rendered = adapters::render(&session, options.target, &cwd)?;
    if options.strict && !rendered.losses.is_empty() {
        bail!(
            "strict migration rejected losses: {}",
            serde_json::to_string(&rendered.losses)?
        );
    }
    adapters::verify(&session, &rendered, options.target)?;
    let output = if let Some(path) = options.output {
        path.to_path_buf()
    } else {
        if options.target == Format::Opencode {
            bail!(
                "OpenCode migration produces a portable bundle: pass --output FILE, then import with the matching OpenCode CLI; native database writes are never attempted"
            );
        }
        let home = match options.home {
            Some(path) => path.to_path_buf(),
            None => target_home(options.target)?,
        };
        home.join(&rendered.relative_path)
    };
    if !options.dry_run {
        let parent = output
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        std::fs::create_dir_all(parent).context("creating native target directory")?;
        let mut temp = tempfile::NamedTempFile::new_in(parent)?;
        temp.write_all(&rendered.bytes)?;
        temp.as_file().sync_all()?;
        temp.persist_noclobber(&output).with_context(|| {
            format!(
                "installing {}; existing files are never overwritten",
                output.display()
            )
        })?;
    }
    Ok(
        json!({"source":source.path, "source_format":session.format, "target":options.target,
        "id":rendered.id, "output":output, "events":session.events.len(), "losses":rendered.losses,
        "bytes":rendered.bytes.len(), "dry_run":options.dry_run,
        "resume_args":rendered.resume_args, "installed":!options.dry_run && options.output.is_none()}),
    )
}

fn run(cli: Cli) -> Result<()> {
    match cli.command.as_ref() {
        Some(Commands::Completions { shell }) => {
            clap_complete::generate(*shell, &mut Cli::command(), "ams", &mut io::stdout());
        }
        Some(Commands::Scan { roots, no_defaults }) => {
            let mut db = open_catalog(&cli, false)?;
            let mut all = if *no_defaults {
                Vec::new()
            } else {
                catalog::default_roots()
            };
            all.extend(roots.clone());
            if *no_defaults && all.is_empty() && db.roots()?.is_empty() {
                bail!("--no-defaults requires at least one --root on a new catalog");
            }
            let report = db.scan(&all, workers(cli.jobs))?;
            let incomplete = report.errors > 0 || !report.root_errors.is_empty();
            if cli.json {
                emit(&report)?;
            } else {
                println!(
                    "{} files · {} indexed · {} unchanged · {} missing · {} errors",
                    report.files_seen,
                    report.scanned,
                    report.unchanged,
                    report.missing,
                    report.errors
                );
                for e in report.root_errors {
                    eprintln!("ams: {e}");
                }
            }
            if incomplete {
                bail!(
                    "scan incomplete; healthy sessions remain searchable, inspect reported errors"
                );
            }
        }
        Some(Commands::Find {
            query,
            format,
            limit,
            titles_only,
            pick,
        }) => {
            let db = open_catalog(&cli, true)?;
            if *pick {
                if let Some(entry) =
                    selected_entry(&cli, &db, Some(query), false, DEFAULT_MODEL, *format)?
                {
                    emit_selection(&entry, cli.json)?;
                }
            } else {
                let entries = if *titles_only {
                    db.find_titles(query, *format, *limit)?
                } else {
                    db.find(query, *format, *limit)?
                };
                print_entries(&entries, cli.json, cli.details)?;
            }
        }
        Some(Commands::Ls { format, limit }) => {
            let db = open_catalog(&cli, true)?;
            print_entries(&db.list(*format, *limit)?, cli.json, cli.details)?;
        }
        None => {
            let db = open_catalog(&cli, true)?;
            print_entries(&db.list(None, 20)?, cli.json, cli.details)?;
        }
        Some(Commands::Show { id }) => {
            let db = open_catalog(&cli, true)?;
            let entry = db.resolve(id)?;
            if cli.json {
                emit(&entry)?;
            } else {
                display::detail(&entry, &mut io::stdout().lock(), display::terminal_width())?;
            }
        }
        Some(Commands::Pick {
            query,
            semantic,
            model,
            format,
            resume,
        }) => {
            let db = open_catalog(&cli, true)?;
            if let Some(entry) =
                selected_entry(&cli, &db, query.as_deref(), *semantic, model, *format)?
            {
                if *resume {
                    resume_entry(&entry, false, cli.json)?;
                } else {
                    emit_selection(&entry, cli.json)?;
                }
            }
        }
        Some(Commands::Resume { id, print }) => {
            let db = open_catalog(&cli, true)?;
            if let Some(id) = id {
                resume_entry(&db.resolve(id)?, *print, cli.json)?;
            } else {
                if let Some(entry) = selected_entry(&cli, &db, None, false, DEFAULT_MODEL, None)? {
                    resume_entry(&entry, *print, cli.json)?;
                }
            }
        }
        Some(Commands::Embed { model }) => {
            let mut db = open_catalog(&cli, true)?;
            #[cfg(feature = "semantic")]
            emit(&ams::semantic::index(&mut db, model)?)?;
            #[cfg(not(feature = "semantic"))]
            {
                let _ = (&mut db, model);
                bail!(
                    "this binary was built without semantic support; rebuild without --no-default-features"
                );
            }
        }
        Some(Commands::Ask {
            query,
            model,
            semantic_only,
            format,
            limit,
            pick,
        }) => {
            let db = open_catalog(&cli, true)?;
            if *pick {
                if let Some(entry) = selected_entry(&cli, &db, Some(query), true, model, *format)? {
                    emit_selection(&entry, cli.json)?;
                }
            } else {
                #[cfg(feature = "semantic")]
                print_entries(
                    &ams::semantic::search(&db, query, model, *format, *limit, !semantic_only)?,
                    cli.json,
                    cli.details,
                )?;
                #[cfg(not(feature = "semantic"))]
                {
                    let _ = (db, query, model, semantic_only, format, limit);
                    bail!("this binary was built without semantic support");
                }
            }
        }
        Some(Commands::PickerSearch {
            query,
            semantic,
            model,
            format,
            preview,
        }) => {
            let path = db_path(&cli)?;
            if !path.is_file() {
                bail!("picker catalog is unavailable; run ams scan first");
            }
            let db = Catalog::open(&path)?;
            let entries = search_picker_entries(
                &db,
                query.as_deref().unwrap_or(""),
                *semantic,
                model,
                *format,
            )?;
            let width = display::terminal_width();
            let mut out = io::stdout().lock();
            if let Some(id) = preview {
                let entry = match entries.into_iter().find(|e| e.id == *id) {
                    Some(entry) => entry,
                    None => db.resolve(id)?,
                };
                display::detail(&entry, &mut out, width)?;
            } else {
                for entry in entries {
                    writeln!(out, "{}\t{}", entry.id, display::picker_row(&entry, width))?;
                }
            }
        }
        Some(Commands::Transfer {
            sources,
            source_format,
            to,
            home,
            cwd,
            output,
            dry_run,
            strict,
        }) => {
            let mut expanded = Vec::new();
            for source in sources {
                if source == "-" {
                    for line in io::stdin().lock().lines() {
                        let line = line?;
                        if !line.trim().is_empty() {
                            expanded.push(line);
                        }
                    }
                } else {
                    expanded.push(source.clone());
                }
            }
            if expanded.is_empty() {
                bail!("no sources supplied");
            }
            if output.is_some() && expanded.len() != 1 {
                bail!("--output requires exactly one source");
            }
            let mut db = None;
            let resolved = expanded.iter().map(|source| -> Result<Source> {
                let path = PathBuf::from(source);
                if path.is_file() { Ok(Source {path,format:*source_format}) } else {
                    if db.is_none() { db = Some(open_catalog(&cli,true)?); }
                    let e = db.as_ref().unwrap().resolve(source)?;
                    if e.status != "ok" { bail!("cannot migrate session with status {}", e.status); }
                    if e.format == Format::Opencode && e.path.extension().is_some_and(|x| x == "db" || x == "sqlite") { bail!("live OpenCode migration requires an export from its matching CLI; search/resume work directly, but transfer accepts exported JSON bundles"); }
                    if source_format.is_some_and(|f| f != e.format) { bail!("--from disagrees with catalog format"); }
                    Ok(Source {path:e.path,format:Some(e.format)})
                }
            }).collect::<Result<Vec<_>>>()?;
            let options = TransferOptions {
                target: *to,
                home: home.as_deref(),
                cwd: cwd.as_deref(),
                output: output.as_deref(),
                dry_run: *dry_run,
                strict: *strict,
            };
            let results: Vec<_> = if resolved.len() == 1 {
                vec![transfer(&resolved[0], &options)]
            } else {
                let pool = rayon::ThreadPoolBuilder::new()
                    .num_threads(workers(cli.jobs))
                    .build()?;
                pool.install(|| resolved.par_iter().map(|s| transfer(s, &options)).collect())
            };
            let mut failed = false;
            let values: Vec<_> = results
                .into_iter()
                .zip(expanded)
                .map(|(r, source)| match r {
                    Ok(v) => v,
                    Err(e) => {
                        failed = true;
                        json!({"source":source,"error":format!("{e:#}")})
                    }
                })
                .collect();
            if values.len() == 1 {
                emit(&values[0])?;
            } else {
                emit(&values)?;
            }
            if failed {
                bail!("one or more migrations failed; successful sources are reported above");
            }
        }
        Some(Commands::Roots { command }) => {
            let mut db = open_catalog(&cli, false)?;
            match command {
                None => emit(&db.roots()?)?,
                Some(RootCommands::Remove {
                    root: (format, path),
                }) => {
                    let removed = db.remove_root(*format, path)?;
                    emit(
                        &json!({"format":format,"path":path,"removed_sessions":removed,"native_files_changed":false}),
                    )?;
                }
            }
        }
        Some(Commands::Doctor) => {
            let path = db_path(&cli)?;
            let registered = if path.exists() {
                Catalog::open(&path)?.roots()?
            } else {
                Vec::new()
            };
            let roots: Vec<_> = catalog::default_roots()
                .iter()
                .map(|(f, p)| json!({"format":f,"path":p,"exists":p.exists()}))
                .collect();
            emit(
                &json!({"version":env!("CARGO_PKG_VERSION"),"catalog":path,"semantic_compiled":cfg!(feature="semantic"),"model_downloads":"only explicit embed/ask","indexed_content":"readable user/assistant text; excludes tool outputs and private thinking","default_roots":roots,"registered_roots":registered,"workers":workers(cli.jobs)}),
            )?;
        }
    }
    Ok(())
}

fn main() {
    if let Err(error) = run(Cli::parse()) {
        if error
            .downcast_ref::<io::Error>()
            .is_some_and(|e| e.kind() == io::ErrorKind::BrokenPipe)
        {
            return;
        }
        eprintln!("ams: {error:#}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn quoted_commands_cannot_inject_shell_statements() {
        assert_eq!(
            shell_quote("a'; touch /tmp/nope; '"),
            "'a'\\''; touch /tmp/nope; '\\'''"
        );
        assert_eq!(shell_quote(""), "''");
    }
    #[test]
    fn native_terminal_escape_sequences_are_not_replayed() {
        assert_eq!(clean("\x1b[31mevil\nnext", 100), " [31mevil next");
    }
}
