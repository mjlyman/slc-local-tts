//! Changes `config.toml` for the Settings page, keeping the user's comments,
//! their other keys and the file's layout.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use toml_edit::{DocumentMut, Item, Value};

use crate::config::{self, Config, ConfigError};
use crate::models::install::rename_with_retry;

/// The settings the Settings page can change.
#[derive(Clone, Debug, PartialEq)]
pub struct FileSettings {
    pub check_for_updates: bool,
    pub num_threads: i32,
    pub port: u16,
    pub volume_percent: u16,
}

impl FileSettings {
    pub fn from_config(c: &Config) -> FileSettings {
        FileSettings {
            check_for_updates: c.check_for_updates,
            num_threads: c.num_threads,
            port: c.port,
            volume_percent: c.volume_percent,
        }
    }
}

/// Why `write` changed nothing.
#[derive(Debug, thiserror::Error)]
pub enum EditError {
    /// The file on disk can't be loaded, so it's left for the user to fix.
    #[error(
        "config.toml has a mistake, so Settings can't change it: {0} Open config.toml to fix it."
    )]
    Unreadable(ConfigError),
    /// The new settings would make the file invalid.
    #[error("{0}")]
    Invalid(ConfigError),
    #[error("Couldn't save config.toml: {0}.")]
    Io(io::Error),
}

/// A new `config.toml`: every setting commented out with its default and
/// range, so it loads as the defaults.
pub const TEMPLATE: &str = r#"# SLC Local TTS settings.
#
# A setting that's commented out keeps its default. To change one, delete
# the # at the start of its line and edit the value. Save the file, then
# restart SLC Local TTS.

# The port SLC's Custom API URL points at (http://127.0.0.1:8765).
# 1 to 65535.
# port = 8765

# CPU threads for each voice engine. More is faster but takes more CPU from
# the flight sim. 1 to 8.
# num_threads = 2

# How loud the voices are, in percent. Every voice is evened out to the
# same level first; this turns that level up or down. 50 to 200.
# volume_percent = 100

# Windows priority for speech: "normal" or "below_normal". Below normal can
# be starved next to a busy flight sim.
# priority = "normal"

# Minutes without speech before a voice is unloaded from memory.
# 1 to 1440.
# idle_unload_minutes = 10

# Check GitHub for a newer version at startup: true or false.
# check_for_updates = true

# How much the log records: "error", "warn", "info", "debug" or "trace".
# log_level = "info"

# A speed for each engine, applied on top of SLC's own speech rate.
# 0.5 to 2.0. To change one, also delete the # in front of [base_speed].
# [base_speed]
# piper = 1.0
# kokoro = 1.0
# supertonic = 1.05

# Supertonic's quality steps: more sounds better but takes longer. 1 to 20.
# To change it, also delete the # in front of [supertonic].
# [supertonic]
# num_steps = 5
"#;

/// The current settings in the file at `path` (the defaults if it's missing).
pub fn read(path: &Path) -> Result<FileSettings, ConfigError> {
    config::load(path).map(|c| FileSettings::from_config(&c))
}

