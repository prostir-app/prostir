//! Who the PC and the headset are to each other.
//!
//! * Every device has a long-term Curve25519 key ([`Identity`]); each connection
//!   proves it with a Noise `XX` handshake (see [`crate::link::tcp`]).
//! * Devices trust each other's keys after pairing once ([`TrustStore`]): one side shows a
//!   6-digit code, the user types it on the other side, and SPAKE2 turns it into a shared
//!   key without exposing it to offline guessing. A confirmation tag bound to this
//!   connection's handshake hash proves both sides used the same code over the same
//!   channel, which defeats a man in the middle.

use std::fmt::Write as _;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use hmac::{Hmac, Mac};
use sha2::Sha256;
use spake2::{Ed25519Group, Identity as SpakeId, Password, Spake2};

use crate::link::PeerInfo;
use crate::link::tcp::NOISE;

fn err(e: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, e.to_string())
}

/// A device's long-term key pair.
pub struct Identity {
    pub(crate) private: [u8; 32],
    pub public: [u8; 32],
}

impl Identity {
    /// Loads the key pair from `path` (`private public`, hex), creating it — readable
    /// by the owner only — on first use. A file that is there but cannot be read, or
    /// does not read as a key pair, is an error: replacing it would make every paired
    /// device a stranger without a word.
    pub fn load_or_create(path: &Path) -> io::Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                let pair = text.trim().split_once(' ');
                return match pair.map(|(a, b)| (from_hex(a), from_hex(b))) {
                    Some((Some(private), Some(public))) => Ok(Self { private, public }),
                    _ => Err(err(format!(
                        "{} is no key pair (move it away to make a new key, and pair again)",
                        path.display()
                    ))),
                };
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(io::Error::new(
                    e.kind(),
                    format!("cannot read the key in {}: {e}", path.display()),
                ));
            }
        }
        let keypair = snow::Builder::new(NOISE.parse().map_err(err)?)
            .generate_keypair()
            .map_err(err)?;
        let private: [u8; 32] = keypair.private.as_slice().try_into().map_err(err)?;
        let public: [u8; 32] = keypair.public.as_slice().try_into().map_err(err)?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        write_private(path, &format!("{} {}\n", to_hex(&private), to_hex(&public)))?;
        Ok(Self { private, public })
    }
}

fn write_private(path: &Path, text: &str) -> io::Result<()> {
    write_atomic(path, text.as_bytes())
}

/// Numbers the temporary files of this process: two writes of one file at the same
/// time (two threads, or one an earlier start of the app in the same process left
/// running) never share a temporary file.
static TEMPORARY: AtomicU64 = AtomicU64::new(0);

/// Replaces a file in one step: written next to it, to a temporary file of its own
/// (`<file>.tmp.<process>.<number>`), synced, then renamed over it. A crash or a full
/// disk mid-write leaves the old file, never a cut-off one. The file is readable by
/// its owner only (keys, trusted devices, settings).
pub fn write_atomic(path: &Path, data: &[u8]) -> io::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    let n = TEMPORARY.fetch_add(1, Ordering::Relaxed);
    let _ = write!(tmp, ".tmp.{}.{n}", std::process::id());
    let tmp = PathBuf::from(tmp);
    let written = write_synced(&tmp, data).and_then(|()| std::fs::rename(&tmp, path));
    if written.is_err() {
        // A write that failed leaves nothing behind.
        let _ = std::fs::remove_file(&tmp);
    }
    written
}

fn write_synced(path: &Path, data: &[u8]) -> io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut f = options.open(path)?;
    f.write_all(data)?;
    f.sync_all()
}

