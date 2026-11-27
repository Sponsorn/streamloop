//! `config.json`: the schema both modes read, the control process's load and save, and the
//! supervisor's read-only view. Same keys, defaults, bounds and write-back as Node's `config.ts`.

mod merge;
pub mod ordered;
mod schema;

use std::fmt;
use std::path::{Path, PathBuf};

use ordered::Value;
pub use schema::{Config, Discord, DiscordEvents, DiscordTemplates, Playlist, Supervisor, DEFAULT_TEMPLATES, TEMPLATE_VARIABLES};

/// One refused field: its dotted path (`supervisor.freezeSeconds`, `playlists.0.id`) and why.
#[derive(Clone, Debug, PartialEq)]
pub struct Issue {
    pub path: String,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ConfigError {
    /// The file cannot be read or written, or is not a JSON object.
    File { path: PathBuf, message: String },
    /// Fields the schema refuses, in schema order.
    Invalid(Vec<Issue>),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            ConfigError::File { path, message } => write!(f, "{}: {message}", path.display()),
            ConfigError::Invalid(issues) => {
                let parts: Vec<String> = issues.iter().map(|i| format!("{}: {}", i.path, i.message)).collect();
                f.write_str(&parts.join("; "))
            }
        }
    }
}

impl std::error::Error for ConfigError {}

fn file_error(path: &Path, message: impl fmt::Display) -> ConfigError {
    ConfigError::File { path: path.to_path_buf(), message: message.to_string() }
}

/// The file's JSON. A leading UTF-8 byte order mark is skipped, since Notepad writes one.
fn read_json(path: &Path) -> Result<Value, ConfigError> {
    let bytes = std::fs::read(path).map_err(|e| file_error(path, e))?;
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&bytes);
    Value::parse(&String::from_utf8_lossy(bytes)).map_err(|e| file_error(path, format!("not valid JSON: {e}")))
}

/// The file's JSON with the legacy keys moved, and the config the schema makes of it.
fn read_migrated(path: &Path) -> Result<(Value, Config), ConfigError> {
    let mut json = read_json(path)?;
    merge::migrate(&mut json);
    let Value::Object(root) = &json else {
        return Err(file_error(path, "the top level is not a JSON object"));
    };
    let config = schema::validate(root).map_err(ConfigError::Invalid)?;
    Ok((json, config))
}

/// Through `<path>.tmp`, so a crash mid-write cannot leave half a config.
fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

/// Creates `config.json` from the `config.example.json` beside it when missing (releases ship only
/// the example), and writes it back in schema order when filling defaults or dropping keys changed it.
pub fn load(path: &Path) -> Result<Config, ConfigError> {
    let example = path.with_file_name("config.example.json");
    if !path.exists() && example.exists() {
        std::fs::copy(&example, path).map_err(|e| file_error(path, e))?;
    }
    let (json, config) = read_migrated(path)?;
    let text = to_json(&config);
    if text != json.to_pretty() {
        // As in Node, a failed write-back is not a failed load: the config read is good.
        let _ = write_atomic(path, &text);
    }
    Ok(config)
}

/// The supervisor's view: the same checks as `load`, and nothing is ever written.
pub fn read(path: &Path) -> Result<Config, ConfigError> {
    read_migrated(path).map(|(_, config)| config)
}

/// The `port` in config.json, read without the loader's write-back; None when missing or unreadable.
pub fn read_port(config_json: &Path) -> Option<u16> {
    let bytes = std::fs::read(config_json).ok()?;
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&bytes);
    let json: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    // As the schema reads it: a whole number, `7654.0` included.
    let port = json.get("port")?.as_f64().filter(|p| p.fract() == 0.0)?;
    (1.0..=f64::from(u16::MAX)).contains(&port).then_some(port as u16)
}

/// Merges `patch` into the file (see `merge`), checks the result and writes it. A refused save
/// leaves the file as it was. A file that cannot be read or parsed counts as empty.
pub fn save(patch: &serde_json::Value, path: &Path) -> Result<Config, ConfigError> {
    let mut existing = read_json(path).unwrap_or(Value::Object(vec![]));
    merge::migrate(&mut existing);
    let mut merged = merge::merge(&existing, &Value::from(patch));
    merge::migrate(&mut merged);
    let Value::Object(root) = &merged else { unreachable!("merge always builds an object") };
    let config = schema::validate(root).map_err(ConfigError::Invalid)?;
    write_atomic(path, &to_json(&config)).map_err(|e| file_error(path, e))?;
    Ok(config)
}

/// The shipped example's placeholder playlist is still the only one.
pub fn is_first_run(c: &Config) -> bool {
    c.playlists.len() == 1 && c.playlists[0].id.contains("xxxxx")
}

/// `JSON.stringify(config, null, 2)` in schema order: whole numbers without `.0`, no trailing newline.
pub fn to_json(c: &Config) -> String {
    schema::to_value(c).to_pretty()
}
