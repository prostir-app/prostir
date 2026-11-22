//! Discovery by asking: the headset sends a query ([`Query`], `PRSQ`) to UDP port
//! [`crate::DISCOVERY_PORT`], as a broadcast and straight to the addresses of PCs it
//! knows (typed in, or the last one it reached, a VPN address too); a PC answers
//! ([`Answer`], `PRSA`) to the address and port the query came from, and only to it.
//!
//! Who a PC answers: everyone while it has no paired headset or while its pairing
//! window is open; otherwise only a query that carries its own tag ([`find_tag`]). A
//! tag is made with the PC's find secret ([`FindSecret`]), a random value the PC gives
//! only to the headsets it trusts, inside the encrypted session
//! (`HostMsg::FindSecret`), and makes anew when it forgets one: knowing the PC's
//! public key, or having connected to it once, is not enough to make a tag. With a new
//! nonce each time, the tags hide which PC is looked for, and the PC learns nothing of
//! the other PCs the headset knows. A query carries at least [`MIN_TAGS`] tags (random
//! ones where the headset knows fewer PCs, in a random order), so the count does not
//! tell headsets apart either.
//!
//! What the tags do not hide: that a headset asks (and from which address), its name
//! while it looks for a PC to pair with, and that a PC answered. A query is not bound to
//! a time: someone who captured one can send it again and get the same PC's answer.
//!
//! A query is at least [`MIN_QUERY`] bytes (padded with zeros) and an answer always
//! smaller ([`MAX_ANSWER`]): a forged sender address gets less from a PC than was
//! sent, so a PC cannot be used to amplify traffic.
//!
//! Neither is encrypted or authenticated beyond the tags: an answer is a hint where to
//! connect, and trust comes from the handshake.
//!
//! A PC reads a query without making anything new ([`QueryRef::read`] borrows the
//! datagram, [`Answer::write_to`] fills a buffer kept for it), and a headset makes its
//! queries in the same memory each time ([`Query::renew`], [`Query::encode_into`]): a
//! stream of datagrams costs neither side allocations.

use sha2::{Digest, Sha256};

/// The start of a query.
pub const QUERY_MAGIC: &[u8; 4] = b"PRSQ";
/// The start of an answer.
pub const ANSWER_MAGIC: &[u8; 4] = b"PRSA";
/// The version of the format this build writes. A reader takes a query of a newer
/// version by the fields of this one: a newer version only adds after them.
pub const VERSION: u8 = 1;
/// The smallest query a PC reads.
pub const MIN_QUERY: usize = 128;
/// The biggest query a PC reads.
pub const MAX_QUERY: usize = 512;
/// The most PCs one query looks for.
pub const MAX_TAGS: usize = 32;
/// The fewest tags a query carries: random ones fill it up.
pub const MIN_TAGS: usize = 8;
/// The longest headset name in a query, bytes.
pub const MAX_HEADSET_NAME: usize = 32;
/// The longest PC name in an answer, bytes.
pub const MAX_PC_NAME: usize = 64;
/// The biggest answer: always below [`MIN_QUERY`].
pub const MAX_ANSWER: usize = 4 + 1 + NONCE + TAG + 2 + 1 + 1 + MAX_PC_NAME;
/// The bytes of a nonce.
pub const NONCE: usize = 16;
/// The bytes of a tag.
pub const TAG: usize = 8;

const _: () = assert!(MAX_ANSWER < MIN_QUERY);
const _: () = assert!(4 + 1 + NONCE + 1 + MAX_TAGS * TAG + 1 + MAX_HEADSET_NAME <= MAX_QUERY);

/// Flags of an answer.
pub mod flags {
    /// The PC's pairing window is open: a new headset can pair now.
    pub const PAIRING: u8 = 1;
    /// The PC answers every query (no paired headset, or the pairing window is open).
    pub const ANSWERS_ALL: u8 = 2;
}

