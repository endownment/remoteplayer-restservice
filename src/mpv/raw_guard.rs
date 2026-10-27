//! Best-effort filter for commands forwarded verbatim to mpv.
//!
//! mpv's IPC is not designed as a security boundary: `run`/`subprocess` start
//! arbitrary programs, scripts can be loaded, and several options make mpv
//! write files. This list blocks the well-known ways in; it cannot make raw IPC
//! safe for untrusted clients. Disable `mpv.raw-ipc-enabled` if the API is
//! reachable by people you do not trust.

use serde_json::Value;

const DENIED_COMMANDS: [&str; 10] = [
    "run",
    "subprocess",
    "load-script",
    "load-config-file",
    "load-input-conf",
    "screenshot-to-file",
    "dump-cache",
    "vf",
    "af",
    "apply-profile",
];

const PROPERTY_COMMANDS: [&str; 9] =
    ["set", "set_property", "set_property_string", "add", "cycle", "cycle-values", "multiply", "change-list", "del"];

const DENIED_PROPERTY_PREFIXES: [&str; 23] = [
    "input-ipc-server",
    "input-ipc-client",
    "input-conf",
    "script",
    "scripts",
    "load-scripts",
    "ytdl",
    "stream-record",
    "record-file",
    "screenshot-dir",
    "screenshot-directory",
    "screenshot-template",
    "watch-later-dir",
    "log-file",
    "dump-stats",
    "include",
    "config-dir",
    "use-filedir-conf",
    "vf",
    "af",
    "lavfi-complex",
    "o",
    "profile",
];

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RawCommandError {
    #[error("the command must be a non-empty JSON array whose first element is the command name")]
    Malformed,
    #[error("command `{0}` is not allowed through the raw IPC endpoint")]
    DeniedCommand(String),
    #[error("property `{0}` cannot be changed through the raw IPC endpoint")]
    DeniedProperty(String),
    #[error("loadfile options may not set `{0}`")]
    DeniedLoadOption(String),
}

pub fn check_raw_command(command: &[Value]) -> Result<(), RawCommandError> {
    let name: &str = command.first().and_then(Value::as_str).ok_or(RawCommandError::Malformed)?;
    let name_lower: String = name.to_ascii_lowercase();
    if DENIED_COMMANDS.contains(&name_lower.as_str()) {
        return Err(RawCommandError::DeniedCommand(name.to_string()));
    }
    if PROPERTY_COMMANDS.contains(&name_lower.as_str()) {
        let property: &str = command.get(1).and_then(Value::as_str).unwrap_or_default();
        if is_denied_property(property) {
            return Err(RawCommandError::DeniedProperty(property.to_string()));
        }
    }
    if name_lower == "loadfile" || name_lower == "loadlist" {
        // Per-file options are the trailing string argument ("key=value,key=value").
        for argument in command.iter().skip(2).filter_map(Value::as_str) {
            for option in argument.split(',') {
                let key: &str = option.split('=').next().unwrap_or_default().trim();
                if is_denied_property(key) {
                    return Err(RawCommandError::DeniedLoadOption(key.to_string()));
                }
            }
        }
    }
    Ok(())
}

fn is_denied_property(property: &str) -> bool {
    let normalized: String = property.trim().trim_start_matches("options/").to_ascii_lowercase();
    DENIED_PROPERTY_PREFIXES.iter().any(|prefix: &&str| {
        normalized == *prefix
            || normalized.starts_with(&format!("{prefix}-"))
            || normalized.starts_with(&format!("{prefix}/"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn check(command: Value) -> Result<(), RawCommandError> {
        let array: Vec<Value> = command.as_array().expect("array").clone();
        check_raw_command(&array)
    }

    #[test]
    fn ordinary_playback_commands_pass() {
        assert_eq!(check(json!(["cycle", "pause"])), Ok(()));
        assert_eq!(check(json!(["seek", 10, "relative"])), Ok(()));
        assert_eq!(check(json!(["set_property", "volume", 50])), Ok(()));
        assert_eq!(check(json!(["loadfile", "/media/a.mkv", "append", -1, "start=10"])), Ok(()));
    }

    #[test]
    fn process_spawning_and_scripts_are_blocked() {
        assert!(matches!(check(json!(["run", "sh", "-c", "id"])), Err(RawCommandError::DeniedCommand(_))));
        assert!(matches!(check(json!(["load-script", "/tmp/x.lua"])), Err(RawCommandError::DeniedCommand(_))));
    }

    #[test]
    fn dangerous_properties_are_blocked_including_options_prefix() {
        assert!(matches!(
            check(json!(["set_property", "script-opts", "x=y"])),
            Err(RawCommandError::DeniedProperty(_))
        ));
        assert!(matches!(
            check(json!(["set", "options/ytdl-path", "/tmp/x"])),
            Err(RawCommandError::DeniedProperty(_))
        ));
        assert!(matches!(
            check(json!(["change-list", "scripts", "append", "/tmp/x.lua"])),
            Err(RawCommandError::DeniedProperty(_))
        ));
    }

    #[test]
    fn loadfile_cannot_smuggle_options() {
        assert!(matches!(
            check(json!(["loadfile", "http://x", "replace", -1, "start=1,script-opts=ytdl_hook-ytdl_path=/tmp/x"])),
            Err(RawCommandError::DeniedLoadOption(_))
        ));
    }

    #[test]
    fn malformed_commands_are_rejected() {
        assert_eq!(check(json!([])), Err(RawCommandError::Malformed));
        assert_eq!(check(json!([42])), Err(RawCommandError::Malformed));
    }
}
