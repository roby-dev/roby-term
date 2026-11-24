use std::collections::{BTreeMap, HashMap};
use std::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListeningPort {
    pub port: u16,
    pub pid: u32,
    pub process: String,
}

/// Lists local TCP ports currently in `LISTENING` state (zero external dependencies).
pub fn list_listening_ports() -> Vec<ListeningPort> {
    #[cfg(target_os = "windows")]
    {
        list_windows_ports()
    }
    #[cfg(not(target_os = "windows"))]
    {
        list_unix_ports()
    }
}

/// Terminates a local process by PID.
pub fn kill_process(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/F"])
            .creation_flags(CREATE_NO_WINDOW)
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
    #[cfg(not(target_os = "windows"))]
    {
        Command::new("kill")
            .args(["-9", &pid.to_string()])
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
}

#[cfg(target_os = "windows")]
fn list_windows_ports() -> Vec<ListeningPort> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    let mut combined = String::new();
    for proto in ["tcp", "tcpv6"] {
        if let Ok(out) = Command::new("netstat")
            .args(["-ano", "-p", proto])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
        {
            combined.push_str(&String::from_utf8_lossy(&out.stdout));
            combined.push('\n');
        }
    }
    if combined.is_empty() {
        return Vec::new();
    }

    let pid_map = windows_pid_names();
    parse_windows_netstat(&combined, &pid_map)
}

#[cfg(target_os = "windows")]
fn windows_pid_names() -> HashMap<u32, String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    let mut map = HashMap::new();
    let Ok(out) = Command::new("tasklist")
        .args(["/FO", "CSV", "/NH"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
    else {
        return map;
    };
    let text = String::from_utf8_lossy(&out.stdout);
    for line in text.lines() {
        let mut cols = line.split("\",\"");
        let Some(name_raw) = cols.next() else {
            continue;
        };
        let Some(pid_raw) = cols.next() else {
            continue;
        };
        let name = name_raw.trim_matches('"').to_string();
        if let Ok(pid) = pid_raw.trim_matches('"').parse::<u32>() {
            map.insert(pid, name);
        }
    }
    map
}

pub fn parse_windows_netstat(text: &str, pid_names: &HashMap<u32, String>) -> Vec<ListeningPort> {
    let mut by_port: BTreeMap<(u16, u32), ListeningPort> = BTreeMap::new();
    for line in text.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 5 {
            continue;
        }
        // State can be "LISTENING" (English) or localized ("ESCUCHANDO" in Spanish Windows)
        let state = parts[3].to_uppercase();
        if !matches!(state.as_str(), "LISTENING" | "ESCUCHANDO" | "ABHÖREN" | "ÉCOUTE") {
            continue;
        }
        let local_addr = parts[1];
        let Some(port_str) = local_addr.rsplit(':').next() else {
            continue;
        };
        let Ok(port) = port_str.parse::<u16>() else {
            continue;
        };
        let Ok(pid) = parts[4].parse::<u32>() else {
            continue;
        };
        if pid == 0 {
            continue;
        }
        let process = pid_names
            .get(&pid)
            .cloned()
            .unwrap_or_else(|| format!("pid-{pid}"));
        by_port.entry((port, pid)).or_insert(ListeningPort {
            port,
            pid,
            process,
        });
    }
    by_port.into_values().collect()
}

#[cfg(not(target_os = "windows"))]
fn list_unix_ports() -> Vec<ListeningPort> {
    let Ok(out) = Command::new("lsof")
        .args(["-iTCP", "-sTCP:LISTEN", "-n", "-P"])
        .output()
    else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let mut by_port: BTreeMap<(u16, u32), ListeningPort> = BTreeMap::new();
    for line in text.lines().skip(1) {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 9 {
            continue;
        }
        let process = parts[0].to_string();
        let Ok(pid) = parts[1].parse::<u32>() else {
            continue;
        };
        let name_col = parts[8];
        let Some(port_str) = name_col.rsplit(':').next() else {
            continue;
        };
        let Ok(port) = port_str.parse::<u16>() else {
            continue;
        };
        by_port.entry((port, pid)).or_insert(ListeningPort {
            port,
            pid,
            process,
        });
    }
    by_port.into_values().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_english_and_spanish_windows_netstat_listening_ports() {
        let sample = "\
  TCP    0.0.0.0:3000           0.0.0.0:0              LISTENING       1234
  TCP    127.0.0.1:8080         0.0.0.0:0              ESCUCHANDO      5678
  TCP    127.0.0.1:8080         127.0.0.1:50000        ESTABLISHED     5678
";
        let mut names = HashMap::new();
        names.insert(1234, "node.exe".to_string());
        names.insert(5678, "cargo.exe".to_string());

        let ports = parse_windows_netstat(sample, &names);
        assert_eq!(ports.len(), 2);
        assert_eq!(ports[0], ListeningPort { port: 3000, pid: 1234, process: "node.exe".into() });
        assert_eq!(ports[1], ListeningPort { port: 8080, pid: 5678, process: "cargo.exe".into() });
    }

    #[test]
    fn parses_ipv6_and_mixed_case_unicode_listening_states() {
        let sample = "\
  TCP    [::1]:5173             [::]:0                 Abhören         2222
  TCP    [::]:3000              [::]:0                 écoute          3333
";
        let mut names = HashMap::new();
        names.insert(2222, "vite.exe".to_string());
        names.insert(3333, "node.exe".to_string());

        let ports = parse_windows_netstat(sample, &names);
        assert_eq!(ports.len(), 2);
        assert_eq!(ports[0].port, 3000);
        assert_eq!(ports[1].port, 5173);
    }
}