/// A PC's find secret: what its tags are made with. Random, kept by the PC, given to
/// the headsets it trusts in the encrypted session, and made anew when it forgets one.
pub type FindSecret = [u8; 32];

/// A new find secret.
pub fn new_secret() -> FindSecret {
    let mut secret = [0; 32];
    getrandom::fill(&mut secret).expect("system randomness");
    secret
}

/// A tag of the PC whose find secret is `secret`, for a query with `nonce`: the
/// headset puts it in to look for that PC.
pub fn find_tag(secret: &FindSecret, nonce: &[u8; NONCE]) -> [u8; TAG] {
    tag(b"prostir-find", secret, nonce)
}

/// The tag a PC puts in its answer to a query with `nonce`: the headset tells which of
/// the PCs it knows answered before it connects.
pub fn here_tag(secret: &FindSecret, nonce: &[u8; NONCE]) -> [u8; TAG] {
    tag(b"prostir-here", secret, nonce)
}

fn tag(label: &[u8], secret: &FindSecret, nonce: &[u8; NONCE]) -> [u8; TAG] {
    let mut h = Sha256::new();
    h.update(label);
    h.update(secret);
    h.update(nonce);
    let digest = h.finalize();
    let mut out = [0; TAG];
    out.copy_from_slice(&digest[..TAG]);
    out
}

/// `text` cut to at most `max` bytes, at a character's edge.
fn cut(text: &str, max: usize) -> &str {
    let mut end = text.len().min(max);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// A headset asking which PCs are there.
///
/// ```text
/// 0     4  "PRSQ"
/// 4     1  version (1)
/// 5    16  nonce
/// 21    1  count (8 to 32 from this build; a PC reads 0 to 32)
/// 22  8×n  a tag for each PC looked for (find_tag), random ones up to 8, in a random
///          order
/// .     1  name_len (0 to 32)
/// .     n  the headset's name, UTF-8: empty but while it looks for a PC to pair with
/// .     .  zeros up to 128 bytes at least, 512 at most
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Query {
    pub nonce: [u8; NONCE],
    pub tags: Vec<[u8; TAG]>,
    /// The headset's name, while it looks for a PC to pair with; empty otherwise.
    pub name: String,
}

impl Query {
    /// A query for the PCs whose find secrets are `pcs` (the first [`MAX_TAGS`]), with
    /// a new nonce, filled up to [`MIN_TAGS`] with random tags and shuffled; `name` is
    /// cut to [`MAX_HEADSET_NAME`] bytes.
    pub fn new<'a>(pcs: impl IntoIterator<Item = &'a FindSecret>, name: &str) -> Self {
        let mut query = Self {
            nonce: [0; NONCE],
            tags: Vec::with_capacity(MIN_TAGS),
            name: String::new(),
        };
        query.renew(pcs, name);
        query
    }

    /// Makes this query anew in the memory it has (a new nonce, the tags of `pcs`,
    /// `name`), as [`Query::new`] would.
    pub fn renew<'a>(&mut self, pcs: impl IntoIterator<Item = &'a FindSecret>, name: &str) {
        getrandom::fill(&mut self.nonce).expect("system randomness");
        self.tags.clear();
        for secret in pcs.into_iter().take(MAX_TAGS) {
            self.tags.push(find_tag(secret, &self.nonce));
        }
        while self.tags.len() < MIN_TAGS {
            let mut dummy = [0; TAG];
            getrandom::fill(&mut dummy).expect("system randomness");
            self.tags.push(dummy);
        }
        // Fisher-Yates, so where the real tags are tells nothing.
        let mut random = [0u8; MAX_TAGS * 4];
        getrandom::fill(&mut random).expect("system randomness");
        for i in (1..self.tags.len()).rev() {
            let r = u32::from_le_bytes(random[i * 4..i * 4 + 4].try_into().unwrap());
            self.tags.swap(i, r as usize % (i + 1));
        }
        self.name.clear();
        self.name.push_str(cut(name, MAX_HEADSET_NAME));
    }

    /// Whether it looks for the PC whose find secret is `secret`.
    pub fn looks_for(&self, secret: &FindSecret) -> bool {
        let ours = find_tag(secret, &self.nonce);
        self.tags.contains(&ours)
    }

    /// The datagram: [`MIN_QUERY`] bytes at least.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(MIN_QUERY);
        self.encode_into(&mut out);
        out
    }

    /// The datagram in `out`, which is cleared first.
    pub fn encode_into(&self, out: &mut Vec<u8>) {
        let tags = &self.tags[..self.tags.len().min(MAX_TAGS)];
        let name = cut(&self.name, MAX_HEADSET_NAME);
        out.clear();
        out.extend_from_slice(QUERY_MAGIC);
        out.push(VERSION);
        out.extend_from_slice(&self.nonce);
        out.push(tags.len() as u8);
        for t in tags {
            out.extend_from_slice(t);
        }
        out.push(name.len() as u8);
        out.extend_from_slice(name.as_bytes());
        if out.len() < MIN_QUERY {
            out.resize(MIN_QUERY, 0);
        }
    }

    /// Reads a datagram: `None` for anything that is not a whole query of
    /// [`MIN_QUERY`] to [`MAX_QUERY`] bytes (a short or a broken one, an old PC's
    /// beacon).
    pub fn decode(buf: &[u8]) -> Option<Self> {
        let q = QueryRef::read(buf)?;
        Some(Self {
            nonce: q.nonce,
            tags: q.tags().collect(),
            name: q.name.to_owned(),
        })
    }
}

