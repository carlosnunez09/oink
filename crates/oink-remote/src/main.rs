//! `oink-remote host` on the computer to reach, `oink-remote connect <address>` on the other.

use clap::{Parser, Subcommand};
#[cfg_attr(not(any(target_os = "macos", windows)), allow(unused_imports))]
use oink_remote::auth;
use oink_remote::{config, proto::DEFAULT_PORT};
use std::{io, process::ExitCode};

#[derive(Debug, Parser)]
#[command(
    name = "oink-remote",
    version,
    about = "See and control this computer from another one over Tailscale"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Let another computer see and control this one (Windows).
    Host {
        #[arg(long, default_value_t = DEFAULT_PORT)]
        port: u16,
        /// Monitor to show to viewers that do not choose one; 0 is the primary monitor.
        #[arg(long, default_value_t = 0)]
        monitor: u8,
        /// Also accept viewers on the local network, not only over Tailscale.
        /// The connection is then not encrypted.
        #[arg(long)]
        allow_lan: bool,
        /// Replace the access key with a new one. Viewers must enter it again.
        #[arg(long)]
        new_key: bool,
        /// Use the slower GDI screen capture even where Desktop Duplication works.
        #[arg(long)]
        gdi: bool,
        /// Let the computer go to sleep while hosting (it cannot be reached asleep).
        #[arg(long)]
        allow_sleep: bool,
    },
    /// See and control a computer that runs `oink-remote host`.
    Connect {
        /// Tailscale address or name of the computer, optionally with `:port`.
        host: String,
        /// Access key shown by the host. Needed once; it is remembered per host.
        #[arg(long)]
        key: Option<String>,
        /// Monitor of the host to show; 0 is its primary monitor.
        #[arg(long)]
        monitor: Option<u8>,
        /// Highest frame rate.
        #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u8).range(1..=120))]
        fps: u8,
        /// Picture quality while the screen moves, 10 (smallest) to 95. A still screen is always sharp.
        #[arg(long, default_value_t = 60, value_parser = clap::value_parser!(u8).range(10..=95))]
        quality: u8,
        /// Send the keys as they are: Command is the Windows key. By default on a Mac,
        /// left Command acts as Ctrl and right Command as the Windows key.
        #[arg(long)]
        pc_keys: bool,
    },
    /// Show this computer's access key.
    Key,
}

/// This computer's Tailscale address, if Tailscale is running: the local
/// address the system would use to reach Tailscale's own service address.
/// No packet is sent.
#[cfg(windows)]
fn tailscale_address() -> Option<std::net::IpAddr> {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("100.100.100.100:53").ok()?;
    socket
        .local_addr()
        .ok()
        .map(|address| address.ip())
        .filter(|ip| auth::is_tailscale(*ip))
}

#[cfg(windows)]
fn host(port: u16, monitor: u8, allow_lan: bool, new_key: bool, gdi: bool, allow_sleep: bool) -> io::Result<()> {
    use oink_remote::{host, windows_host};
    use std::sync::Arc;
    windows_host::init_process();
    if !allow_sleep {
        windows_host::stay_awake_on_this_thread();
    }
    let code = config::host_code(&config::directory()?, new_key)?;
    let key = auth::AccessKey::from_code(&code).expect("a stored code is valid");
    let listener = std::net::TcpListener::bind(("0.0.0.0", port))?;
    println!("Oink Remote is hosting this computer on port {port}.");
    println!("Access key: {code}");
    match tailscale_address() {
        Some(address) if port == DEFAULT_PORT => println!("On the other computer run: oink-remote connect {address}"),
        Some(address) => println!("On the other computer run: oink-remote connect {address}:{port}"),
        None => println!("Tailscale does not seem to be running here; start it so the other computer can connect."),
    }
    if allow_lan {
        println!("Local network viewers are accepted too; that connection is not encrypted.");
    }
    println!("Press Ctrl+C to stop.");
    let policy = if allow_lan {
        auth::PeerPolicy::Lan
    } else {
        auth::PeerPolicy::Tailscale
    };
    let log: host::Log = Arc::new(|line| println!("{line}"));
    host::serve(
        listener,
        host::HostConfig {
            key,
            policy,
            monitor,
            log,
        },
        Arc::new(windows_host::WindowsPlatform { gdi_only: gdi }),
    )
}

