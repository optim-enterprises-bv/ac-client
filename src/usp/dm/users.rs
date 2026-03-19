//! TR-181 Device.Users.* — System user accounts.
//!
//! Reads from /etc/passwd and supports password changes via the `passwd` command.

use crate::config::ClientConfig;
use std::collections::HashMap;

pub type Params = HashMap<String, String>;

pub async fn get(_cfg: &ClientConfig, path: &str) -> Params {
    let mut m = Params::new();
    let users = get_system_users();

    if path == "Device.Users." || path.contains("UserNumberOfEntries") {
        m.insert(
            "Device.Users.UserNumberOfEntries".to_string(),
            users.len().to_string(),
        );
    }

    if path == "Device.Users." || path.starts_with("Device.Users.User.") {
        let specific_idx = extract_index(path);
        for (i, user) in users.iter().enumerate() {
            let idx = i + 1;
            if let Some(si) = specific_idx {
                if si != idx {
                    continue;
                }
            }
            let base = format!("Device.Users.User.{idx}.");
            m.insert(format!("{base}Alias"), user.name.clone());
            m.insert(format!("{base}Username"), user.name.clone());
            m.insert(format!("{base}Enable"), user.enabled.to_string());
            m.insert(format!("{base}RemoteAccessCapable"), "true".to_string());
            m.insert(format!("{base}Language"), String::new());
        }
    }

    m
}

pub async fn set(_cfg: &ClientConfig, path: &str, value: &str) -> Result<(), String> {
    if path.ends_with("Password") {
        let users = get_system_users();
        let idx = extract_index(path)
            .ok_or_else(|| format!("Cannot parse User index from: {path}"))?;
        if idx == 0 || idx > users.len() {
            return Err(format!("User index {idx} out of range"));
        }
        let username = &users[idx - 1].name;

        // Use chpasswd to set password (reads "user:password" from stdin)
        let child = std::process::Command::new("chpasswd")
            .stdin(std::process::Stdio::piped())
            .spawn();

        match child {
            Ok(mut c) => {
                use std::io::Write;
                if let Some(ref mut stdin) = c.stdin {
                    stdin
                        .write_all(format!("{username}:{value}\n").as_bytes())
                        .map_err(|e| format!("Failed to write to chpasswd: {e}"))?;
                }
                let status = c.wait().map_err(|e| format!("chpasswd failed: {e}"))?;
                if status.success() {
                    Ok(())
                } else {
                    Err("chpasswd returned non-zero exit code".to_string())
                }
            }
            Err(e) => Err(format!("Failed to run chpasswd: {e}")),
        }
    } else {
        Err(format!("Read-only User param: {path}"))
    }
}

struct SystemUser {
    name: String,
    enabled: bool,
}

fn get_system_users() -> Vec<SystemUser> {
    let content = std::fs::read_to_string("/etc/passwd").unwrap_or_default();
    let mut users = Vec::new();

    for line in content.lines() {
        let fields: Vec<&str> = line.split(':').collect();
        if fields.len() < 7 {
            continue;
        }
        let name = fields[0].to_string();
        let shell = fields[6];
        // Only include interactive users (non-nologin, non-false shells)
        let enabled = !shell.contains("nologin")
            && !shell.contains("false")
            && !shell.is_empty();

        // Skip system accounts with UID < 1000, except root
        let uid: u32 = fields[2].parse().unwrap_or(0);
        if uid >= 1000 || name == "root" {
            users.push(SystemUser { name, enabled });
        }
    }

    users
}

fn extract_index(path: &str) -> Option<usize> {
    if let Some(pos) = path.find("User.") {
        let rest = &path[pos + 5..];
        let num_str: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        num_str.parse().ok()
    } else {
        None
    }
}