/// A query as it lies in the datagram, read without copying (see [`Query`] for the
/// layout): what a PC looks at for each datagram that comes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueryRef<'a> {
    pub nonce: [u8; NONCE],
    tags: &'a [[u8; TAG]],
    /// The headset's name, while it looks for a PC to pair with; empty otherwise.
    pub name: &'a str,
}

impl<'a> QueryRef<'a> {
    /// Reads a datagram: `None` for anything that is not a whole query of
    /// [`MIN_QUERY`] to [`MAX_QUERY`] bytes.
    pub fn read(buf: &'a [u8]) -> Option<Self> {
        if !(MIN_QUERY..=MAX_QUERY).contains(&buf.len()) {
            return None;
        }
        let rest = buf.strip_prefix(QUERY_MAGIC.as_slice())?;
        let (&version, rest) = rest.split_first()?;
        if version < 1 {
            return None;
        }
        let nonce: [u8; NONCE] = rest.get(..NONCE)?.try_into().ok()?;
        let (&count, rest) = rest[NONCE..].split_first()?;
        let count = usize::from(count);
        if count > MAX_TAGS {
            return None;
        }
        let (tags, _) = rest.get(..count * TAG)?.as_chunks::<TAG>();
        let (&len, rest) = rest[count * TAG..].split_first()?;
        if usize::from(len) > MAX_HEADSET_NAME {
            return None;
        }
        let name = std::str::from_utf8(rest.get(..len.into())?).ok()?;
        Some(Self { nonce, tags, name })
    }

    /// The tags, in their order.
    pub fn tags(&self) -> impl Iterator<Item = [u8; TAG]> + 'a {
        self.tags.iter().copied()
    }

    /// Whether it looks for the PC whose find secret is `secret`.
    pub fn looks_for(&self, secret: &FindSecret) -> bool {
        let ours = find_tag(secret, &self.nonce);
        self.tags.contains(&ours)
    }
}

/// A PC answering a query.
///
/// ```text
/// 0     4  "PRSA"
/// 4     1  version (1)
/// 5    16  the query's nonce
/// 21    8  here_tag of the PC's key and the nonce
/// 29    2  the PC's TCP port, little-endian
/// 31    1  flags (1: pairing open, 2: answers every query)
/// 32    1  name_len (0 to 64)
/// 33    n  the PC's name, UTF-8
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answer {
    pub nonce: [u8; NONCE],
    pub tag: [u8; TAG],
    pub port: u16,
    pub flags: u8,
    pub name: String,
}

