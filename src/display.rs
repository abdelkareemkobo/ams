use ams::catalog::Entry;
use std::io::{self, Write};
use terminal_size::{Width, terminal_size};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

const PREVIEW_GRAPHEMES: usize = 1_600;
const PREVIEW_LINES: usize = 18;

pub fn terminal_width() -> usize {
    let columns = |name| {
        std::env::var(name)
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|width| (1..=10_000).contains(width))
    };
    columns("FZF_PREVIEW_COLUMNS")
        .or_else(|| columns("COLUMNS"))
        .or_else(|| {
            terminal_size()
                .map(|(Width(width), _)| usize::from(width))
                .filter(|width| *width > 0)
        })
        .unwrap_or(80)
}

pub fn results(
    entries: &[Entry],
    out: &mut impl Write,
    width: usize,
    details: bool,
) -> io::Result<()> {
    if width == 0 {
        return Ok(());
    }
    if entries.is_empty() {
        write_wrapped(
            out,
            "No sessions found. Try fewer words or ams ask.",
            width,
            true,
            None,
        )?;
        return Ok(());
    }
    if details {
        for (index, entry) in entries.iter().enumerate() {
            if index > 0 {
                writeln!(out)?;
            }
            detail(entry, out, width)?;
        }
        return Ok(());
    }
    let columns = columns(width, true);
    let home = std::env::var("HOME").ok();
    writeln!(out, "{}", render_row(None, &columns, home.as_deref()))?;
    for entry in entries {
        writeln!(
            out,
            "{}",
            render_row(Some(entry), &columns, home.as_deref())
        )?;
    }
    Ok(())
}

pub fn picker_row(entry: &Entry, width: usize) -> String {
    let home = std::env::var("HOME").ok();
    render_row(Some(entry), &columns(width, false), home.as_deref())
}