/// Saves `new` into the file at `path`, changing only the settings that
/// differ from the file's, so the user's comments, layout and other keys
/// stay as they were. A missing file starts from `TEMPLATE`.
///
/// A setting that isn't live in the file yet replaces its commented-out
/// line (`# num_threads = 2` becomes `num_threads = 3`), so it lands under
/// its own comment and the user can't later uncomment a duplicate. See
/// `place_new_key`.
///
/// Nothing is written if the file can't be read (`Io`), if it's broken
/// (`Unreadable`), if the result wouldn't load (`Invalid`), or if the change
/// couldn't be placed so that the file says what was chosen (`Io` too). The new file
/// is written next to the old one and then renamed over it, so a failed save
/// never leaves half a file behind.
pub fn write(path: &Path, new: &FileSettings) -> Result<(), EditError> {
    let (text, existed) = match fs::read_to_string(path) {
        Ok(text) => (text, true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => (TEMPLATE.to_string(), false),
        // Locked or access denied: not a mistake in the file.
        Err(e) => return Err(EditError::Io(e)),
    };
    let current = parse(&text).map_err(EditError::Unreadable)?;
    let current = FileSettings::from_config(&current);
    if existed && current == *new {
        return Ok(());
    }

    let mut changes: Vec<(&str, Value)> = Vec::new();
    if current.port != new.port {
        changes.push(("port", i64::from(new.port).into()));
    }
    if current.num_threads != new.num_threads {
        changes.push(("num_threads", i64::from(new.num_threads).into()));
    }
    if current.check_for_updates != new.check_for_updates {
        changes.push(("check_for_updates", new.check_for_updates.into()));
    }
    if current.volume_percent != new.volume_percent {
        changes.push(("volume_percent", i64::from(new.volume_percent).into()));
    }

    // `toml::from_str` has accepted the text, so `toml_edit` accepts it too.
    let mut doc = parse_doc(&text).map_err(EditError::Unreadable)?;
    for (key, value) in changes {
        if !doc.contains_key(key) {
            // `trim` drops any spacing `to_string` puts around the value.
            let literal = value.to_string();
            let has_root_values = doc.iter().any(|(_, item)| item.is_value());
            if let Some(placed) =
                place_new_key(&doc.to_string(), key, literal.trim(), has_root_values)
            {
                doc = parse_doc(&placed).map_err(EditError::Invalid)?;
            }
        }
        set(&mut doc, key, value);
    }
    let mut edited = doc.to_string();
    // `toml_edit` writes every line ending as LF; a file saved with CRLF
    // (Notepad's habit) gets CRLF back.
    if text.contains("\r\n") {
        edited = edited.replace("\r\n", "\n").replace('\n', "\r\n");
    }
    let saved = parse(&edited).map_err(EditError::Invalid)?;
    // A last check that the file now says what the user chose, so a change
    // that landed in the wrong place (inside a table, say) is never saved.
    if FileSettings::from_config(&saved) != *new {
        return Err(EditError::Io(io::Error::other(
            "Settings couldn't place the change safely, so the file is unchanged; edit config.toml by hand instead",
        )));
    }

    save(path, &edited).map_err(EditError::Io)
}

/// Writes `TEMPLATE` to `path` if there's no file there yet.
pub fn ensure_exists(path: &Path) -> io::Result<()> {
    // `create_new` fails if the file exists, so a file someone else made a
    // moment ago is never replaced.
    match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(mut file) => file.write_all(TEMPLATE.as_bytes()),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(e),
    }
}

/// Parses and checks `text` the same way `config::load` does.
fn parse(text: &str) -> Result<Config, ConfigError> {
    let c: Config = toml::from_str(text).map_err(|e| ConfigError::Parse(e.to_string()))?;
    config::validate(&c)?;
    Ok(c)
}

/// `text` as an editable document.
fn parse_doc(text: &str) -> Result<DocumentMut, ConfigError> {
    text.parse()
        .map_err(|e: toml_edit::TomlError| ConfigError::Parse(e.to_string()))
}

/// Makes the top-level `key` live in `text` as `key = literal`, by editing
/// the text, where `toml_edit` alone would put it in the wrong place.
/// Returns `None` when `toml_edit` places it well itself.
///
/// The root section is everything above the first table header, live
/// (`[base_speed]`) or commented out (`# [base_speed]`).
/// - **The key has a commented-out line in the root section** (the
///   template's `# num_threads = 2`): the first one becomes the live line,
///   and any more are removed, so uncommenting one later can't make a
///   duplicate key.
/// - **Otherwise, if the file has live top-level keys,** `None`:
///   `toml_edit` adds the key after them.
/// - **Otherwise** the key goes on a new line at the end of the root
///   section. (`toml_edit` would put it above every comment in the file,
///   the header included.)
fn place_new_key(text: &str, key: &str, literal: &str, has_root_values: bool) -> Option<String> {
    // `split_inclusive` keeps each line's ending, so CRLF files stay CRLF.
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let root_end = lines
        .iter()
        .position(|line| is_header(line))
        .unwrap_or(lines.len());
    let commented: Vec<usize> = (0..root_end)
        .filter(|&i| is_commented_key(lines[i], key))
        .collect();

    if let Some(&first) = commented.first() {
        let mut out = String::with_capacity(text.len());
        for (i, line) in lines.iter().enumerate() {
            if i == first {
                let ending = &line[line.trim_end_matches(['\r', '\n']).len()..];
                out.push_str(&format!("{key} = {literal}{ending}"));
            } else if !commented.contains(&i) {
                out.push_str(line);
            }
        }
        return Some(out);
    }
    if has_root_values {
        return None;
    }
    let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let mut out = lines[..root_end].concat();
    if !out.is_empty() && !out.ends_with('\n') {
        out.push_str(newline);
    }
    out.push_str(&format!("{key} = {literal}{newline}"));
    out.push_str(&lines[root_end..].concat());
    Some(out)
}