impl Answer {
    /// The answer of the PC whose find secret is `secret` to `query`; `name` is cut to
    /// [`MAX_PC_NAME`] bytes.
    pub fn to(query: &Query, secret: &FindSecret, port: u16, flags: u8, name: &str) -> Self {
        Self {
            nonce: query.nonce,
            tag: here_tag(secret, &query.nonce),
            port,
            flags,
            name: cut(name, MAX_PC_NAME).to_owned(),
        }
    }

    /// Whether it comes from the PC whose find secret is `secret`.
    pub fn from_pc(&self, secret: &FindSecret) -> bool {
        here_tag(secret, &self.nonce) == self.tag
    }

    /// The datagram: [`MAX_ANSWER`] bytes at most.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(MAX_ANSWER);
        write(
            &self.nonce,
            self.tag,
            self.port,
            self.flags,
            &self.name,
            &mut out,
        );
        out
    }

    /// The datagram of the answer of the PC whose find secret is `secret` to a query
    /// with `nonce`, in `out` (cleared first): [`MAX_ANSWER`] bytes at most, `name` cut
    /// to [`MAX_PC_NAME`] bytes. Nothing new is made.
    pub fn write_to(
        nonce: &[u8; NONCE],
        secret: &FindSecret,
        port: u16,
        flags: u8,
        name: &str,
        out: &mut Vec<u8>,
    ) {
        write(nonce, here_tag(secret, nonce), port, flags, name, out);
    }

    /// Reads a datagram: `None` for anything that is not an answer. Bytes after the
    /// name are a newer version's and are skipped.
    pub fn decode(buf: &[u8]) -> Option<Self> {
        let a = AnswerRef::read(buf)?;
        Some(Self {
            nonce: a.nonce,
            tag: a.tag,
            port: a.port,
            flags: a.flags,
            name: a.name.to_owned(),
        })
    }
}

/// An answer as it lies in the datagram, read without copying (see [`Answer`] for the
/// layout): what a headset looks at before it takes one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnswerRef<'a> {
    pub nonce: [u8; NONCE],
    pub tag: [u8; TAG],
    pub port: u16,
    pub flags: u8,
    pub name: &'a str,
}

impl<'a> AnswerRef<'a> {
    /// Reads a datagram: `None` for anything that is not an answer. Bytes after the
    /// name are a newer version's and are skipped.
    pub fn read(buf: &'a [u8]) -> Option<Self> {
        let rest = buf.strip_prefix(ANSWER_MAGIC.as_slice())?;
        let (&version, rest) = rest.split_first()?;
        if version < 1 {
            return None;
        }
        let nonce = rest.get(..NONCE)?.try_into().ok()?;
        let tag = rest.get(NONCE..NONCE + TAG)?.try_into().ok()?;
        let rest = &rest[NONCE + TAG..];
        let port = u16::from_le_bytes(rest.get(..2)?.try_into().ok()?);
        let flags = *rest.get(2)?;
        let len = usize::from(*rest.get(3)?);
        if len > MAX_PC_NAME {
            return None;
        }
        let name = std::str::from_utf8(rest.get(4..4 + len)?).ok()?;
        Some(Self {
            nonce,
            tag,
            port,
            flags,
            name,
        })
    }

    /// Whether it comes from the PC whose find secret is `secret`.
    pub fn from_pc(&self, secret: &FindSecret) -> bool {
        here_tag(secret, &self.nonce) == self.tag
    }
}