pub fn detail(entry: &Entry, out: &mut impl Write, width: usize) -> io::Result<()> {
    if width == 0 {
        return Ok(());
    }
    for (label, value) in [
        ("Session", entry.title.as_str()),
        ("Agent", &entry.format.to_string()),
        ("Date", entry.started_at.as_str()),
        ("Project", entry.cwd.as_str()),
        ("Status", entry.status.as_str()),
    ] {
        let value = field_text(value);
        let value = if value.is_empty() { "—" } else { &value };
        write_wrapped(out, &format!("{label}: {value}"), width, false, None)?;
    }
    if let Some(error) = &entry.error {
        write_wrapped(
            out,
            &format!("Error: {}", field_text(error)),
            width,
            false,
            None,
        )?;
    }
    if !entry.summary.trim().is_empty() {
        writeln!(out)?;
        write_wrapped(out, "Preview:", width, false, None)?;
        let mut preview = String::new();
        let mut graphemes = entry.summary.graphemes(true);
        for grapheme in graphemes.by_ref().take(PREVIEW_GRAPHEMES) {
            preview.push_str(grapheme);
        }
        let truncated = graphemes.next().is_some();
        let preview = safe_text(&preview, true);
        let line_truncated = write_wrapped(out, &preview, width, true, Some(PREVIEW_LINES))?;
        if truncated || line_truncated {
            writeln!(out, "…")?;
        }
    }
    writeln!(out)?;
    for (label, value) in [
        ("ID", entry.id.as_str()),
        ("Native ID", entry.native_id.as_str()),
        ("Path", &entry.path.to_string_lossy()),
    ] {
        let value = field_text(value);
        let value = if value.is_empty() { "—" } else { &value };
        write_wrapped(out, &format!("{label}: {value}"), width, false, None)?;
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum Column {
    Id,
    Agent,
    Date,
    Project,
    Session,
}

fn columns(width: usize, with_id: bool) -> Vec<(Column, usize)> {
    if width == 0 {
        return Vec::new();
    }
    let mut columns = Vec::with_capacity(5);
    let mut remaining = width;
    if with_id {
        let id_width = if width >= 20 {
            12
        } else if width >= 16 {
            8
        } else {
            width.min(12)
        };
        columns.push((Column::Id, id_width));
        if width < 16 {
            return columns;
        }
        remaining -= id_width + 2;
    }
    // Keep a useful session title rather than squeezing every metadata field in.
    if remaining >= 32 {
        columns.push((Column::Agent, 8));
        columns.push((Column::Date, 10));
        remaining -= 22;
    } else if remaining >= 16 {
        columns.push((Column::Agent, 8));
        remaining -= 10;
    }
    if remaining >= 30 {
        let project_width = (remaining / 3).clamp(16, 30);
        columns.push((Column::Project, project_width));
        remaining -= project_width + 2;
    }
    columns.push((Column::Session, remaining));
    columns
}

fn render_row(entry: Option<&Entry>, columns: &[(Column, usize)], home: Option<&str>) -> String {
    let capacity =
        columns.iter().map(|(_, width)| width).sum::<usize>() + columns.len().saturating_sub(1) * 2;
    let mut row = String::with_capacity(capacity);
    for (index, &(column, width)) in columns.iter().enumerate() {
        if index > 0 {
            row.push_str("  ");
        }
        let text = match (entry, column) {
            (None, Column::Id) => "ID".into(),
            (None, Column::Agent) => "AGENT".into(),
            (None, Column::Date) => "DATE".into(),
            (None, Column::Project) => "PROJECT".into(),
            (None, Column::Session) => "SESSION".into(),
            (Some(entry), Column::Id) => take_cells(&single_line(&entry.id), width),
            (Some(entry), Column::Agent) => entry.format.to_string(),
            (Some(entry), Column::Date) => date(&entry.started_at).into(),
            (Some(entry), Column::Project) => project(&entry.cwd, home, width),
            (Some(entry), Column::Session) => {
                let title = single_line(&entry.title);
                let title = if title.is_empty() {
                    "(untitled)".into()
                } else {
                    title
                };
                if entry.status == "ok" {
                    title
                } else {
                    format!("[{}] {title}", single_line(&entry.status))
                }
            }
        };
        let text = if UnicodeWidthStr::width(text.as_str()) > width {
            ellipsize(&text, width)
        } else {
            text
        };
        let padding = width.saturating_sub(UnicodeWidthStr::width(text.as_str()));
        row.push_str(&text);
        if index + 1 < columns.len() {
            row.extend(std::iter::repeat_n(' ', padding));
        }
    }
    row
}

fn date(value: &str) -> &str {
    value
        .get(..10)
        .filter(|date| {
            date.bytes().enumerate().all(|(index, byte)| {
                if index == 4 || index == 7 {
                    byte == b'-'
                } else {
                    byte.is_ascii_digit()
                }
            })
        })
        .unwrap_or("—")
}

fn project(cwd: &str, home: Option<&str>, width: usize) -> String {
    let cwd = single_line(cwd);
    let cwd = cwd.trim_end_matches('/');
    if cwd.is_empty() {
        return "—".into();
    }
    let relative = home
        .filter(|home| !home.is_empty())
        .and_then(|home| cwd.strip_prefix(home.trim_end_matches('/')))
        .filter(|tail| tail.is_empty() || tail.starts_with('/'));
    let readable = match relative {
        Some(tail) => format!("~{tail}"),
        None => cwd.to_owned(),
    };
    if UnicodeWidthStr::width(readable.as_str()) <= width {
        return readable;
    }
    let budget = width.saturating_sub(1);
    let mut tail_start = readable.len();
    let mut cells = 0;
    for (index, grapheme) in readable.grapheme_indices(true).rev() {
        let next = UnicodeWidthStr::width(grapheme);
        if cells + next > budget {
            break;
        }
        cells += next;
        tail_start = index;
    }
    if width == 0 {
        String::new()
    } else {
        format!("…{}", &readable[tail_start..])
    }
}

fn take_cells(text: &str, width: usize) -> String {
    let mut cells = 0;
    let mut end = 0;
    for (index, grapheme) in text.grapheme_indices(true) {
        let next = UnicodeWidthStr::width(grapheme);
        if cells + next > width {
            break;
        }
        cells += next;
        end = index + grapheme.len();
    }
    text[..end].into()
}

fn ellipsize(text: &str, width: usize) -> String {
    if UnicodeWidthStr::width(text) <= width {
        return text.into();
    }
    if width == 0 {
        return String::new();
    }
    let mut text = take_cells(text, width - 1);
    text.push('…');
    text
}

fn unsafe_direction(character: char) -> bool {
    matches!(character, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{206f}')
}

fn safe_text(text: &str, multiline: bool) -> String {
    let mut safe = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '\n' if multiline => safe.push('\n'),
            '\t' if multiline => safe.push_str("    "),
            character
                if unsafe_direction(character)
                    || (character.is_control() && !character.is_whitespace()) =>
            {
                safe.extend(character.escape_unicode());
            }
            character if character.is_whitespace() => {
                if multiline || (!safe.is_empty() && !safe.ends_with(' ')) {
                    safe.push(' ');
                }
            }
            character => safe.push(character),
        }
    }
    if !multiline {
        safe.truncate(safe.trim_end().len());
    }
    safe
}

fn single_line(text: &str) -> String {
    safe_text(text, false)
}

fn field_text(text: &str) -> String {
    let mut safe = String::with_capacity(text.len());
    for character in text.chars() {
        if character.is_control()
            || unsafe_direction(character)
            || matches!(character, '\u{2028}' | '\u{2029}')
        {
            safe.extend(character.escape_unicode());
        } else {
            safe.push(character);
        }
    }
    safe
}

// Metadata is losslessly hard-wrapped. Preview prose prefers word boundaries,
// while a long path/word still splits only between complete Unicode graphemes.
fn write_wrapped(
    out: &mut impl Write,
    text: &str,
    width: usize,
    words: bool,
    max_lines: Option<usize>,
) -> io::Result<bool> {
    if width == 0 {
        return Ok(!text.is_empty());
    }
    let mut lines = 0;
    for paragraph in text.split('\n') {
        let mut rest = paragraph;
        loop {
            if max_lines.is_some_and(|limit| lines >= limit) {
                return Ok(true);
            }
            let mut cells = 0;
            let mut end = 0;
            let mut word_end = 0;
            for (index, grapheme) in rest.grapheme_indices(true) {
                let next = UnicodeWidthStr::width(grapheme);
                if cells + next > width {
                    break;
                }
                cells += next;
                end = index + grapheme.len();
                if grapheme.chars().all(char::is_whitespace) {
                    word_end = end;
                }
            }
            if end == 0 && !rest.is_empty() {
                // A two-cell glyph cannot fit a one-cell terminal. Consume it
                // as one replacement cell, never split its UTF-8/grapheme.
                let grapheme = rest.graphemes(true).next().expect("nonempty text");
                writeln!(out, "�")?;
                rest = &rest[grapheme.len()..];
            } else {
                if words && end < rest.len() && word_end > 0 {
                    end = word_end;
                }
                let line = &rest[..end];
                writeln!(out, "{}", if words { line.trim_end() } else { line })?;
                rest = &rest[end..];
                if words {
                    rest = rest.trim_start();
                }
            }
            lines += 1;
            if rest.is_empty() {
                break;
            }
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ams::model::Format;
    use std::path::PathBuf;

    fn entry() -> Entry {
        Entry {
            id: "abcdef0123456789abcdef0123456789".into(),
            format: Format::Opencode,
            native_id: "native-session-123456789".into(),
            path: PathBuf::from("/srv/private/sessions/native-session-123456789.jsonl"),
            title: "Fix 👩‍💻 e\u{301} 界 project indexing".into(),
            cwd: "/srv/work/projects/session-finder".into(),
            started_at: "2026-11-10T18:24:16.000Z".into(),
            bytes: 42,
            status: "ok".into(),
            error: None,
            summary: "Secret transcript body. A readable preview of the matching conversation."
                .into(),
        }
    }

    fn assert_safe_width(text: &str, width: usize) {
        for line in text.lines() {
            assert!(
                UnicodeWidthStr::width(line) <= width,
                "line exceeds {width} cells: {line:?}"
            );
        }
        assert!(!text.chars().any(|character| {
            unsafe_direction(character) || (character.is_control() && character != '\n')
        }));
    }

    #[test]
    fn all_surfaces_fit_narrow_and_wide_display_cells() {
        let entry = entry();
        for width in [0, 1, 2, 8, 16, 20, 32, 48, 64, 80, 120] {
            for details in [false, true] {
                let mut output = Vec::new();
                results(std::slice::from_ref(&entry), &mut output, width, details).unwrap();
                assert_safe_width(std::str::from_utf8(&output).unwrap(), width);
            }
            let row = picker_row(&entry, width);
            assert_safe_width(&row, width);
            assert!(!row.contains(['\n', '\t', '\r']));
        }
    }

    #[test]
    fn compact_table_has_identity_complete_date_and_project_not_transcript() {
        let entry = entry();
        for width in [80, 120] {
            let mut output = Vec::new();
            results(std::slice::from_ref(&entry), &mut output, width, false).unwrap();
            let output = String::from_utf8(output).unwrap();
            assert!(output.contains("abcdef012345"));
            assert!(output.contains("opencode"));
            assert!(output.contains("2026-11-10"));
            assert!(output.contains("session-finder"));
            assert!(!output.contains(&entry.native_id));
            assert!(!output.contains("Secret transcript"));
            assert!(!output.contains("T18:"));
            assert_eq!(output.lines().count(), 2);
            let header = output.lines().next().unwrap();
            let row = output.lines().nth(1).unwrap();
            for (label, value) in [
                ("AGENT", "opencode"),
                ("DATE", "2026-11-10"),
                ("SESSION", "Fix"),
            ] {
                let header_offset = header.find(label).unwrap();
                let row_offset = row.find(value).unwrap();
                assert_eq!(
                    UnicodeWidthStr::width(&header[..header_offset]),
                    UnicodeWidthStr::width(&row[..row_offset])
                );
            }
        }
    }

    #[test]
    fn detail_retains_native_identity_location_and_bounded_preview() {
        let mut entry = entry();
        entry.summary = "Readable preview words 界 👩‍💻. ".repeat(400);
        let mut output = Vec::new();
        detail(&entry, &mut output, 120).unwrap();
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains(&entry.id));
        assert!(output.contains(&entry.native_id));
        assert!(output.contains(entry.path.to_str().unwrap()));
        assert!(output.contains(&entry.cwd));
        assert!(output.contains(&entry.started_at));
        assert!(output.contains("Readable preview"));
        assert!(output.lines().count() < 35);
        assert_safe_width(&output, 120);
    }

    #[test]
    fn unsafe_native_text_cannot_inject_terminal_or_picker_records() {
        let mut entry = entry();
        entry.title = "normal\x1b[31m\nforged\trow\r\u{202e}evil\u{2066}hidden\x07".into();
        entry.cwd = "/tmp/\x1b]8;;bad\x07/\u{200f}project".into();
        entry.native_id = "native\x00\u{061c}id".into();
        entry.summary = "body\x1b[2J\r\nvisible\u{2028}next\u{202e}line".into();
        for details in [false, true] {
            let mut output = Vec::new();
            results(std::slice::from_ref(&entry), &mut output, 80, details).unwrap();
            let output = String::from_utf8(output).unwrap();
            assert_safe_width(&output, 80);
            if !details {
                assert_eq!(output.lines().count(), 2);
            }
        }
        let row = picker_row(&entry, 80);
        assert_safe_width(&row, 80);
        assert!(!row.contains(['\n', '\t', '\r']));
    }

    #[test]
    fn consumer_rows_preserve_complete_emoji_and_combining_graphemes() {
        let entry = entry();
        let row = picker_row(&entry, 120);
        assert!(row.contains("👩‍💻"));
        assert!(row.contains("e\u{301}"));
        assert!(row.contains('界'));
        assert!(!row.contains("Secret transcript"));
        assert!(!row.contains(&entry.native_id));
        assert_safe_width(&row, 120);
    }

    #[test]
    fn wrapped_details_preserve_full_identity_and_path_spacing() {
        let mut entry = entry();
        entry.path = PathBuf::from("/srv/two  spaces/very-long-session-file-name.jsonl");
        entry.native_id = "native-identity-with-many-characters-".repeat(4);
        let mut output = Vec::new();
        detail(&entry, &mut output, 24).unwrap();
        let output = String::from_utf8(output).unwrap();
        let unwrapped = output.replace('\n', "");
        assert!(unwrapped.contains(&entry.id));
        assert!(unwrapped.contains(&entry.native_id));
        assert!(unwrapped.contains(entry.path.to_str().unwrap()));
        assert_safe_width(&output, 24);
    }

    #[test]
    fn clipping_preserves_graphemes_and_home_relative_project_identity() {
        assert_eq!(take_cells("e\u{301}👩‍💻界", 3), "e\u{301}👩‍💻");
        assert_eq!(ellipsize("e\u{301}👩‍💻界", 4), "e\u{301}👩‍💻…");
        assert_eq!(
            project("/home/person/work/ams", Some("/home/person"), 30),
            "~/work/ams"
        );
        let abbreviated = project("/very/long/parent/directory/session-finder", None, 18);
        assert!(abbreviated.ends_with("session-finder"));
        assert_safe_width(&abbreviated, 18);
        assert_eq!(date("2026-11-10T18:24:16Z"), "2026-11-10");
        assert_eq!(date("not a valid date"), "—");
    }
}