/// The text after a line's leading `#`s and spaces, if it's a comment.
fn comment_body(line: &str) -> Option<&str> {
    let line = line.trim_start();
    line.starts_with('#')
        .then(|| line.trim_start_matches('#').trim_start())
}

/// Whether `line` is a table header, live or commented out: `[name]`, then
/// nothing but spaces or a comment.
fn is_header(line: &str) -> bool {
    let body = comment_body(line).unwrap_or_else(|| line.trim_start());
    body.strip_prefix('[')
        .and_then(|rest| rest.split_once(']'))
        .is_some_and(|(_, after)| {
            let after = after.trim();
            after.is_empty() || after.starts_with('#')
        })
}

/// Whether `line` is `key = ...` commented out, like `# port = 8765`.
fn is_commented_key(line: &str, key: &str) -> bool {
    comment_body(line)
        .and_then(|body| body.strip_prefix(key))
        .is_some_and(|rest| rest.trim_start().starts_with('='))
}

/// Sets the top-level `key` to `value`. A key already in the file keeps
/// the spacing and comment around its value (`port = 8765 # SLC uses this`
/// keeps `# SLC uses this`).
fn set(doc: &mut DocumentMut, key: &str, value: impl Into<Value>) {
    let mut value: Value = value.into();
    if let Some(old) = doc.get(key).and_then(Item::as_value) {
        *value.decor_mut() = old.decor().clone();
    }
    // Assigning through the index keeps the key's own decor (any comment
    // lines above it) and its place in the file.
    doc[key] = Item::Value(value);
}

/// Writes `text` to a temp file next to `path`, then renames it over `path`.
fn save(path: &Path, text: &str) -> io::Result<()> {
    let mut temp = path.as_os_str().to_owned();
    temp.push(".tmp");
    let temp = PathBuf::from(temp);
    let result = write_synced(&temp, text).and_then(|()| rename_with_retry(&temp, path));
    if result.is_err() {
        // Best effort: the temp file is ours, and a leftover one is harmless.
        let _ = fs::remove_file(&temp);
    }
    result
}