/// An answer's datagram in `out`, cleared first.
fn write(nonce: &[u8; NONCE], tag: [u8; TAG], port: u16, flags: u8, name: &str, out: &mut Vec<u8>) {
    let name = cut(name, MAX_PC_NAME);
    out.clear();
    out.extend_from_slice(ANSWER_MAGIC);
    out.push(VERSION);
    out.extend_from_slice(nonce);
    out.extend_from_slice(&tag);
    out.extend_from_slice(&port.to_le_bytes());
    out.push(flags);
    out.push(name.len() as u8);
    out.extend_from_slice(name.as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    const PC: [u8; 32] = [7; 32];
    const OTHER: [u8; 32] = [9; 32];

    #[test]
    fn a_query_reads_back_and_finds_its_pcs() {
        let q = Query::new([&PC, &OTHER], "Quest 3S");
        let bytes = q.encode();
        assert_eq!(bytes.len(), MIN_QUERY, "padded to the smallest size");
        assert_eq!(&bytes[..4], b"PRSQ");
        let back = Query::decode(&bytes).unwrap();
        assert_eq!(back, q);
        assert!(back.looks_for(&PC));
        assert!(back.looks_for(&OTHER));
        assert!(!back.looks_for(&[1; 32]));
        // A new nonce each time: the tags differ, so nobody tells which PC it is.
        let again = Query::new([&PC], "");
        assert_ne!(again.nonce, q.nonce);
        assert_ne!(again.tags[0], q.tags[0]);
    }

    /// Fewer PCs than eight: random tags fill the query up, in a random order, so the
    /// count and the places tell nothing.
    #[test]
    fn a_query_always_carries_eight_tags_or_more() {
        let q = Query::new([&PC], "");
        assert_eq!(q.tags.len(), MIN_TAGS);
        assert!(q.looks_for(&PC));
        let none = Query::new([], "");
        assert_eq!(none.tags.len(), MIN_TAGS);
        assert!(!none.looks_for(&PC));
        let keys: Vec<[u8; 32]> = (0..9u8).map(|i| [i; 32]).collect();
        assert_eq!(Query::new(keys.iter(), "").tags.len(), 9);
        // The real tag is not always first.
        let first = (0..64)
            .filter(|_| {
                let q = Query::new([&PC], "");
                q.tags[0] == find_tag(&PC, &q.nonce)
            })
            .count();
        assert!(first < 32, "first {first} times of 64");
    }

    /// Made anew in the same memory, a query is as a new one; read in place, it says
    /// what the owned one says.
    #[test]
    fn a_query_made_anew_and_read_in_place() {
        let mut q = Query::new([&PC], "Quest 3S");
        let mut bytes = Vec::new();
        q.encode_into(&mut bytes);
        let first = q.nonce;
        let (tags_at, bytes_at) = (q.tags.as_ptr(), bytes.as_ptr());
        q.renew([&OTHER, &PC], "");
        q.encode_into(&mut bytes);
        assert_ne!(q.nonce, first);
        assert_eq!(q.tags.len(), MIN_TAGS);
        assert_eq!(q.name, "");
        assert_eq!(
            (q.tags.as_ptr(), bytes.as_ptr()),
            (tags_at, bytes_at),
            "the same memory"
        );
        let r = QueryRef::read(&bytes).unwrap();
        assert!(r.looks_for(&PC) && r.looks_for(&OTHER) && !r.looks_for(&[1; 32]));
        assert_eq!(r.tags().collect::<Vec<_>>(), q.tags);
        assert_eq!(Query::decode(&bytes).unwrap(), q);
        let mut answer = Vec::new();
        Answer::write_to(&q.nonce, &PC, 47830, flags::PAIRING, "desk", &mut answer);
        assert_eq!(
            answer,
            Answer::to(&q, &PC, 47830, flags::PAIRING, "desk").encode()
        );
    }

    #[test]
    fn a_query_with_every_tag_fits() {
        let keys: Vec<[u8; 32]> = (0..40u8).map(|i| [i; 32]).collect();
        let q = Query::new(keys.iter(), &"я".repeat(40));
        assert_eq!(q.tags.len(), MAX_TAGS, "at most 32 PCs");
        assert_eq!(
            q.name.len(),
            MAX_HEADSET_NAME,
            "the name cut at a character"
        );
        let bytes = q.encode();
        assert!(bytes.len() > MIN_QUERY && bytes.len() <= MAX_QUERY);
        assert_eq!(Query::decode(&bytes).unwrap(), q);
    }

    #[test]
    fn short_long_or_broken_queries_are_not_read() {
        let bytes = Query::new([&PC], "").encode();
        assert!(
            Query::decode(&bytes[..MIN_QUERY - 1]).is_none(),
            "127 bytes"
        );
        let mut long = bytes.clone();
        long.resize(MAX_QUERY + 1, 0);
        assert!(Query::decode(&long).is_none());
        long.truncate(MAX_QUERY);
        assert!(Query::decode(&long).is_some(), "padding up to 512 bytes");
        let mut magic = bytes.clone();
        magic[3] = b'A';
        assert!(Query::decode(&magic).is_none());
        let mut version = bytes.clone();
        version[4] = 0;
        assert!(Query::decode(&version).is_none());
        let mut count = bytes.clone();
        count[21] = 33;
        assert!(Query::decode(&count).is_none(), "more tags than allowed");
        let mut name = bytes.clone();
        let name_at = 22 + TAG * MIN_TAGS;
        name[name_at] = 33;
        assert!(Query::decode(&name).is_none(), "a name too long");
        let mut utf8 = bytes.clone();
        utf8[name_at] = 1;
        utf8[name_at + 1] = 0xff;
        assert!(Query::decode(&utf8).is_none(), "a name that is not UTF-8");
        // An old PC's beacon (`PROSTIR1`, its port and name) is no query.
        let mut beacon = b"PROSTIR1\xd6\xbadesk".to_vec();
        beacon.resize(MIN_QUERY, 0);
        assert!(Query::decode(&beacon).is_none());
        // A newer version adds after the fields of this one.
        let mut newer = bytes.clone();
        newer[4] = 2;
        newer[name_at + 4] = 0x55;
        assert_eq!(
            Query::decode(&newer).unwrap().tags,
            Query::decode(&bytes).unwrap().tags
        );
    }

    /// Read in place, an answer is from the PC whose secret made it, and not from
    /// another PC, nor an answer to another query.
    #[test]
    fn an_answer_is_from_the_pc_whose_secret_made_it() {
        let q = Query::new([&PC], "");
        let mut bytes = Vec::new();
        Answer::write_to(&q.nonce, &PC, 47830, 0, "desk", &mut bytes);
        let a = AnswerRef::read(&bytes).unwrap();
        assert!(a.from_pc(&PC));
        assert!(!a.from_pc(&OTHER));
        // The same tag with another nonce: not an answer to this PC's tag any more.
        let tag = a.tag;
        bytes[5] ^= 1;
        let elsewhere = AnswerRef::read(&bytes).unwrap();
        assert_eq!(elsewhere.tag, tag);
        assert!(!elsewhere.from_pc(&PC));
    }

    /// An answer with a short name reads; one of version 0 or with a name longer than
    /// a PC's does not; a newer version's is read by the fields of this one.
    #[test]
    fn an_answer_is_read_by_its_version_and_length() {
        let q = Query::new([&PC], "");
        let bytes = Answer::to(&q, &PC, 47830, flags::ANSWERS_ALL, "desk").encode();
        let a = AnswerRef::read(&bytes).unwrap();
        assert_eq!(
            (a.port, a.flags, a.name),
            (47830, flags::ANSWERS_ALL, "desk")
        );
        let mut old = bytes.clone();
        old[4] = 0;
        assert!(AnswerRef::read(&old).is_none(), "version 0");
        let mut newer = bytes.clone();
        newer[4] = 2;
        newer.extend_from_slice(b"more");
        assert_eq!(AnswerRef::read(&newer).unwrap().name, "desk", "version 2");
        let mut long = bytes;
        long[32] = MAX_PC_NAME as u8 + 1;
        long.resize(33 + MAX_PC_NAME + 1, b'x');
        assert!(AnswerRef::read(&long).is_none(), "a name too long");
        long[32] = MAX_PC_NAME as u8;
        assert_eq!(AnswerRef::read(&long).unwrap().name.len(), MAX_PC_NAME);
    }

    /// A name is cut at the edge of a character, never inside one, also when the
    /// limit falls in the middle of a two, three or four byte character.
    #[test]
    fn a_name_is_cut_at_a_character() {
        // Run where a cut that never ends cannot hold up the other tests: the result
        // comes back at once, or the test fails.
        let (send, receive) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut cuts = Vec::new();
            for prefix in ["", "Q", "QQ", "QQQ"] {
                for letter in ["Й", "€", "𝄞"] {
                    let text = format!("{prefix}{}", letter.repeat(40));
                    cuts.push((text.clone(), cut(&text, MAX_HEADSET_NAME).to_owned()));
                }
            }
            let name = format!("Q{}", "Й".repeat(40));
            let query = Query::new([&PC], &name);
            let _ = send.send((cuts, query, name));
        });
        let (cuts, query, name) = receive
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("the cut ends");
        for (text, cut) in cuts {
            assert!(cut.len() <= MAX_HEADSET_NAME, "{text}: {} bytes", cut.len());
            assert!(
                cut.len() > MAX_HEADSET_NAME - 4,
                "{text}: {} bytes",
                cut.len()
            );
            assert!(text.starts_with(&cut));
        }
        assert_eq!(query.name, format!("Q{}", "Й".repeat(15)));
        assert_eq!(query.name.len(), MAX_HEADSET_NAME - 1);
        let back = Query::decode(&query.encode()).unwrap();
        assert_eq!(back.name, query.name);
        assert!(name.starts_with(&back.name));
    }

    /// The real tag lands in every place about as often: where it is tells nothing.
    #[test]
    fn the_real_tag_is_anywhere() {
        const RUNS: usize = 2000;
        let mut at = [0usize; MIN_TAGS];
        let mut q = Query::new([&PC], "");
        for _ in 0..RUNS {
            q.renew([&PC], "");
            let ours = find_tag(&PC, &q.nonce);
            let place = q.tags.iter().position(|t| *t == ours).unwrap();
            at[place] += 1;
        }
        // 250 each on average; 150 to 350 is more than six standard deviations either
        // way, so a fair shuffle stays inside by far, and one that never leaves a tag
        // where it was (the real one starts first) does not.
        for (place, &count) in at.iter().enumerate() {
            assert!(
                (150..=350).contains(&count),
                "place {place}: {count} of {RUNS} ({at:?})"
            );
        }
    }

    /// Find secrets are random: two are never the same, nor all zeros.
    #[test]
    fn find_secrets_are_random() {
        let (a, b) = (new_secret(), new_secret());
        assert_ne!(a, b);
        assert_ne!(a, [0; 32]);
        assert_ne!(b, [0; 32]);
    }

    #[test]
    fn an_answer_reads_back_and_is_always_smaller_than_a_query() {
        let q = Query::new([&PC], "");
        let a = Answer::to(&q, &PC, 47830, flags::PAIRING, &"ж".repeat(100));
        assert_eq!(a.name.len(), MAX_PC_NAME);
        let bytes = a.encode();
        assert_eq!(bytes.len(), MAX_ANSWER);
        assert!(bytes.len() < MIN_QUERY);
        let back = Answer::decode(&bytes).unwrap();
        assert_eq!(back, a);
        assert!(back.from_pc(&PC));
        assert!(!back.from_pc(&OTHER));
        assert_eq!(back.nonce, q.nonce);
        // The tag of an answer is not the query's: a query cannot be sent back as one.
        assert!(!q.tags.contains(&back.tag));
        assert!(Answer::decode(&bytes[..bytes.len() - 1]).is_none());
        assert!(Answer::decode(&q.encode()).is_none());
        let mut more = bytes;
        more.extend_from_slice(b"later");
        assert_eq!(Answer::decode(&more).unwrap(), a);
    }
}
