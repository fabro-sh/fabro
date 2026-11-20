//! The ports a run's sandbox listens on, for the sandbox page's services
//! list: a script run over the sandbox's captured exec, and its parser.

use sandbox_driver::{Sandbox, run_bash};

/// One listening TCP socket.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ListeningPort {
    pub(crate) port:    u16,
    /// The local address as the sandbox reports it (`0.0.0.0:8080`,
    /// `[::]:8080`, `127.0.0.1:3000`).
    pub(crate) address: String,
    /// The listening process, when the sandbox can name it (`node`,
    /// `python3`, or `pid=1234`).
    pub(crate) process: Option<String>,
}

impl ListeningPort {
    pub(crate) fn new(port: u16, address: impl Into<String>) -> Self {
        Self {
            port,
            address: address.into(),
            process: None,
        }
    }
}

/// Lists listening TCP sockets with whatever the sandbox has: `ss`, then
/// `/proc/net/tcp*`, then `lsof` (a macOS host).
const LISTENING_PORTS_SCRIPT: &str = r#"if command -v ss >/dev/null 2>&1; then
  echo "SANDBOX_DRIVER_PORTS ss"
  ss -H -ltnp
  exit 0
fi
if [ -r /proc/net/tcp ] || [ -r /proc/net/tcp6 ]; then
  for file in /proc/net/tcp /proc/net/tcp6; do
    if [ -r "$file" ]; then
      echo "SANDBOX_DRIVER_PORTS procfs $file"
      cat -- "$file"
    fi
  done
  exit 0
fi
if command -v lsof >/dev/null 2>&1; then
  echo "SANDBOX_DRIVER_PORTS lsof"
  lsof -nP -iTCP -sTCP:LISTEN 2>/dev/null
  exit 0
fi
echo "SANDBOX_DRIVER_PORTS none"
"#;

/// The ports `sandbox` listens on, lowest first.
pub(crate) async fn listening_ports(sandbox: &dyn Sandbox) -> anyhow::Result<Vec<ListeningPort>> {
    let captured = run_bash(sandbox, LISTENING_PORTS_SCRIPT).await?;
    if !captured.success() {
        anyhow::bail!(
            "the listening-ports script failed: {}",
            String::from_utf8_lossy(&captured.stderr.bytes).trim()
        );
    }
    Ok(parse_listening_ports(&String::from_utf8_lossy(
        &captured.stdout.bytes,
    )))
}

/// Parses [`LISTENING_PORTS_SCRIPT`] output: the source marker, then that
/// tool's lines. Ports are reported once each, lowest first.
fn parse_listening_ports(output: &str) -> Vec<ListeningPort> {
    let mut lines = output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty());
    let Some(marker) = lines.next() else {
        return Vec::new();
    };
    let mut ports: Vec<ListeningPort> = match marker.strip_prefix("SANDBOX_DRIVER_PORTS ") {
        Some("ss") => lines.filter_map(parse_ss_line).collect(),
        Some(procfs) if procfs.starts_with("procfs") => parse_procfs(lines),
        Some("lsof") => lines.filter_map(parse_lsof_line).collect(),
        _ => Vec::new(),
    };
    ports.sort_by_key(|entry| entry.port);
    ports.dedup_by_key(|entry| entry.port);
    ports
}

/// `ss -H -ltnp`: `LISTEN 0 128 0.0.0.0:8080 0.0.0.0:*
/// users:(("node",pid=12,fd=18))`.
fn parse_ss_line(line: &str) -> Option<ListeningPort> {
    let fields: Vec<&str> = line.split_whitespace().collect();
    let address = *fields.get(3)?;
    let port = address.rsplit_once(':')?.1.parse::<u16>().ok()?;
    let process = fields
        .get(5..)
        .filter(|rest| !rest.is_empty())
        .map(|rest| rest.join(" "))
        .and_then(|users| ss_process_name(&users));
    let mut entry = ListeningPort::new(port, address);
    entry.process = process;
    Some(entry)
}

/// The first process name in `users:(("node",pid=12,fd=18),...)`.
fn ss_process_name(users: &str) -> Option<String> {
    let start = users.find("((\"")? + 3;
    let end = users[start..].find('"')? + start;
    Some(users[start..end].to_owned())
}

/// `/proc/net/tcp` and `tcp6`: hex `local_address` and state `0A` (LISTEN).
fn parse_procfs<'a>(lines: impl Iterator<Item = &'a str>) -> Vec<ListeningPort> {
    let mut ports = Vec::new();
    let mut ipv6 = false;
    for line in lines {
        if let Some(path) = line.strip_prefix("SANDBOX_DRIVER_PORTS procfs ") {
            ipv6 = path.ends_with("tcp6");
            continue;
        }
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.first().is_some_and(|first| *first == "sl") {
            continue;
        }
        let (Some(local), Some(state)) = (fields.get(1), fields.get(3)) else {
            continue;
        };
        if *state != "0A" {
            continue;
        }
        let Some((address_hex, port_hex)) = local.rsplit_once(':') else {
            continue;
        };
        let Ok(port) = u16::from_str_radix(port_hex, 16) else {
            continue;
        };
        let address = if ipv6 {
            format!("[{}]:{port}", procfs_ipv6(address_hex))
        } else {
            format!("{}:{port}", procfs_ipv4(address_hex))
        };
        ports.push(ListeningPort::new(port, address));
    }
    ports
}