/// Writes `text` to `path` and waits until it's on the disk, so a power cut
/// right after the rename can't leave an empty `config.toml`.
fn write_synced(path: &Path, text: &str) -> io::Result<()> {
    let mut file = fs::File::create(path)?;
    file.write_all(text.as_bytes())?;
    file.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINE: &str = "# my settings\nport = 8765 # SLC uses this\n[base_speed]\nkokoro = 1.1\n";

    /// A temp dir and a `config.toml` path inside it holding `contents`
    /// (no file if `None`).
    fn file_with(contents: Option<&str>) -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.toml");
        if let Some(text) = contents {
            fs::write(&path, text).unwrap();
        }
        (tmp, path)
    }

    fn defaults() -> FileSettings {
        FileSettings::from_config(&Config::default())
    }

    #[test]
    fn write_keeps_comments_and_other_keys() {
        let (_tmp, path) = file_with(Some(MINE));
        let new = FileSettings {
            check_for_updates: false,
            num_threads: 3,
            port: 8765,
            volume_percent: 100,
        };

        write(&path, &new).unwrap();

        let text = fs::read_to_string(&path).unwrap();
        for part in [
            "# my settings",
            "port = 8765 # SLC uses this",
            "kokoro = 1.1",
            "num_threads = 3",
            "check_for_updates = false",
        ] {
            assert!(text.contains(part), "{part:?} missing from:\n{text}");
        }
        let c = config::load(&path).unwrap();
        assert_eq!(c.num_threads, 3);
        assert!(!c.check_for_updates);
        assert_eq!(c.base_speed.kokoro, 1.1);
        assert_eq!(read(&path).unwrap(), new);
    }

    #[test]
    fn changed_key_keeps_its_comment() {
        let (_tmp, path) = file_with(Some(MINE));
        let new = FileSettings {
            port: 9000,
            ..defaults()
        };

        write(&path, &new).unwrap();

        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("port = 9000 # SLC uses this"), "{text}");
        assert_eq!(config::load(&path).unwrap().port, 9000);
    }

    #[test]
    fn unchanged_settings_leave_file_identical() {
        let (_tmp, path) = file_with(Some(MINE));

        write(&path, &defaults()).unwrap();

        assert_eq!(fs::read(&path).unwrap(), MINE.as_bytes());
    }

    #[test]
    fn write_creates_file_from_template_when_missing() {
        let (_tmp, path) = file_with(None);
        let new = FileSettings {
            num_threads: 3,
            ..defaults()
        };

        write(&path, &new).unwrap();

        let c = config::load(&path).unwrap();
        assert_eq!(c.num_threads, 3);
        assert_eq!(
            c,
            Config {
                num_threads: 3,
                ..Config::default()
            }
        );
    }

    #[test]
    fn write_rejects_out_of_range_and_leaves_file() {
        let (_tmp, path) = file_with(Some(MINE));
        let new = FileSettings {
            port: 0,
            ..defaults()
        };

        let e = write(&path, &new).unwrap_err();

        assert!(matches!(e, EditError::Invalid(_)), "{e:?}");
        assert!(e.to_string().contains("port"), "{e}");
        assert_eq!(fs::read(&path).unwrap(), MINE.as_bytes());
    }

    #[test]
    fn write_refuses_unreadable_file_and_leaves_it() {
        let broken = "nm_threads = 3\n";
        let (_tmp, path) = file_with(Some(broken));

        let e = write(&path, &defaults()).unwrap_err();

        assert!(matches!(e, EditError::Unreadable(_)), "{e:?}");
        let text = e.to_string();
        assert!(text.contains("nm_threads"), "{text}");
        assert!(
            text.starts_with("config.toml has a mistake, so Settings can't change it: "),
            "{text}"
        );
        assert!(text.ends_with(" Open config.toml to fix it."), "{text}");
        assert_eq!(fs::read(&path).unwrap(), broken.as_bytes());
    }

    #[test]
    fn write_leaves_no_temp_file() {
        let (tmp, path) = file_with(Some(MINE));
        let new = FileSettings {
            num_threads: 4,
            ..defaults()
        };

        write(&path, &new).unwrap();

        let names: Vec<_> = fs::read_dir(tmp.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, ["config.toml"]);
    }

    #[test]
    fn template_loads_as_defaults() {
        let (_tmp, path) = file_with(Some(TEMPLATE));
        assert_eq!(config::load(&path).unwrap(), Config::default());
        // Every key is in it, commented out.
        for key in [
            "# port = 8765",
            "# num_threads = 2",
            "# volume_percent = 100",
            "# priority = \"normal\"",
            "# idle_unload_minutes = 10",
            "# check_for_updates = true",
            "# log_level = \"info\"",
            "# [base_speed]",
            "# piper = 1.0",
            "# kokoro = 1.0",
            "# supertonic = 1.05",
            "# [supertonic]",
            "# num_steps = 5",
        ] {
            assert!(TEMPLATE.contains(key), "{key:?} missing from the template");
        }
    }

    #[test]
    fn ensure_exists_writes_template_once() {
        let (_tmp, path) = file_with(None);
        ensure_exists(&path).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), TEMPLATE);

        fs::write(&path, MINE).unwrap();
        ensure_exists(&path).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), MINE);
    }

    #[test]
    fn read_gives_file_settings_or_defaults() {
        let (_tmp, path) = file_with(None);
        assert_eq!(read(&path).unwrap(), defaults());
        fs::write(&path, "check_for_updates = false\nport = 9000\n").unwrap();
        assert_eq!(
            read(&path).unwrap(),
            FileSettings {
                check_for_updates: false,
                num_threads: 2,
                port: 9000,
                volume_percent: 100,
            }
        );
    }

    /// `text` with every commented-out key and table header uncommented, as
    /// a user following the file's own instructions would do.
    fn uncomment_all(text: &str) -> String {
        text.lines()
            .map(|line| match line.strip_prefix("# ") {
                Some(rest)
                    if rest.starts_with('[')
                        || rest.split_once(" = ").is_some_and(|(k, _)| {
                            k.chars().all(|c| c.is_ascii_lowercase() || c == '_')
                        }) =>
                {
                    rest
                }
                _ => line,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn write_into_template_replaces_the_commented_line() {
        let (_tmp, path) = file_with(Some(TEMPLATE));
        let new = FileSettings {
            num_threads: 3,
            ..defaults()
        };

        write(&path, &new).unwrap();

        let text = fs::read_to_string(&path).unwrap();
        assert!(!text.contains("# num_threads = 2"), "{text}");
        assert!(
            text.contains("# the flight sim. 1 to 8.\nnum_threads = 3\n"),
            "{text}"
        );
        assert!(text.starts_with("# SLC Local TTS settings."), "{text}");
        // Uncommenting everything else, as the header says, still loads.
        let (_tmp2, all) = file_with(Some(&uncomment_all(&text)));
        assert_eq!(config::load(&all).unwrap().num_threads, 3);
    }

    #[test]
    fn write_sets_volume_under_its_comment() {
        let (_tmp, path) = file_with(Some(TEMPLATE));
        let new = FileSettings {
            volume_percent: 130,
            ..defaults()
        };

        write(&path, &new).unwrap();

        let text = fs::read_to_string(&path).unwrap();
        assert_eq!(
            text,
            TEMPLATE.replace("# volume_percent = 100\n", "volume_percent = 130\n")
        );
        assert_eq!(config::load(&path).unwrap().volume_percent, 130);
    }

    #[test]
    fn write_with_only_a_table_live_puts_the_key_in_the_root_section() {
        let start = TEMPLATE.replace("# [base_speed]", "[base_speed]");
        let (_tmp, path) = file_with(Some(&start));
        let new = FileSettings {
            port: 9000,
            ..defaults()
        };

        write(&path, &new).unwrap();

        let text = fs::read_to_string(&path).unwrap();
        assert!(!text.contains("# port = 8765"), "{text}");
        let header = text.find("# SLC Local TTS settings.").unwrap();
        let port = text.find("port = 9000").unwrap();
        let table = text.find("[base_speed]").unwrap();
        assert!(header < port && port < table, "{text}");
        assert_eq!(config::load(&path).unwrap().port, 9000);
        let (_tmp2, all) = file_with(Some(&uncomment_all(&text)));
        assert_eq!(config::load(&all).unwrap().port, 9000);
    }

    #[test]
    fn write_without_a_commented_line_goes_below_the_comments() {
        let start = "# my notes\n[base_speed]\nkokoro = 1.1\n";
        let (_tmp, path) = file_with(Some(start));
        let new = FileSettings {
            port: 9000,
            ..defaults()
        };

        write(&path, &new).unwrap();

        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "# my notes\nport = 9000\n[base_speed]\nkokoro = 1.1\n"
        );
    }

    #[test]
    fn write_keeps_crlf_line_endings() {
        let start = TEMPLATE.replace('\n', "\r\n");
        let (_tmp, path) = file_with(Some(&start));
        let new = FileSettings {
            check_for_updates: false,
            ..defaults()
        };

        write(&path, &new).unwrap();

        let text = fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("\r\ncheck_for_updates = false\r\n"),
            "{text:?}"
        );
        assert!(!text.replace("\r\n", "").contains('\n'), "{text:?}");
        assert!(!config::load(&path).unwrap().check_for_updates);
    }

    #[test]
    fn write_reports_a_file_it_cannot_read_as_io() {
        // A folder where the file should be can't be read as a file.
        let tmp = tempfile::tempdir().unwrap();
        let e = write(tmp.path(), &defaults()).unwrap_err();
        assert!(matches!(e, EditError::Io(_)), "{e:?}");
        assert!(
            e.to_string().starts_with("Couldn't save config.toml: "),
            "{e}"
        );
    }
}
