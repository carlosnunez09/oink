//! Access key and handshake.
//!
//! The access key is a random 100-bit code shown on the host and typed once on
//! the viewer. The handshake proves both sides know it without sending it:
//!
//! ```text
//! viewer -> host   MAGIC, viewer nonce (32 bytes)
//! host -> viewer   host nonce (32 bytes), keyed hash of ("host", both nonces)
//! viewer -> host   keyed hash of ("viewer", both nonces)
//! host -> viewer   one byte: 0 accepted
//! ```
//!
//! The handshake authenticates; it does not encrypt. Encryption is the job of
//! the network underneath, which is why a host accepts only Tailscale peers
//! unless told otherwise ([`PeerPolicy`]).

use crate::proto::MAGIC;
use std::{
    io::{self, Read, Write},
    net::IpAddr,
};

const ALPHABET: &[u8; 32] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
const CODE_LENGTH: usize = 20;
const KEY_CONTEXT: &str = "oink-remote 2026-11 access key v1";

/// A new access code such as `K7QPM-2XHVB-9RTWC-N4JDF`.
pub fn generate_code() -> String {
    let mut random = [0u8; CODE_LENGTH];
    getrandom::fill(&mut random).expect("the system random source is available");
    let mut code = String::with_capacity(CODE_LENGTH + 3);
    for (index, byte) in random.iter().enumerate() {
        if index > 0 && index % 5 == 0 {
            code.push('-');
        }
        // 256 is a multiple of 32, so the low five bits are uniform.
        code.push(ALPHABET[(byte & 31) as usize] as char);
    }
    code
}

/// The code without separators and in upper case, or `None` if it is not a valid code.
pub fn normalize_code(code: &str) -> Option<String> {
    let normalized: String = code
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .map(|c| c.to_ascii_uppercase())
        .collect();
    (normalized.len() == CODE_LENGTH && normalized.bytes().all(|byte| ALPHABET.contains(&byte))).then_some(normalized)
}

/// The same code grouped for display.
pub fn display_code(normalized: &str) -> String {
    normalized
        .as_bytes()
        .chunks(5)
        .map(|chunk| std::str::from_utf8(chunk).unwrap_or(""))
        .collect::<Vec<_>>()
        .join("-")
}

#[derive(Clone)]
pub struct AccessKey([u8; 32]);

impl std::fmt::Debug for AccessKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AccessKey(..)")
    }
}

impl AccessKey {
    pub fn from_code(code: &str) -> Option<Self> {
        Some(Self(blake3::derive_key(KEY_CONTEXT, normalize_code(code)?.as_bytes())))
    }

    fn proof(&self, role: &[u8], viewer_nonce: &[u8; 32], host_nonce: &[u8; 32]) -> blake3::Hash {
        let mut hasher = blake3::Hasher::new_keyed(&self.0);
        hasher.update(&[role.len() as u8]);
        hasher.update(role);
        hasher.update(viewer_nonce);
        hasher.update(host_nonce);
        hasher.finalize()
    }
}

fn nonce() -> [u8; 32] {
    let mut nonce = [0u8; 32];
    getrandom::fill(&mut nonce).expect("the system random source is available");
    nonce
}

fn denied(text: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, text)
}

/// Viewer side. Fails with `PermissionDenied` when the keys differ.
pub fn viewer_handshake(stream: &mut (impl Read + Write), key: &AccessKey) -> io::Result<()> {
    let viewer_nonce = nonce();
    stream.write_all(&MAGIC)?;
    stream.write_all(&viewer_nonce)?;
    let mut host_nonce = [0u8; 32];
    let mut host_proof = [0u8; 32];
    stream.read_exact(&mut host_nonce)?;
    stream.read_exact(&mut host_proof)?;
    // `blake3::Hash` compares in constant time.
    if key.proof(b"host", &viewer_nonce, &host_nonce) != blake3::Hash::from_bytes(host_proof) {
        return Err(denied("the access key does not match this computer's key"));
    }
    stream.write_all(key.proof(b"viewer", &viewer_nonce, &host_nonce).as_bytes())?;
    let mut status = [0u8; 1];
    stream.read_exact(&mut status)?;
    if status[0] != 0 {
        return Err(denied("the computer refused the connection"));
    }
    Ok(())
}

/// Host side. Fails with `InvalidData` for another protocol or version and
/// with `PermissionDenied` when the keys differ.
pub fn host_handshake(stream: &mut (impl Read + Write), key: &AccessKey) -> io::Result<()> {
    let mut magic = [0u8; 8];
    stream.read_exact(&mut magic)?;
    if magic != MAGIC {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "not an Oink remote viewer of this version",
        ));
    }
    let mut viewer_nonce = [0u8; 32];
    stream.read_exact(&mut viewer_nonce)?;
    let host_nonce = nonce();
    stream.write_all(&host_nonce)?;
    stream.write_all(key.proof(b"host", &viewer_nonce, &host_nonce).as_bytes())?;
    let mut viewer_proof = [0u8; 32];
    stream.read_exact(&mut viewer_proof)?;
    if key.proof(b"viewer", &viewer_nonce, &host_nonce) != blake3::Hash::from_bytes(viewer_proof) {
        return Err(denied("wrong access key"));
    }
    stream.write_all(&[0])
}

