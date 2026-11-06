//! `config.toml`: optional user settings with validated ranges.

use std::path::Path;

use serde::{Deserialize, Serialize};

/// Why `config.toml` could not be used. Every message starts with
/// `config.toml` so users can spot it in the console or log.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error(
        "config.toml: couldn't read {path}: {source}. Check that the file is readable, or delete it."
    )]
    Read {
        path: String,
        source: std::io::Error,
    },
    #[error("config.toml: couldn't understand the file. Fix or delete the line it points at.\n{0}")]
    Parse(String),
    #[error(
        "config.toml: {field} is {value}, but it must be between {min} and {max}. Fix or delete that line."
    )]
    OutOfRange {
        field: &'static str,
        value: String,
        min: String,
        max: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
// Names in the file are snake_case: `below_normal`, not `BelowNormal`.
#[serde(rename_all = "snake_case")]
pub enum Priority {
    BelowNormal,
    Normal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

/// Per-engine speed multipliers applied before SLC's own requested speed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
// `default`: keys missing from the file fall back to `Default::default()`.
// `deny_unknown_fields`: a misspelled key is an error instead of being ignored.
#[serde(default, deny_unknown_fields)]
pub struct BaseSpeeds {
    pub piper: f32,
    pub kokoro: f32,
    pub supertonic: f32,
}

impl Default for BaseSpeeds {
    fn default() -> Self {
        BaseSpeeds {
            piper: 1.0,
            kokoro: 1.0,
            supertonic: 1.05,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SupertonicConfig {
    pub num_steps: i32,
}

impl Default for SupertonicConfig {
    fn default() -> Self {
        SupertonicConfig { num_steps: 5 }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub port: u16,
    pub num_threads: i32,
    pub priority: Priority,
    pub idle_unload_minutes: u64,
    pub check_for_updates: bool,
    pub log_level: LogLevel,
    pub base_speed: BaseSpeeds,
    pub supertonic: SupertonicConfig,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            port: 8765,
            num_threads: 2,
            priority: Priority::Normal,
            idle_unload_minutes: 10,
            check_for_updates: true,
            log_level: LogLevel::Info,
            base_speed: BaseSpeeds::default(),
            supertonic: SupertonicConfig::default(),
        }
    }
}

/// Loads and validates `config.toml`. A missing file means all defaults.
pub fn load(path: &Path) -> Result<Config, ConfigError> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Config::default()),
        Err(source) => {
            return Err(ConfigError::Read {
                path: path.display().to_string(),
                source,
            });
        }
    };
    let config: Config = toml::from_str(&text).map_err(|e| ConfigError::Parse(e.to_string()))?;
    validate(&config)?;
    Ok(config)
}

fn check_int(field: &'static str, value: i64, min: i64, max: i64) -> Result<(), ConfigError> {
    if (min..=max).contains(&value) {
        Ok(())
    } else {
        Err(ConfigError::OutOfRange {
            field,
            value: value.to_string(),
            min: min.to_string(),
            max: max.to_string(),
        })
    }
}

fn check_speed(field: &'static str, value: f32) -> Result<(), ConfigError> {
    // `contains` is false for NaN, so NaN is rejected too.
    if (0.5..=2.0).contains(&value) {
        Ok(())
    } else {
        Err(ConfigError::OutOfRange {
            field,
            value: value.to_string(),
            min: "0.5".into(),
            max: "2.0".into(),
        })
    }
}

fn validate(c: &Config) -> Result<(), ConfigError> {
    check_int("port", c.port.into(), 1, 65535)?;
    check_int("num_threads", c.num_threads.into(), 1, 8)?;
    // u64 may not fit in i64; saturate so huge values still read as out of range.
    check_int(
        "idle_unload_minutes",
        i64::try_from(c.idle_unload_minutes).unwrap_or(i64::MAX),
        1,
        1440,
    )?;
    check_speed("base_speed.piper", c.base_speed.piper)?;
    check_speed("base_speed.kokoro", c.base_speed.kokoro)?;
    check_speed("base_speed.supertonic", c.base_speed.supertonic)?;
    check_int("supertonic.num_steps", c.supertonic.num_steps.into(), 1, 20)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Writes `contents` to a config.toml in a temp dir and loads it.
    fn load_str(contents: &str) -> Result<Config, ConfigError> {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(&path, contents).unwrap();
        load(&path)
    }

    fn err_of(contents: &str) -> String {
        load_str(contents).unwrap_err().to_string()
    }

    #[test]
    fn missing_file_gives_defaults() {
        let path = PathBuf::from("definitely/not/here/config.toml");
        let c = load(&path).unwrap();
        assert_eq!(c.port, 8765);
        assert_eq!(c.num_threads, 2);
        assert_eq!(c.priority, Priority::Normal);
        assert_eq!(c.idle_unload_minutes, 10);
        assert_eq!(c.log_level, LogLevel::Info);
        assert_eq!(c.base_speed.piper, 1.0);
        assert_eq!(c.base_speed.kokoro, 1.0);
        assert_eq!(c.base_speed.supertonic, 1.05);
        assert_eq!(c.supertonic.num_steps, 5);
        assert!(c.check_for_updates);
    }

    #[test]
    fn partial_file_keeps_other_defaults() {
        let c = load_str("port = 9000\n").unwrap();
        assert_eq!(c.port, 9000);
        assert_eq!(c.num_threads, 2);
    }

    #[test]
    fn priority_and_log_level_parse() {
        let c = load_str("priority = \"below_normal\"\nlog_level = \"debug\"\n").unwrap();
        assert_eq!(c.priority, Priority::BelowNormal);
        assert_eq!(c.log_level, LogLevel::Debug);
    }

    #[test]
    fn unknown_key_is_named() {
        let e = err_of("nm_threads = 3\n");
        assert!(e.starts_with("config.toml"), "{e}");
        assert!(e.contains("nm_threads"), "{e}");
    }

    #[test]
    fn unknown_nested_key_is_named() {
        let e = err_of("[base_speed]\npipr = 1.0\n");
        assert!(e.contains("pipr"), "{e}");
        let e = err_of("[supertonic]\nnum_stps = 3\n");
        assert!(e.contains("num_stps"), "{e}");
    }

    #[test]
    fn out_of_range_is_named() {
        let cases = [
            ("num_threads = 0\n", "num_threads", "0"),
            ("num_threads = 9\n", "num_threads", "9"),
            ("port = 0\n", "port", "0"),
            ("idle_unload_minutes = 0\n", "idle_unload_minutes", "0"),
            ("[base_speed]\npiper = 3.0\n", "base_speed.piper", "3"),
            ("[supertonic]\nnum_steps = 0\n", "supertonic.num_steps", "0"),
        ];
        for (toml, field, value) in cases {
            let e = err_of(toml);
            assert!(e.starts_with("config.toml"), "{e}");
            assert!(e.contains(field), "{field} missing from: {e}");
            assert!(e.contains(value), "{value} missing from: {e}");
        }
    }

    #[test]
    fn bad_toml_is_reported() {
        let e = err_of("port = \"x\"\n");
        assert!(e.starts_with("config.toml"), "{e}");
    }

    #[test]
    fn unreadable_file_is_reported() {
        // A directory exists but cannot be read as a file.
        let tmp = tempfile::tempdir().unwrap();
        let e = load(tmp.path()).unwrap_err().to_string();
        assert!(e.starts_with("config.toml"), "{e}");
    }
}