#[cfg(not(windows))]
fn host(_port: u16, _monitor: u8, _allow_lan: bool, _new_key: bool, _gdi: bool, _allow_sleep: bool) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "hosting is available on Windows only for now",
    ))
}

#[cfg_attr(not(any(target_os = "macos", windows)), allow(dead_code))]
fn split_host(host: &str) -> (String, u16) {
    // `name:port` or `1.2.3.4:port`; a bare IPv6 address has more than one colon.
    match host.rsplit_once(':') {
        Some((name, port)) if !name.contains(':') => match port.parse() {
            Ok(port) => (name.to_owned(), port),
            Err(_) => (host.to_owned(), DEFAULT_PORT),
        },
        _ => (host.to_owned(), DEFAULT_PORT),
    }
}

#[cfg(any(target_os = "macos", windows))]
fn connect(
    host: String,
    key: Option<String>,
    monitor: Option<u8>,
    fps: u8,
    quality: u8,
    pc_keys: bool,
) -> io::Result<()> {
    use oink_remote::{client::Options, host::DEFAULT_MONITOR, viewer};
    #[cfg(windows)]
    oink_remote::windows_host::init_process();
    let directory = config::directory()?;
    let (name, port) = split_host(&host);
    let code = match key.or_else(|| config::saved_code(&directory, &name)) {
        Some(code) => code,
        None => {
            println!("Access key shown by `oink-remote host` on {name}:");
            let mut line = String::new();
            io::stdin().read_line(&mut line)?;
            line
        }
    };
    let Some(access) = auth::AccessKey::from_code(&code) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "an access key looks like K7QPM-2XHVB-9RTWC-N4JDF",
        ));
    };
    let options = viewer::ViewerOptions {
        host: name.clone(),
        connection: Options {
            monitor: monitor.unwrap_or(DEFAULT_MONITOR),
            max_fps: fps,
            quality,
        },
        mac_layout: cfg!(target_os = "macos") && !pc_keys,
    };
    let saved = code.trim().to_owned();
    let remember = move || {
        if let Err(error) = config::save_code(&directory, &name, &saved) {
            eprintln!("Could not remember the access key: {error}");
        }
    };
    viewer::run((options.host.clone(), port), access, options, remember).map_err(|error| {
        match error.downcast::<io::Error>() {
            Ok(error) => *error,
            Err(error) => io::Error::other(error.to_string()),
        }
    })
}

#[cfg(not(any(target_os = "macos", windows)))]
fn connect(
    _host: String,
    _key: Option<String>,
    _monitor: Option<u8>,
    _fps: u8,
    _quality: u8,
    _pc_keys: bool,
) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "the viewer is available on macOS and Windows",
    ))
}

fn main() -> ExitCode {
    let result = match Cli::parse().command {
        Command::Host {
            port,
            monitor,
            allow_lan,
            new_key,
            gdi,
            allow_sleep,
        } => host(port, monitor, allow_lan, new_key, gdi, allow_sleep),
        Command::Connect {
            host,
            key,
            monitor,
            fps,
            quality,
            pc_keys,
        } => connect(host, key, monitor, fps, quality, pc_keys),
        Command::Key => config::directory()
            .and_then(|directory| config::host_code(&directory, false))
            .map(|code| {
                println!("{code}");
            }),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
            eprintln!("Refused: {error}. Check the access key (run `oink-remote key` on the host).");
            ExitCode::FAILURE
        }
        Err(error) => {
            eprintln!("Error: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_argument_splits_into_name_and_port() {
        assert_eq!(split_host("100.64.0.9"), ("100.64.0.9".to_owned(), DEFAULT_PORT));
        assert_eq!(split_host("desk-pc:5000"), ("desk-pc".to_owned(), 5000));
        assert_eq!(
            split_host("fd7a:115c:a1e0::9"),
            ("fd7a:115c:a1e0::9".to_owned(), DEFAULT_PORT)
        );
        assert_eq!(split_host("desk-pc:x"), ("desk-pc:x".to_owned(), DEFAULT_PORT));
    }
}