/// Removes the temporary files that writes of `path` cut short (a crash or power cut
/// between writing and renaming) left next to it: `<file>.tmp.<process>.<number>` of
/// [`write_atomic`], and `<file>.tmp` of older versions. Those of this process stay:
/// a write of it may be under way (a thread an earlier start in the same process left
/// running), and none of them can be left over from a crash. Call it at a start,
/// before the first write of `path`.
pub fn remove_leftovers(path: &Path) {
    let (Some(dir), Some(name)) = (path.parent(), path.file_name().and_then(|n| n.to_str())) else {
        return;
    };
    let dir = if dir.as_os_str().is_empty() {
        Path::new(".")
    } else {
        dir
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let own = std::process::id().to_string();
    for entry in entries.flatten() {
        let file = entry.file_name();
        let leftover = file
            .to_str()
            .and_then(|f| f.strip_prefix(name))
            .and_then(|rest| rest.strip_prefix(".tmp"))
            .is_some_and(|rest| match rest.strip_prefix('.') {
                None => rest.is_empty(),
                Some(numbers) => numbers.split_once('.').is_some_and(|(process, number)| {
                    digits(process) && digits(number) && process != own
                }),
            });
        if leftover {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

fn digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

pub fn to_hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .fold(String::with_capacity(2 * bytes.len()), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

/// `N` bytes from `2 * N` hex digits, else nothing.
pub fn from_hex<const N: usize>(s: &str) -> Option<[u8; N]> {
    if s.len() != N * 2 {
        return None;
    }
    let mut out = [0u8; N];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(s.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

/// Short human-readable form of a key, for logs and lists.
pub fn fingerprint(key: &[u8; 32]) -> String {
    to_hex(&key[..6])
}

/// A short number for the device whose key is `key`: the first 8 bytes of the key's
/// SHA-256. The same key always gives the same number, so files can name a device by
/// it across runs; it is never sent, and proves nothing (the key does).
pub fn key_id(key: &[u8; 32]) -> [u8; 8] {
    use sha2::Digest as _;
    let digest = Sha256::digest(key);
    let mut out = [0; 8];
    out.copy_from_slice(&digest[..8]);
    out
}

/// Peer keys this device has paired with, one `hexkey label` per line.
pub struct TrustStore {
    path: PathBuf,
    entries: Vec<([u8; 32], String)>,
}

impl TrustStore {
    /// The store in `path`, empty when there is no file yet (or it cannot be read:
    /// where that would let strangers in, use [`TrustStore::try_load`]).
    pub fn load(path: &Path) -> Self {
        Self::try_load(path).unwrap_or_else(|_| Self {
            path: path.to_owned(),
            entries: Vec::new(),
        })
    }

    /// The store in `path`, empty when there is no file yet; an error when the file is
    /// there but cannot be read.
    pub fn try_load(path: &Path) -> io::Result<Self> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
            Err(e) => {
                return Err(io::Error::new(
                    e.kind(),
                    format!("cannot read {}: {e}", path.display()),
                ));
            }
        };
        let entries = text
            .lines()
            .filter_map(|l| {
                let (key, label) = l.split_once(' ').unwrap_or((l, ""));
                Some((from_hex(key.trim())?, label.trim().to_owned()))
            })
            .collect();
        Ok(Self {
            path: path.to_owned(),
            entries,
        })
    }

    pub fn contains(&self, key: &[u8; 32]) -> bool {
        self.entries.iter().any(|(k, _)| k == key)
    }

    pub fn label(&self, key: &[u8; 32]) -> Option<&str> {
        self.entries
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, l)| l.as_str())
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn add(&mut self, key: [u8; 32], label: &str) -> io::Result<()> {
        self.entries.retain(|(k, _)| *k != key);
        let label = label.replace(['\n', '\r'], " ");
        self.entries.push((key, label));
        self.save()
    }

    /// Every trusted key with its label, oldest first.
    pub fn entries(&self) -> &[([u8; 32], String)] {
        &self.entries
    }

    pub fn remove(&mut self, key: &[u8; 32]) -> io::Result<()> {
        self.entries.retain(|(k, _)| k != key);
        self.save()
    }

    pub fn clear(&mut self) -> io::Result<()> {
        self.entries.clear();
        self.save()
    }

    fn save(&self) -> io::Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = self.entries.iter().fold(String::new(), |mut s, (k, l)| {
            let _ = writeln!(s, "{} {l}", to_hex(k));
            s
        });
        write_atomic(&self.path, text.as_bytes())
    }
}

/// Bytes of a SPAKE2 message (Ed25519 group, symmetric: a marker byte and a point).
pub const PAIR_MSG_LEN: usize = 33;
/// Bytes of a pairing confirmation tag (HMAC-SHA256).
pub const CONFIRM_LEN: usize = 32;

/// A random 6-digit pairing code.
pub fn pairing_code() -> String {
    let mut b = [0u8; 4];
    getrandom::fill(&mut b).expect("system randomness");
    format!("{:06}", u32::from_le_bytes(b) % 1_000_000)
}

/// One side of a SPAKE2 exchange keyed by the pairing code.
pub struct Pairing(Spake2<Ed25519Group>);

impl Pairing {
    /// Starts pairing with `code`; send the returned message to the other side.
    pub fn start(code: &str) -> (Self, [u8; PAIR_MSG_LEN]) {
        let digits: String = code.chars().filter(char::is_ascii_digit).collect();
        let (state, msg) = Spake2::<Ed25519Group>::start_symmetric(
            &Password::new(digits.as_bytes()),
            &SpakeId::new(b"prostir pairing v1"),
        );
        let msg = msg
            .try_into()
            .expect("an Ed25519 SPAKE2 message is 33 bytes");
        (Self(state), msg)
    }

    /// Key shared with the other side if both used the same code (otherwise the keys
    /// differ and the confirmation tags will not match).
    pub fn finish(self, peer_msg: &[u8]) -> io::Result<Vec<u8>> {
        self.0
            .finish(peer_msg)
            .map_err(|_| err("bad pairing message"))
    }
}

/// Tag proving knowledge of the pairing key, bound to this connection and to `role`.
pub fn confirmation(
    key: &[u8],
    role: &str,
    peer: &PeerInfo,
    own_key: &[u8; 32],
) -> [u8; CONFIRM_LEN] {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("any key length");
    mac.update(b"prostir confirm v1");
    mac.update(role.as_bytes());
    mac.update(&peer.binding);
    // Both long-term keys, in a fixed (sorted) order so both sides compute the same.
    let (a, b) = if own_key <= &peer.key {
        (own_key, &peer.key)
    } else {
        (&peer.key, own_key)
    };
    mac.update(a);
    mac.update(b);
    mac.finalize().into_bytes().into()
}

/// Constant-time tag comparison.
pub fn tags_match(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::link::Via;

    fn identity(tag: &str) -> Identity {
        let dir = std::env::temp_dir().join(format!("prostir-test-{}-{tag}", std::process::id()));
        let id = Identity::load_or_create(&dir.join("key")).unwrap();
        // Loading again gives the same key.
        assert_eq!(
            Identity::load_or_create(&dir.join("key")).unwrap().public,
            id.public
        );
        std::fs::remove_dir_all(dir).unwrap();
        id
    }

    /// What each side of one connection knows of the other.
    fn peers(a: &Identity, b: &Identity, binding: &[u8]) -> (PeerInfo, PeerInfo) {
        let info = |key| PeerInfo {
            key,
            binding: binding.to_vec(),
            via: Via::Network,
        };
        (info(b.public), info(a.public))
    }

    /// A key's number is the start of its SHA-256, the same every time.
    #[test]
    fn a_keys_number() {
        assert_eq!(to_hex(&key_id(&[0; 32])), "66687aadf862bd77");
        assert_eq!(key_id(&[7; 32]), key_id(&[7; 32]));
        assert_ne!(key_id(&[7; 32]), key_id(&[8; 32]));
    }

    #[test]
    fn the_same_code_confirms_both_ways() {
        let (headset, pc) = (identity("h"), identity("p"));
        // `of_pc`: what the headset knows of the PC, and the other way round.
        let (of_pc, of_headset) = peers(&headset, &pc, &[3; 32]);
        let (h, h_msg) = Pairing::start("123 456");
        let (p, p_msg) = Pairing::start("123456");
        let h_key = h.finish(&p_msg).unwrap();
        let p_key = p.finish(&h_msg).unwrap();
        let from_headset = confirmation(&h_key, "headset", &of_pc, &headset.public);
        assert!(tags_match(
            &from_headset,
            &confirmation(&p_key, "headset", &of_headset, &pc.public)
        ));
        let from_pc = confirmation(&p_key, "pc", &of_headset, &pc.public);
        assert!(tags_match(
            &from_pc,
            &confirmation(&h_key, "pc", &of_pc, &headset.public)
        ));
        // Not one role for the other.
        assert!(!tags_match(&from_headset, &from_pc));
    }

    #[test]
    fn wrong_code_does_not_confirm() {
        let (a, b) = (identity("a"), identity("b"));
        let (of_b, of_a) = peers(&a, &b, &[5; 32]);
        let (pa, ma) = Pairing::start("111111");
        let (pb, mb) = Pairing::start("222222");
        let ka = pa.finish(&mb).unwrap();
        let kb = pb.finish(&ma).unwrap();
        assert!(!tags_match(
            &confirmation(&ka, "headset", &of_b, &a.public),
            &confirmation(&kb, "headset", &of_a, &b.public)
        ));
    }

    /// The same code over another connection (a man in the middle runs two) does not
    /// confirm.
    #[test]
    fn a_confirmation_is_bound_to_its_connection() {
        let (a, b) = (identity("c"), identity("d"));
        let (of_b, _) = peers(&a, &b, &[1; 32]);
        let (_, of_a_elsewhere) = peers(&a, &b, &[2; 32]);
        let (pa, ma) = Pairing::start("333333");
        let (pb, mb) = Pairing::start("333333");
        let (ka, kb) = (pa.finish(&mb).unwrap(), pb.finish(&ma).unwrap());
        assert!(!tags_match(
            &confirmation(&ka, "headset", &of_b, &a.public),
            &confirmation(&kb, "headset", &of_a_elsewhere, &b.public)
        ));
    }

    #[test]
    fn trust_store_roundtrip() {
        let path = std::env::temp_dir().join(format!("prostir-trust-{}", std::process::id()));
        let mut t = TrustStore::load(&path);
        assert!(t.is_empty());
        t.add([7; 32], "Quest 3").unwrap();
        let t2 = TrustStore::load(&path);
        assert!(t2.contains(&[7; 32]) && !t2.contains(&[8; 32]));
        assert_eq!(t2.label(&[7; 32]), Some("Quest 3"));
        std::fs::remove_file(path).unwrap();
        let code = pairing_code();
        assert_eq!(code.len(), 6);
    }

    /// A store or a key that is there but does not read is an error, never an empty
    /// store or a new key.
    #[test]
    fn unreadable_files_fail_closed() {
        let dir = std::env::temp_dir().join(format!("prostir-closed-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(TrustStore::try_load(&dir.join("none")).unwrap().is_empty());
        // A directory where the file should be cannot be read as one.
        assert!(TrustStore::try_load(&dir).is_err());
        assert!(Identity::load_or_create(&dir).is_err());
        let key = dir.join("key");
        std::fs::write(&key, "broken\n").unwrap();
        assert!(Identity::load_or_create(&key).is_err());
        assert_eq!(std::fs::read_to_string(&key).unwrap(), "broken\n", "kept");
        std::fs::remove_file(&key).unwrap();
        let made = Identity::load_or_create(&key).unwrap();
        assert_eq!(Identity::load_or_create(&key).unwrap().public, made.public);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Each write has a temporary file of its own, and none stays behind; leftovers of
    /// writes cut short, old style and new, are removed, and nothing else.
    #[test]
    fn temporary_files() {
        let dir = std::env::temp_dir().join(format!("prostir-atomic-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.txt");
        let names = || {
            let mut names: Vec<String> = std::fs::read_dir(&dir)
                .unwrap()
                .map(|e| e.unwrap().file_name().into_string().unwrap())
                .collect();
            names.sort();
            names
        };
        std::thread::scope(|s| {
            for i in 0..8 {
                let path = &path;
                s.spawn(move || write_atomic(path, format!("{i}\n").as_bytes()).unwrap());
            }
        });
        assert_eq!(names(), ["settings.txt"]);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.ends_with('\n') && text.len() == 2, "{text:?}");
        // A directory in the way: the write fails, and leaves no temporary file.
        std::fs::create_dir(dir.join("busy")).unwrap();
        assert!(write_atomic(&dir.join("busy"), b"x").is_err());
        assert_eq!(names(), ["busy", "settings.txt"]);
        std::fs::remove_dir(dir.join("busy")).unwrap();

        for leftover in [
            "settings.txt.tmp",
            "settings.txt.tmp.12.0",
            "settings.txt.tmp.7.3",
        ] {
            std::fs::write(dir.join(leftover), "half a fi").unwrap();
        }
        // This process's own may be a write under way.
        let own = format!("settings.txt.tmp.{}.5", std::process::id());
        for kept in [
            "settings.txt.tmpx",
            "settings.txt.tmp.12",
            "settings.txt.tmp.x.1",
            "settings.txt.tmp.12.0.bak",
            "other.txt.tmp",
            "settings.txt.old",
            &own,
        ] {
            std::fs::write(dir.join(kept), "").unwrap();
        }
        remove_leftovers(&path);
        let mut kept = vec![
            "other.txt.tmp".to_owned(),
            "settings.txt".into(),
            "settings.txt.old".into(),
            "settings.txt.tmp.12".into(),
            "settings.txt.tmp.12.0.bak".into(),
            "settings.txt.tmp.x.1".into(),
            "settings.txt.tmpx".into(),
            own,
        ];
        kept.sort();
        assert_eq!(names(), kept);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
