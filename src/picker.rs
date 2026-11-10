use std::{
    ffi::OsString,
    io::{self, IsTerminal, Write},
    path::Path,
    process::{Command, Stdio},
};

use ams::model::Format;
use anyhow::{Context, Result, bail};
use terminal_size::{Width, terminal_size};

pub fn select(
    db_path: &Path,
    initial_query: Option<&str>,
    semantic: bool,
    model: &str,
    format: Option<Format>,
) -> Result<Option<String>> {
    if !io::stdin().is_terminal() || !io::stderr().is_terminal() {
        bail!("interactive picker needs a terminal; use ams find --json for scripts");
    }

    let executable = std::env::current_exe().context("locating ams for live picker search")?;
    let columns = terminal_columns();
    let query = initial_query.unwrap_or("");
    // Run once before opening fzf so a missing index/model remains an actionable
    // error instead of an apparently empty interactive search.
    let initial = helper_command(&executable, db_path, query, semantic, model, format)
        .env("COLUMNS", list_columns(columns).to_string())
        .stdin(Stdio::null())
        .stderr(Stdio::inherit())
        .output()
        .context("loading initial picker results")?;
    if !initial.status.success() {
        bail!(
            "initial picker search failed ({}); see the diagnostic above",
            initial.status
        );
    }

    // fzf quotes {q} and {1} itself. Constants travel in environment variables,
    // expanded inside double quotes by a known POSIX shell. Besides protecting
    // spaces/metacharacters, this prevents a path containing `{q}` or a binding
    // delimiter from being interpreted by fzf in the first place.
    // fzf may discard reload stderr; retain diagnostics on the controlling tty.
    // Converting failure to successful empty output replaces stale matches.
    let reload = format!(
        "change:unbind(enter)+reload:{} 2>/dev/tty || :",
        shell_helper(semantic, format, false)
    );
    let preview_window = if columns >= 120 {
        "right,45%,border-left"
    } else {
        "down,45%,border-top"
    };
    let preview = shell_helper(semantic, format, true);
    let header = if semantic {
        "Sessions · meaning + words\nEnter select · Esc cancel"
    } else {
        "Sessions · words in history\nEnter select · Esc cancel"
    };
    let mut child = Command::new("fzf")
        .args([
            "--disabled",
            "--no-sort",
            "--delimiter=\t",
            "--with-nth=2..",
            "--no-multi",
            "--height=80%",
            "--layout=reverse",
            "--no-hscroll",
            "--with-shell=sh -c",
        ])
        .arg(if semantic {
            "--prompt=meaning> "
        } else {
            "--prompt=words> "
        })
        .arg(format!("--query={query}"))
        .arg(format!("--header={header}"))
        .arg(format!("--bind={reload}"))
        .arg("--bind=load:rebind(enter)")
        .arg(format!("--preview={preview}"))
        .arg(format!("--preview-window={preview_window}"))
        .env("AMS_PICKER_EXE", &executable)
        .env("AMS_PICKER_DB", db_path)
        .env("AMS_PICKER_MODEL", model)
        .env(
            "AMS_PICKER_FORMAT",
            format.map(|f| f.to_string()).unwrap_or_default(),
        )
        .env("AMS_PICKER_TERMINAL_COLUMNS", columns.to_string())
        .env(
            "AMS_PICKER_SIDE_PREVIEW",
            if columns >= 120 { "1" } else { "0" },
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .context("picker requires fzf on PATH")?;
    let mut input = child.stdin.take().context("opening picker input")?;
    let written = input.write_all(&initial.stdout);
    drop(input);
    let output = child.wait_with_output().context("waiting for fzf")?;
    if let Err(error) = written
        && error.kind() != io::ErrorKind::BrokenPipe
    {
        return Err(error).context("sending initial picker results");
    }
    if matches!(output.status.code(), Some(1 | 130)) {
        return Ok(None);
    }
    if !output.status.success() {
        bail!("fzf exited {}", output.status);
    }
    let selected = String::from_utf8(output.stdout).context("reading selected catalog ID")?;
    let Some(row) = selected.lines().next() else {
        return Ok(None);
    };
    let (id, _) = row
        .split_once('\t')
        .context("picker returned a row without a catalog ID")?;
    if id.is_empty() {
        bail!("picker returned an empty catalog ID");
    }
    Ok(Some(id.to_owned()))
}

fn helper_command(
    executable: &Path,
    db_path: &Path,
    query: &str,
    semantic: bool,
    model: &str,
    format: Option<Format>,
) -> Command {
    let mut command = Command::new(executable);
    // Attached option values also protect leading '-' in paths, models and
    // queries from being parsed as another CLI flag.
    let mut db = OsString::from("--db=");
    db.push(db_path);
    command.arg(db).arg("__picker-search");
    command.arg(format!("--query={query}"));
    command.arg(format!("--model={model}"));
    if semantic {
        command.arg("--semantic");
    }
    if let Some(format) = format {
        command.arg(format!("--format={format}"));
    }
    command
}

fn shell_helper(semantic: bool, format: Option<Format>, preview: bool) -> String {
    let mut command = String::new();
    if !preview {
        // Match the responsive preview split for every reload, including after
        // a terminal resize. Four cells leave room for fzf's pointer/gutter.
        command.push_str(
            "columns=${FZF_COLUMNS:-$AMS_PICKER_TERMINAL_COLUMNS}; \
             if [ \"$AMS_PICKER_SIDE_PREVIEW\" = 1 ]; then columns=$((columns - columns * 45 / 100)); fi; \
             COLUMNS=$((columns - 4)) ",
        );
    }
    command.push_str(
        "\"$AMS_PICKER_EXE\" --db=\"$AMS_PICKER_DB\" __picker-search \
         --model=\"$AMS_PICKER_MODEL\" --query={q}",
    );
    if semantic {
        command.push_str(" --semantic");
    }
    if format.is_some() {
        command.push_str(" --format=\"$AMS_PICKER_FORMAT\"");
    }
    if preview {
        command.push_str(" --preview={1}");
    }
    command
}

fn terminal_columns() -> usize {
    terminal_size()
        .map(|(Width(width), _)| usize::from(width))
        .filter(|width| *width > 0)
        .or_else(|| {
            std::env::var("COLUMNS")
                .ok()?
                .parse()
                .ok()
                .filter(|width| *width > 0)
        })
        .unwrap_or(80)
}

fn list_columns(columns: usize) -> usize {
    let columns = if columns >= 120 {
        columns - columns * 45 / 100
    } else {
        columns
    };
    columns.saturating_sub(4).max(1)
}