/// A little-endian hex IPv4 address from procfs (`0100007F` is 127.0.0.1).
fn procfs_ipv4(hex: &str) -> String {
    let Ok(value) = u32::from_str_radix(hex, 16) else {
        return hex.to_owned();
    };
    let bytes = value.to_le_bytes();
    format!("{}.{}.{}.{}", bytes[0], bytes[1], bytes[2], bytes[3])
}

/// A procfs IPv6 address: four little-endian 32-bit words in hex. Rendered
/// as the words' groups without compression.
fn procfs_ipv6(hex: &str) -> String {
    if hex.len() != 32 {
        return hex.to_owned();
    }
    let mut groups = Vec::with_capacity(8);
    for word in 0..4 {
        let Ok(value) = u32::from_str_radix(&hex[word * 8..word * 8 + 8], 16) else {
            return hex.to_owned();
        };
        let bytes = value.to_le_bytes();
        groups.push(format!("{:x}", u16::from_be_bytes([bytes[0], bytes[1]])));
        groups.push(format!("{:x}", u16::from_be_bytes([bytes[2], bytes[3]])));
    }
    groups.join(":")
}

/// `lsof -nP -iTCP -sTCP:LISTEN`: `node 12 user 18u IPv4 0x... 0t0 TCP *:8080
/// (LISTEN)`.
fn parse_lsof_line(line: &str) -> Option<ListeningPort> {
    let fields: Vec<&str> = line.split_whitespace().collect();
    if fields.first().is_some_and(|first| *first == "COMMAND") {
        return None;
    }
    let name = fields.iter().rev().nth(1)?;
    let port = name.rsplit_once(':')?.1.parse::<u16>().ok()?;
    let mut entry = ListeningPort::new(port, (*name).to_owned());
    entry.process = fields.first().map(|command| (*command).to_owned());
    Some(entry)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listening_ports_parse_every_source() {
        let ss = "SANDBOX_DRIVER_PORTS ss\n\
                  LISTEN 0 128 0.0.0.0:8080 0.0.0.0:* users:((\"node\",pid=12,fd=18))\n\
                  LISTEN 0 128 [::]:22 [::]:*\n";
        let ports = parse_listening_ports(ss);
        assert_eq!(ports.len(), 2);
        assert_eq!(ports[0].port, 22);
        assert_eq!(ports[1].port, 8080);
        assert_eq!(ports[1].process.as_deref(), Some("node"));

        let procfs = "SANDBOX_DRIVER_PORTS procfs /proc/net/tcp\n\
                      sl local_address rem_address st tx_queue rx_queue tr tm->when retrnsmt uid timeout inode\n\
                      0: 0100007F:1F90 00000000:0000 0A 00000000:00000000 00:00000000 00000000 0 0 1 1 0000000000000000 100 0 0 10 0\n\
                      1: 0100007F:1F91 00000000:0000 01 00000000:00000000 00:00000000 00000000 0 0 1 1 0000000000000000 100 0 0 10 0\n\
                      SANDBOX_DRIVER_PORTS procfs /proc/net/tcp6\n\
                      sl local_address rem_address st\n\
                      0: 00000000000000000000000000000000:0016 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000 0 0 1 1 0000000000000000 100 0 0 10 0\n";
        let ports = parse_listening_ports(procfs);
        assert_eq!(ports.len(), 2, "{ports:?}");
        assert_eq!(ports[0].port, 22);
        assert_eq!(ports[0].address, "[0:0:0:0:0:0:0:0]:22");
        assert_eq!(ports[1].port, 8080);
        assert_eq!(ports[1].address, "127.0.0.1:8080");

        let lsof = "SANDBOX_DRIVER_PORTS lsof\n\
                    COMMAND PID USER FD TYPE DEVICE SIZE/OFF NODE NAME\n\
                    python3 41 user 3u IPv4 0x1 0t0 TCP 127.0.0.1:3000 (LISTEN)\n";
        let ports = parse_listening_ports(lsof);
        assert_eq!(ports.len(), 1);
        assert_eq!(ports[0].port, 3000);
        assert_eq!(ports[0].process.as_deref(), Some("python3"));

        assert!(parse_listening_ports("SANDBOX_DRIVER_PORTS none\n").is_empty());
    }
}