/// Which peer addresses a host accepts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PeerPolicy {
    /// Tailscale addresses and this computer itself. The default.
    Tailscale,
    /// Also private, link-local and unique-local addresses.
    Lan,
    /// Any address. The traffic is not encrypted by this program.
    Any,
}

/// Tailscale gives every device an address in 100.64.0.0/10 and fd7a:115c:a1e0::/48.
pub fn is_tailscale(address: IpAddr) -> bool {
    match address.to_canonical() {
        IpAddr::V4(ip) => ip.octets()[0] == 100 && ip.octets()[1] & 0xC0 == 64,
        IpAddr::V6(ip) => ip.segments()[..3] == [0xfd7a, 0x115c, 0xa1e0],
    }
}

impl PeerPolicy {
    pub fn allows(self, address: IpAddr) -> bool {
        let address = address.to_canonical();
        if address.is_loopback() || is_tailscale(address) {
            return true;
        }
        match self {
            Self::Tailscale => false,
            Self::Lan => match address {
                IpAddr::V4(ip) => ip.is_private() || ip.is_link_local(),
                IpAddr::V6(ip) => ip.is_unicast_link_local() || ip.is_unique_local(),
            },
            Self::Any => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        net::{TcpListener, TcpStream},
        thread,
    };

    fn pair() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let viewer = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        (viewer, listener.accept().unwrap().0)
    }

    #[test]
    fn codes_are_valid_and_distinct() {
        let (a, b) = (generate_code(), generate_code());
        assert_ne!(a, b);
        assert_eq!(a.len(), 23);
        let normalized = normalize_code(&a).unwrap();
        assert_eq!(display_code(&normalized), a);
        assert_eq!(normalize_code(&a.to_lowercase().replace('-', " ")), Some(normalized));
        assert_eq!(normalize_code("too-short"), None);
        // 0, 1, I and O are left out because they are easy to confuse.
        assert_eq!(normalize_code("00000-00000-00000-00000"), None);
    }

    #[test]
    fn matching_keys_connect() {
        let (mut viewer, mut host) = pair();
        let key = AccessKey::from_code("ABCDE-FGHJK-LMNPQ-RSTUV").unwrap();
        let host_key = key.clone();
        let host = thread::spawn(move || host_handshake(&mut host, &host_key));
        viewer_handshake(&mut viewer, &key).unwrap();
        host.join().unwrap().unwrap();
    }

    #[test]
    fn different_keys_are_refused_on_both_sides() {
        let (mut viewer, mut host) = pair();
        let host_key = AccessKey::from_code("ABCDE-FGHJK-LMNPQ-RSTUV").unwrap();
        let viewer_key = AccessKey::from_code("ABCDE-FGHJK-LMNPQ-RSTUW").unwrap();
        let host = thread::spawn(move || host_handshake(&mut host, &host_key));
        let error = viewer_handshake(&mut viewer, &viewer_key).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        drop(viewer);
        assert!(host.join().unwrap().is_err());
    }

    #[test]
    fn a_viewer_that_skips_its_proof_is_refused() {
        let (mut viewer, mut host) = pair();
        let key = AccessKey::from_code("ABCDE-FGHJK-LMNPQ-RSTUV").unwrap();
        let host = thread::spawn(move || host_handshake(&mut host, &key));
        viewer.write_all(&MAGIC).unwrap();
        viewer.write_all(&[7u8; 32]).unwrap();
        let mut reply = [0u8; 64];
        viewer.read_exact(&mut reply).unwrap();
        viewer.write_all(&[0u8; 32]).unwrap();
        assert_eq!(
            host.join().unwrap().unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
    }

    #[test]
    fn default_policy_accepts_only_tailscale_and_loopback() {
        let allowed = [
            "100.64.0.1",
            "100.101.102.103",
            "100.127.255.254",
            "127.0.0.1",
            "::1",
            "fd7a:115c:a1e0::1",
        ];
        let lan = [
            "192.168.1.20",
            "10.0.0.5",
            "169.254.3.4",
            "fe80::1",
            "fd00::1",
            "::ffff:192.168.1.20",
        ];
        let public = ["100.63.255.255", "100.128.0.1", "8.8.8.8", "2001:db8::1"];
        for text in allowed {
            assert!(PeerPolicy::Tailscale.allows(text.parse().unwrap()), "{text}");
        }
        for text in lan {
            assert!(!PeerPolicy::Tailscale.allows(text.parse().unwrap()), "{text}");
            assert!(PeerPolicy::Lan.allows(text.parse().unwrap()), "{text}");
        }
        for text in public {
            assert!(!PeerPolicy::Lan.allows(text.parse().unwrap()), "{text}");
            assert!(PeerPolicy::Any.allows(text.parse().unwrap()), "{text}");
        }
        assert!(PeerPolicy::Tailscale.allows("::ffff:100.64.0.1".parse().unwrap()));
    }
}
