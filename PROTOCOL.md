# Prostir protocol, version 2

This document describes how the PC program (`crates/prostir/`) and the Meta Quest app (`crates/prostir-client/`)
talk to each other. It is meant for anyone who wants to understand the connection or
write their own client or host. How programs on the PC talk to the PC program
(`prostir ctl`, the settings window, D-Bus) is in [LOCAL-API.md](LOCAL-API.md).

The source of truth is the code: [`crates/prostir-proto/src/lib.rs`](crates/prostir-proto/src/lib.rs) (messages),
[`crates/prostir-proto/src/state.rs`](crates/prostir-proto/src/state.rs) (the state each side keeps),
[`crates/prostir-proto/src/request.rs`](crates/prostir-proto/src/request.rs) (requests, commands and replies),
[`crates/prostir-proto/src/volume.rs`](crates/prostir-proto/src/volume.rs) (the shared volume),
[`crates/prostir-proto/src/wire.rs`](crates/prostir-proto/src/wire.rs) (how values are encoded),
[`crates/prostir-proto/src/packet.rs`](crates/prostir-proto/src/packet.rs) (packets),
[`crates/prostir-proto/src/handshake.rs`](crates/prostir-proto/src/handshake.rs) (the version handshake on records),
[`crates/prostir-proto/src/link/tcp.rs`](crates/prostir-proto/src/link/tcp.rs) (encryption),
[`crates/prostir-proto/src/secure.rs`](crates/prostir-proto/src/secure.rs) (keys and pairing) and
[`crates/prostir-proto/src/discovery.rs`](crates/prostir-proto/src/discovery.rs) (finding a PC).

Version 2 (Prostir 0.2) is a clean break from version 1 (Prostir 0.1.x): only the
version handshake is the same, so the two can tell the user which side to update
(see [The version handshake](#the-version-handshake-frozen)). Behaviour that comes from `crates/prostir/` or `crates/prostir-client/` rather than from `crates/prostir-proto/` is
what the current apps do; it is marked **implementation-defined** where another
implementation may reasonably differ.

Words used below:

- **host**: the PC program. It listens for connections.
- **client** or **headset**: the Quest app. It connects to the host.
- **display**: a virtual monitor, a window panel or a display of the PC itself (the
  laptop's panel, a monitor on a cable), identified by a `DisplayId`.
- **stream**: the encoded video of one display, from one `StreamStart` on (one
  generation).
- **part**: a piece of one side's state, sent whole whenever it changes.
- **frozen**: kept byte for byte in every protocol version, past and future.

Contents:

1. [Overview](#1-overview)
2. [Security](#2-security)
3. [Framing](#3-framing)
4. [Session flow](#4-session-flow): opening, state, requests and commands,
   capabilities, rights, volume, end of session
5. [Message reference](#5-message-reference)
6. [Streams, sound and microphone](#6-streams-sound-and-microphone)
7. [Layout](#7-layout)
8. [What a headset decides for itself](#8-what-a-headset-decides-for-itself)

## 1. Overview

### Transport

| What | Value |
|---|---|
| Connection | TCP, client connects to host |
| Host TCP port | `47830` (`TCP_PORT`; the host's `--port` option changes it) |
| Discovery | UDP port `47831` (`DISCOVERY_PORT`): the headset asks, the PC answers |
| Connections | Set up in parallel, up to 16 at once (implementation-defined) |
| Sessions | Several headsets may have one at once; the current host serves one |

The connection is plain TCP carrying encrypted records (see [Security](#2-security)
and [Framing](#3-framing)). There are two ways to reach the host:

- **Wi-Fi or Ethernet**: the client connects to the host's address on the local network.
- **USB**: the host keeps `adb reverse tcp:47830 tcp:47830` set up for every headset
  attached over USB, so the client reaches the host at `127.0.0.1:47830`. The protocol
  is the same. The host's `--no-adb` option turns this off. How the current host does
  it is implementation-defined: it learns of a headset plugged in from the kernel's USB
  events (every 10 s it looks instead where there are none) and of its USB debugging
  being allowed from `adb track-devices`, tries a failed `adb reverse` again after 5,
  10, 20 up to 60 s, and removes the reverses it set up when it quits with nothing left
  to listen on the port, so a headset is not left with a port that nobody answers on.
  Because a reverse can still outlive the program behind it (it was killed, or adb
  restarted), a client must treat a connection to `127.0.0.1:47830` that closes before
  the host says anything as "no PC on the cable", not as a PC that refused it.

### Discovery

A headset finds PCs by **asking** (the query below) on UDP port `47831`
(`DISCOVERY_PORT`). A PC sends nothing on its own: the beacon of earlier development
builds (`PROSTIR1`, its port and name, every second) is gone since step 7.9 of 0.2.0,
and a PC drops one from an older PC unread.

#### Asking

The headset sends a query (`PRSQ`) to port `47831`: as a broadcast, and straight to the
addresses of PCs it knows (an address typed in, or the last one it reached, a VPN
address such as Tailscale or WireGuard too). A PC answers (`PRSA`) to the address and
port the query came from, and only there. Nothing else is needed for a VPN: no UPnP,
no port opened on a router. Source: [`crates/prostir-proto/src/discovery.rs`](crates/prostir-proto/src/discovery.rs).

Query, at least 128 and at most 512 bytes:

| Offset | Size | Field |
|---|---|---|
| 0 | 4 | `PRSQ` in ASCII |
| 4 | 1 | version, `1` |
| 5 | 16 | nonce, random, new for every query |
| 21 | 1 | `count`, 0 to 32 (a headset sends at least 8) |
| 22 | 8 × `count` | a tag for each PC looked for, random ones up to 8, in a random order |
| . | 1 | `name_len`, 0 to 32 |
| . | `name_len` | the headset's name, UTF-8; empty except while it looks for a PC to pair with |
| . | . | zeros, up to 128 bytes at least |

`tag = SHA-256("prostir-find" ‖ find_secret ‖ nonce)[0..8]`, where `find_secret` is
the PC's find secret: 32 random bytes the PC gives only to the headsets it trusts, in
`FindSecret` right after the `Snapshot` of every session, and makes anew whenever it
forgets a headset (the others get the new one at their next session). Not the PC's
static key: Noise `XX` shows that key to anyone who connects, so it would let anyone
make tags. A headset makes a tag for each PC it trusts and keeps each PC's secret with
its key. A query of a newer version is read by the fields of version 1 (a newer
version only adds after them).

A trusted headset that was not in a session when the PC made a new secret still asks
with the one it has. The PC keeps that one valid for it (answering its tags, with the
same secret's tag in the answer) until a session of that headset gave it the current
one, or the PC forgot that headset too; the PC keeps at most a few such older secrets
(4, implementation-defined), a headset beyond them reaches the PC by its last address
(below). A headset the PC forgot may still get answers while an older secret lives,
but gains nothing by them: the handshake shows its key, which the PC no longer trusts.

Answer, at most 97 bytes:

| Offset | Size | Field |
|---|---|---|
| 0 | 4 | `PRSA` in ASCII |
| 4 | 1 | version, `1` |
| 5 | 16 | the query's nonce |
| 21 | 8 | `SHA-256("prostir-here" ‖ find_secret ‖ nonce)[0..8]` |
| 29 | 2 | the PC's TCP port, `u16` little-endian |
| 31 | 1 | flags: `1` pairing open, `2` the PC answers every query |
| 32 | 1 | `name_len`, 0 to 64 |
| 33 | `name_len` | the PC's name, UTF-8 |

The tag of an answer tells the headset which of the PCs it knows answered, before it
connects.

Whom a PC answers:

- every query while it has no paired headset, or while its pairing window is open
  ("Pair a new headset" in the tray, the settings window or `prostir ctl pair`, 3
  minutes, closed early by a pairing that succeeds or by "Stop pairing");
- otherwise only a query that carries its own tag.

What the tags hide, and what they do not:

- Someone without the PC's find secret cannot make a tag the PC answers: knowing its
  public key, or having connected to it, is not enough, and a headset the PC forgot
  can no longer make one (the secret changed).
- Someone listening does not learn which PC is looked for (the nonce changes every
  time), nor tell headsets apart by how many PCs they know (8 tags at least, random
  ones among them, in a random order); the PC does not learn which other PCs the
  headset knows. A query that gives no name (a paired headset without this PC's tag,
  say right after an update) brings no notice on a PC (see below).
- Not hidden: that a headset asks, from which address, its name while it looks for a
  PC to pair with, and that a PC answers (with its name and port). A query is not
  bound to a time: a PC answers each nonce once within 10 s (the same query sent as a
  broadcast and straight to it gets one answer), but someone who captured a headset's
  query can send it again later and get the same PC's answer, which tells them that
  PC is still there.

Limits: a datagram that is not a whole query of 128 to 512 bytes is dropped unread
(the host drops it in the kernel, with a socket filter); an answer is always smaller
than the smallest query, so a forged sender address gets less than it sent and a PC
cannot be used to amplify traffic. A query with the PC's own tag is always looked at
and answered once for each nonce, 100 a second at most; of the rest, a PC looks at 20
a second from one address and 200 in all at most and answers 20 a second at most
(implementation-defined; the others are dropped). A nonce answered is remembered for
10 s whatever comes: with no room left to remember one, a query gets no answer. Nothing a query says changes what a PC keeps: who is trusted, the find secrets and the pairing
window change only by the person at the PC or inside an encrypted session.

Neither is encrypted: an answer is a hint where to connect, and trust comes from the
handshake. Typing the PC's address and USB work whether the PC answers or not
(pairing is protected by its code).

**Implementation-defined**, the host: a query from a headset that is not its own and
looks for a PC to pair with (it gives its name), while the host does not answer it
(paired, pairing closed), shows a notification on the PC, "New headset nearby. Pair it?",
with a button that opens the pairing window, at most once a minute. A query without a
name (a paired headset that has no tag of this PC yet, say right after an update)
brings no notice. The headsets that asked with a name recently are the local API's
`nearby`. A query from one of its own headsets, answered, lets a sleeping PC start
ahead of the connection that follows.

**Implementation-defined**, a sleeping host: nothing of it wakes while nobody on the
network talks to it. Queries that get no answer wake only a light part of it (the
sockets' watcher, or under systemd a short-lived process that stays a minute after
the last query, so a stream of them does not hit systemd's limit on starts). Any TCP
connection to port 47830, though, wakes the whole program: under systemd it first
waits 3 s for the peer to say something and lets a silent one go (a port scan); the
sockets' watcher starts the program for any connection.

#### How the current client picks a host (implementation-defined)

Source: `crates/prostir-client/src/session/mod.rs` (whom and when) and
`crates/prostir-client/src/net/discovery.rs` (the queries). The headset names each PC it
trusts by a `PcId`: the first 8 bytes of the SHA-256 of the PC's public key (16 hex
digits in its files). It is never sent; the headset knows it from the handshake, and a
PC is the same PC at every address and over every link. The headset keeps, for each PC
it trusts, the find secret its sessions gave and the last address it had a session at
(written only once that PC proved its key in the handshake and its session opened;
never from an answer). A round of its search goes:

1. `127.0.0.1:47830` (USB), once its rest is over (see below);
2. a PC picked in the menu (or its address typed there);
3. a query, as a broadcast (to `255.255.255.255` and to the broadcast address of each
   interface) and straight to the last address of each trusted PC, the address in
   `host.txt` and the one picked in the menu, all on port `47831`; the answers that
   come within 300 ms count. An answer with the tag of a trusted PC leads to its
   connection at once;
4. a trusted PC that answered;
5. the address in `host.txt`;
6. a trusted PC's last address by TCP, at most every 10 s for each PC while it does
   not answer (a TCP connection wakes a sleeping PC whole, so not more often). This is
   how the first connection works after an update, before the headset has a find
   secret, and with a secret gone stale beyond what the PC keeps;
7. while it trusts no PC, the one PC that answered every query (unpaired, or its
   pairing window open), if exactly one did: a PC the headset does not trust is
   offered to the user, never paired on its own. With trusted PCs, such answers are
   only listed for the user to pick.

"Not now" to a PC offered leaves that PC alone (it is not dialed on its own until the
user picks something, and should it come again another way it is let go without
asking), while trusted PCs are still connected to by themselves: a trusted PC's answer
is matched to the PC declined by its tag, never by its address. Of the answers with
one trusted PC's tag, a query takes 4 at most, and one from the address the PC had its
last session at stays before one from elsewhere. Each
query goes from a new UDP port, so the port does not tie one query to the next.

A round goes once a second for the first 30 s of a search, then every 3 s. Nothing is
asked while the headset is off the head or while it is in a session with as many PCs as
it takes (one): a PC in use and the other PCs around are left alone. While no PC
answers, the search screen says what to do: with no trusted PC (or while the user picks
another), after 5 s, how to open pairing on the PC, to type its address (the PC's
settings window shows it where it pairs) or to use a USB cable; with trusted PCs, after
30 s without an answer or a connection, that the PC does not answer, to check that
Prostir runs there and is on the same network.

**The cable.** The cable's end is dialed first in every round, but it is not known whose
PC is there until the handshake shows the key. So the dial carries the PCs it may take
(`Takes`): any PC that is not already in a session or being connected to (a PC on Wi-Fi
gets no handshake each round); after "Change PC" or a forget, any but the PC just left
on the cable; for a PC the user picked (or, once a session opened from a pick, for the
PCs picked), that PC alone. A PC the dial may not take is let go right after the
handshake, before a word, and the cable then rests for 10 s, so a PC that is not wanted
is not knocked on every second; a pick ends the rest. A session running over Wi-Fi is
never moved to the cable by itself. The menu's list marks a PC the last dial found at
the cable's end, and picking it goes over USB to that PC alone; a mark that went stale
(the cable was pulled) costs one dial that finds nothing, and the pick then goes over
the network.

**A stale `adb reverse`.** A reverse that outlives the PC program makes
`127.0.0.1:47830` accept a connection that closes before any word. The client counts that as no PC at the cable: the round goes on with the next
step instead of starting over at USB, so the PC is reached on Wi-Fi.

**Picking and "Change PC".** A PC picked in the menu (or typed) is tried before anything
else, and the PCs a session opened from a pick for are the only ones the search then
connects to by itself (their answers, their last addresses, the cable when the
handshake shows one of them), until the user picks again or forgets one. "Change PC"
leaves the PC in use (goodbye `2` from the headset); nothing then connects on its own
over the network until the user picks, but the cable is still tried (a cable is a
choice of its own), except back to the PC just left on it. A PC the user forgot is not
connected to on its own either.

There is no probe of the local network: a home network carries the broadcast; a VPN
and separate subnets carry neither a broadcast nor a probe, but the direct query to
the last (or typed) address works there; client isolation on an access point blocks
all of them (a cable or another network helps); an access point that drops only
broadcasts is covered by an address typed once, kept as that PC's last address after
its first session.

### Connections and sessions

The host sets up each connection on a thread of its own (implementation-defined): the
Noise handshake, the version handshake, pairing and `Open`. A connection that waits for
the person at the PC, or sends its bytes slowly, holds only itself. At most 16
connections are set up at once; one more is closed at once. One pairing runs at a
time: a `PairBegin` while another connection pairs is answered with `Error` ("Another
headset is pairing with this PC right now") and `PairResult` without a tag, and the
connection stays open for another `PairBegin`. A pairing whose connection ends (the
headset hung up while the PC asks for its code) ends with it: its dialog closes, and a
headset that dials again (for the other way round, say) pairs at once.

A connection set up and trusted opens a session. The protocol lets several headsets
have a session at once, each with every display and its own state (see
[Rights](#rights)); the current host takes one: a session of another headset is turned
away with `Error` ("Another headset is already connected to this PC"), and the same
headset connecting again takes over its own earlier session.

## 2. Security

### Noise handshake

Every connection starts with a Noise handshake, before any protocol message:

```
Noise_XX_25519_AESGCM_BLAKE2s
```

- Pattern `XX`: mutual authentication; both static keys travel encrypted.
- DH: Curve25519. Cipher: AES-256-GCM. Hash: BLAKE2s.
- The client (the side that connects) is the **initiator**, the host the **responder**.
- No prologue and no pre-shared key. All three handshake messages carry an empty payload.

```
client -> host   e
host -> client   e, ee, s, es
client -> host   s, se
```

Each handshake message is sent as a 2-byte **big-endian** length followed by the Noise
message (at most 65535 bytes). After the third message both sides switch to transport
mode (see [Records](#records)).

The implementation uses the Rust crate `snow` 0.10.

### Static keys and identity

Each device has one long-term Curve25519 key pair (its *identity*), created on first use.

- Host: `~/.config/prostir/identity` (or `$XDG_CONFIG_HOME/prostir/identity`), one line
  `<private hex> <public hex>`, mode `0600`.
- Headset: a file of the same format inside the app's storage.

A device is identified only by its 32-byte public key, which the handshake reveals to
the other side.

### Trust

Each side keeps a *trust store*: one line per paired peer, `<public key hex> <label>`,
written atomically with mode `0600`. A peer is trusted if and only if its static public
key from the handshake is in the store.

- Host: `~/.config/prostir/paired_headsets`. The label is the `name` from `PairBegin`
  (or `headset` if empty). The tray item "Forget paired headsets" clears the file.
- Headset: a file inside the app. The label is the name the PC gives itself in its
  session (`PcInfo`), its IP address until then, or "PC (USB)". Settings → PC lists and
  forgets entries; forgetting a PC also drops its find secret and last address.

The host serves only trusted headsets. Before trust it sends nothing but `Welcome`
(its protocol version and interface language), `PairingRequired`, `PairMsg`,
`PairResult`, `Error` and `Incompatible`, and it creates no monitors. The current client pairs with an untrusted host only after the user agreed
in the menu (the agreement holds for 10 minutes); otherwise it closes the connection
right after the handshake and asks the user (implementation-defined).

### Pairing with a 6-digit code

Pairing runs inside the encrypted connection, after the handshake. One side shows a
random 6-digit code, the user types it on the other side, and both sides run SPAKE2
with it. The client chooses the direction in `PairBegin`:

| `headset_shows` | Who makes and shows the code | Who asks for it |
|---|---|---|
| `1` | Headset (in its menu) | Host, in a dialog (`kdialog`, `zenity`, `fuzzel`, `wofi` or `rofi`, the first one found) |
| `0` | Host (tray, desktop notification, log) | Headset, in its menu |

**Code.** Six decimal digits: a random `u32` modulo 1,000,000, zero-padded. It is
displayed as `123 456`. Before use, every character that is not an ASCII digit is
removed, and the remaining ASCII digits are the SPAKE2 password. A code made by the
host is reused for 5 minutes, across attempts and connections, so the code on screen
does not change while the user types it.

**SPAKE2.** Ed25519 group, *symmetric* mode, identity string `prostir pairing v1`, as
implemented by the Rust crate `spake2` 0.4 (compatible with `python-spake2`). Each
side's message is 33 bytes (`0x53` followed by a 32-byte point); the derived key is 32
bytes. With different codes the keys differ, which shows up in the confirmation step.

**Confirmation tags.** Each side proves it holds the same key, bound to this
connection:

```
tag(role) = HMAC-SHA256(key = SPAKE2 key,
                        "prostir confirm v1" || role || handshake_hash || lo || hi)
```

- `role` is the ASCII string `headset` for the client's tag, `pc` for the host's.
- `handshake_hash` is the Noise handshake hash of this connection (32 bytes for BLAKE2s).
- `lo` and `hi` are the two static public keys (32 bytes each), sorted in ascending
  byte order, so both sides compute the same value.
- Tags are 32 bytes and compared in constant time.

Because the tag covers the handshake hash and both static keys, a man in the middle
who runs two separate handshakes cannot relay a valid confirmation.

**Message sequence** (after `Hello` and `Welcome`, before the session is open; the
client starts it when it does not trust the host, or when the host answered `Open`
with `PairingRequired`):

```
client                                   host
  PairBegin{headset_shows, name}  ->
                                         obtains the code (dialog or its own)
                                  <-     PairMsg{host SPAKE2 message}
  PairMsg{client SPAKE2 message}  ->
  PairConfirm{tag("headset")}     ->
                                         checks the tag
                                  <-     PairResult{tag("pc") if the codes matched, else none}
  checks the host's tag
```

The two `PairMsg` messages are independent, so their order on the wire does not
matter: the current client sends its own as soon as it has the code. The host reads
the client's `PairMsg` and then `PairConfirm`; any other message in their place closes
the connection.

On success both sides add the other's static key to their trust store. The connection
stays open, and the client continues with `Open`.

If the host cannot obtain a code (the dialog was cancelled or timed out after 120 s, or
no dialog program is installed), it sends `Error{text}` and `PairResult` without a tag
and without a `PairMsg`. A client waiting for `PairMsg` treats that `PairResult` as a
failed attempt.

### Limits and timeouts

Host (implementation-defined values from `crates/prostir/src/pairing.rs`):

| Limit | Value |
|---|---|
| Noise handshake, `Hello` and `Open` | Must be done within 10 s in total, from the TCP connection on; each read waits at most 5 s; each write at most 5 s |
| While pairing | The deadline and the read timeout become 300 s, and go back to 10 s and 5 s after the attempt |
| `PairBegin` per connection | At most 3; a 4th closes the connection |
| Failed attempts in a row (all connections) | The first 3 are free. After the 4th, the next attempt has to wait 30 s, then 60 s, 120 s, 240 s, 480 s, and at most 900 s. A success resets the count |
| During a wait | `PairBegin` is answered with `Error` ("Too many wrong codes. Try again in N min.") and `PairResult` without a tag, and the connection is closed |

Every attempt that does not succeed counts as a failed one, once: a wrong code, a
cancelled dialog, a headset that hangs up while the host asks for the code, and an
exchange that breaks off. An attempt that showed nothing (the host has no dialog
program) does not count. When the headset shows the code and the host has none, the host reads and drops the
headset's `PairMsg` (sent right after `PairBegin`), so the next `PairBegin` can follow on
the same connection.

Client (implementation-defined, `crates/prostir-client/src/net/link.rs`): read timeout 10 s, 300 s while
pairing; each write at most 5 s; at most 3 attempts per connection, after which it
stops and asks the user again; it waits up to 300 s for the user to type a code shown
by the host, looking at the connection every half second meanwhile.

For the timeouts in the session see [End of session](#end-of-session).

### What is encrypted

Everything after the Noise handshake: every protocol message in both directions,
including `Hello`, pairing, video, PC sound and the microphone. Not encrypted: the
discovery queries and answers, and the Noise handshake messages themselves (which carry ephemeral keys in the clear and static
keys encrypted, as Noise `XX` does).

## 3. Framing

There are three layers. The transport carries encrypted **records**; each record after
the version handshake is one **packet**; packets carry **messages**, cut into pieces
when they are big, so that something urgent (sound, a menu answer) never waits behind
a whole video frame.

### Records

After the Noise handshake, everything travels in records:

```
+---------------------+---------------------------------------------+
| length u16 BE       | ciphertext = AES-256-GCM(plaintext) + 16-B tag |
+---------------------+---------------------------------------------+
```

- `length` is **big-endian** (unlike everything inside) and counts the ciphertext
  including its 16-byte tag; at most 65535.
- So one record holds at most 65519 bytes of plaintext.
- Nonces are not sent. Each direction has its own Noise cipher state, and the nonce is
  a counter starting at 0 for the first record in that direction and growing by 1 per
  record (standard Noise nonce encoding). There is no rekeying. A record that does not
  decrypt ends the connection.
- No associated data.
- The TCP connection closing between two records is a normal end of session; closing in
  the middle of a record is an error.

The version handshake comes first, in the stream format of protocol 1 (see
[The version handshake](#the-version-handshake-frozen)); every record after it is a
packet.

### Packets

```
+----------+---------+------------------------------------+
| flags u8 | lane u8 | body: the next bytes of a message  |
+----------+---------+------------------------------------+
```

- `flags`: bit 0 `FIRST` (the message's first packet), bit 1 `LAST` (its last one); a
  message in one packet has both. The other bits must be 0.
- `lane`: where the message is put back together. A lane holds at most one message at
  a time: its packets follow one another in that lane, and the next message in the
  lane starts only after the last packet of the one before. Packets of other lanes may
  come in between. A lane says nothing about what a message is (the tag does).
- `body`: the next bytes of the message, at least one. The first packet's body starts
  with the message's 5-byte header (tag and length, see
  [Protocol messages](#protocol-messages)), whole, so the receiver knows the length at
  once. The bodies of a message's packets joined are exactly its bytes.
- Order: the messages of one lane arrive in order; messages of different lanes in the
  order of their last packets.
- The sender chooses the lanes and how big each packet is (the current apps: 16 KiB of
  body, lane `0` for control messages, `1` for PC sound and the headset's up streams,
  and a free lane from `2` to `31` for each video stream). A receiver accepts any body
  size from 1 byte to the size of a record and any lane.
- A message with a tag the receiver does not know is put together and skipped.

The transport is reliable and ordered, so a packet out of place is a bug or an attack,
never a loss. Each of these ends the connection:

- flag bits other than `FIRST` and `LAST`, or a record too short for the 2-byte packet
  header;
- a packet with an empty body;
- a packet without `FIRST` in a lane that holds no message;
- `FIRST` in a lane that holds a message;
- a first packet whose body is shorter than the 5-byte message header;
- a message longer than the receiver's limit (below), checked on the first packet,
  before anything is allocated (as are a first packet with more bytes than the
  message, or one marked `LAST` with fewer);
- bytes beyond the length the header says;
- `LAST` before the message's end, or a packet that completes the message without
  `LAST`;
- more lanes holding a message at once than the limit, or more bytes in the messages
  being put together than the limit.

Limits:

| What | Before the session | In the session |
|---|---|---|
| Payload of a message from the host | 64 KiB | 64 MiB |
| Payload of a message from the headset | 64 KiB | 64 KiB |
| Lanes holding a message being put together at once | 1 | 32 |
| Bytes those messages say they hold, together | 64 KiB | 128 MiB from the host, 2 MiB from the headset |

Before the session the host may not be trusted yet, and the headset is not trusted
until it pairs, so what either can make the other allocate stays small. The biggest
headset messages in use, `Open` and a piece of microphone sound, are around 1 KiB. A
message complete in one packet takes no lane. The headset applies the session column
once it has sent `Open` (it sends it only to a host it trusts, so `Snapshot` need not
fit in 64 KiB), the host once it has sent `Snapshot`.

### Protocol messages

```
+--------+-------------------+---------------------+
| tag u8 | length u32 LE     | payload (length B)  |
+--------+-------------------+---------------------+
```

- `tag`: which message (see [Message reference](#5-message-reference)). Host→client tags
  are 1 to 40, client→host tags 101 to 141 (some of them retired).
- `length`: payload length in bytes, **little-endian**, at most the limit above.

### Encoding

A payload is the message's fields, one after the other in the order the reference
lists them, with nothing in between (`crates/prostir-proto/src/wire.rs`):

| Type | Bytes |
|---|---|
| `u8`, `u16`, `u32`, `u64` | Little-endian |
| `i32` | Little-endian, two's complement |
| `bool` | One byte, `0` or `1` |
| `opt<T>` | One byte: `0` (none), or `1` followed by a `T` |
| `str(max N)` | A `u16` count of bytes, then that many bytes of UTF-8; at most N bytes |
| `list<T>(max N)` | A `u16` count of items, then the items; at most N items |
| `bytes[N]` | Exactly N bytes, no count |
| `(A, B)` | An `A`, then a `B` |
| struct | Its fields in order |
| enum | A `u8` tag, then the fields of the variant with that tag |
| `str*`, `bytes*` | The rest of the payload, no count (only in the frozen messages and the media messages `Frame`, `Audio`, `Up`) |

Enum tags are fixed numbers written next to each variant in the code, so reordering
variants never changes the bytes. A request or command adds an envelope that a reader
can skip whole (see [Requests, commands and replies](#requests-commands-and-replies)).

Reading is strict, and any of these ends the connection:

- a payload shorter than its fields;
- bytes left over after the last field;
- a `bool` or `opt` byte other than `0` or `1`;
- an enum tag no variant has (an unknown codec, say), except the soft values listed
  under [Capabilities](#capabilities);
- a string that is not valid UTF-8 (also in the frozen messages);
- a string or list over its limit.

A message with a tag the reader does not know is skipped whole (its length is known),
see [Forward compatibility](#forward-compatibility). Unknown values inside a known
message are not skipped: they are errors.

## 4. Session flow

### Opening

```
client                                         host
  TCP connect                          ->
  Noise XX handshake (3 messages)      <->
  Hello{version, min, app}             ->
                                               not compatible:
                                       <-        Error{text}, Incompatible{protocol, app}, close
                                               compatible:
                                       <-        Welcome{version, language}
  [pairing, if the client does not trust the host and the user agreed]
  Open{parts}                          ->
                                               client not trusted:
                                       <-        PairingRequired
  [pairing, then Open again]
                                               cannot set up its displays:
                                       <-        Error{text}, close
                                               trusted:
                                       <-        Snapshot{parts}
                                       <-        StreamStart, Frame... in the stream lanes
  Arrange, parts, Request/Reply, Command/Reply, both ways
```

### The version handshake (frozen)

`Hello`, `Welcome`, `Incompatible` and `Error` keep the bytes they have in version 1 in
every version: the fixed fields, then a UTF-8 text running to the end of the payload
(`str*`). `Hello` is always the client's first message, before pairing or anything
else. So any two versions, older or newer, can finish this exchange and tell the user
which side to update. These four must never change; what a new version needs goes into
other messages.

| Tag | Message | Bytes |
|---:|---|---|
| 101 | `Hello` | `version:u32, min:u32, app:str*` |
| 1 | `Welcome` | `version:u32, language:str*` |
| 17 | `Incompatible` | `protocol:u32, app:str*` |
| 6 | `Error` | `text:str*` |

For example `Hello{version: 2, min: 2, app: "0.2.0"}` is
`65 0d 00 00 00 02 00 00 00 02 00 00 00 30 2e 32 2e 30`.

**In records.** These four go in the stream format of protocol 1, as every version
wrote them: the records joined into one byte stream, the messages one after the other
in it, across record boundaries (an empty record carries nothing here). What decides
where the stream ends:

- the client puts `Hello` in a record of its own, with nothing else in it; once the
  versions fit, the host requires that (a `Hello` that shares its record closes the
  connection);
- the host puts `Welcome` in a record of its own; a `Welcome` that shares its record
  closes the connection;
- the next record from the client after `Hello`, and from the host after `Welcome`, is
  the first [packet](#packets); everything from then on is packets, pairing included;
- `Error` and `Incompatible` end a connection whose versions do not fit, so they stay
  in the stream format; a client reads them across any record boundaries.

**Hello.** The client sends its newest protocol (`version` = `PROTOCOL_VERSION`), the
oldest it still understands (`min` = `MIN_PROTOCOL`) and its app version (for example
`0.2.0`).

**Version check.** The host decides with:

```rust
fn compatible(version: u32, min: u32) -> bool {
    version >= MIN_PROTOCOL && min <= PROTOCOL_VERSION
}
```

where `PROTOCOL_VERSION` and `MIN_PROTOCOL` are the host's own. In this version both are
`2`. There is no negotiated version: if the ranges overlap, both sides speak, and each
skips messages it does not know.

If the check fails, the host sends `Error` with a human-readable text (for clients
older than `Incompatible`), then `Incompatible{protocol: host PROTOCOL_VERSION, app:
host app version}`, and closes the connection. This happens before any trust check or
pairing, so an unpaired client also learns this. The client compares `protocol` with
its own `PROTOCOL_VERSION` to tell which side needs an update. The current client waits
10 s before trying again.

**Development builds.** Between releases the messages of version 2 change on the
development branch while the version stays 2. A development build says so in its app
version: `0.2.0-dev+p5` reads messages of revision 5 (`PROTO_REVISION`). A development
host turns away a `Hello` whose app version carries another revision (or none) the same
way as an incompatible one: `Error`, then `Incompatible`; the side with the lower
revision is the one to update. Released builds carry no revision and do not look at
it.

Within revision 6, `Decoder.max_w` and `max_h` changed their meaning (0.2.0 B6): they
were one box, every picture within it decoding in time; now they are the widest and the
tallest picture apart, and how many pixels a picture may have is the budget in
`DecoderPixels`. A host reads both: a size fits a decoder only within its sides and,
where it has a budget, within that budget. The revision stayed 6, as no development
build of the old meaning is in use. A headset that measured no budget for a decoder
says sides no larger than those taken for granted for its codec (4096x4096 for H.264,
8192x4320 for H.265), and no budget for it.

A first message other than `Hello` can only come from a version 1 client (Prostir
0.1.x), which may ask to pair before saying hello. The host answers it the same way
(`Error` saying to update the headset, then `Incompatible`) and closes the connection,
skipping the message unread.

**Welcome.** The versions fit. `version` is the host's `PROTOCOL_VERSION`; `language`
is the PC's interface language, `uk` or `en`, which the headset follows by default.
Nothing else starts yet.

**Trust.** If the client does not trust the host's key, it pairs now (only after the
user agreed in its menu; otherwise it closes the connection and asks the user). Then it
sends `Open` with every part of its state (see [State](#state)). The host answers
`PairingRequired` if it does not trust the client: the client may pair and send `Open`
again. Once the host trusts the client, it sets up its displays and encoders (which can
take a few seconds) and answers `Snapshot` with every part of its own state, and the
session starts: from then on both sides ping and count silence. Until then the host
accepts only `Open` and `PairBegin` (and skips unknown tags), and closes the connection
on anything else.

If the host cannot set up its displays at all, it answers `Error{text}` instead of
`Snapshot` and closes the connection. A display that does not start is not that: it
stands in `Displays` as `Failed`.

### What a version 1 peer sees

| Peer | What happens |
|---|---|
| 0.1.x client, trusts the host | Sends `Hello{1, 1}`. The host sends `Error` ("Update Prostir in the headset…") and `Incompatible{2, …}`; the client shows its "update the headset" page. |
| 0.1.x client, pairing first, code shown in the headset (its default) | Sends `PairBegin` and its `PairMsg` first. The host sends the same `Error` and `Incompatible` and closes. The client shows the error text, treats `Incompatible` during pairing as unexpected and reconnects, every few hundred milliseconds while the user's approval holds (10 min). |
| 0.1.x client, pairing first, code shown on the PC | Sends `PairBegin`, then waits for the user to type the code the PC would show, up to 300 s; the host shows none, only its "update the headset" notification, and closes. Once a code is typed the client reads the `Error` text, shows it and reconnects as above; after 300 s without one it gives up the attempt and reconnects. |
| 0.1.x host | Gets `Hello{2, 2}` and answers `Error` ("Update Prostir on the PC…") and `Incompatible{1, …}`; the client shows "update the PC". The host also shows a desktop notification. |

### State

Each side keeps a state the other one is told about, cut into **parts**: a part is a
message of its own (`Displays`, `Prefs`, …), sent whole whenever it changes, never as
a difference. At the start of a session the whole state goes as the parts inside one
message: `Open` from the headset, `Snapshot` from the host. A part inside them is

```
part: tag:u8, body:list<u8>(max 65535)
```

where `tag` and `body` are the tag and the payload the part has as a message of its
own, so a reader parses a part in `Snapshot` with the same code as the same part
arriving later. A part with a tag the reader does not know is skipped; a part twice in
one `Open` or `Snapshot` (of the same kind, for the parts kept per kind) is an error.

| Side | Parts | Kept |
|---|---|---|
| Host | `Displays`, `PcInfo`, `HostCaps`, `Access`, `Limits`, `ScaleAuto`, `Encoding`, `Sound`, `UpWanted`, `Focus`, `SoundFrom` | one of each; `UpWanted` one per kind |
| Headset | `HeadsetCaps`, `DecoderPixels`, `Prefs`, `Presence`, `Battery`, `Volume`, `UpState`, `Profiles` | one of each; `UpState` one per kind |

`HeadsetCaps` is always in `Open`, and `HostCaps` in `Snapshot`; neither changes in a
session. `DecoderPixels`, when the headset sends it, is in `Open` only too. The host sends `Displays` first and `Focus` and `SoundFrom` last, in one pass,
so a part that names a display comes after the `Displays` that lists it.

**Checked on arrival**, on both sides: the ids in `Displays` differ, `primary`, `Focus`
and `SoundFrom` are among them or none, every scale is above 0, a `Volume` is at most
100 %, an up stream is 48 kHz with 1 or 2 channels, a decoder is bigger than 0×0, a
pixel budget is above 0, and
an `Arrange` lists each display once. A state that breaks these is the other side's
bug: the session ends, as for a message that does not parse. Bad arguments of a
request are not: they get a failed reply.

The host cuts its texts (window titles, names, details) to their limits on a character
boundary when it builds its state, so a long text never makes a part impossible to
send.

### Requests, commands and replies

The headset changes the host's state with a `Request`, the host asks the headset to do
something with a `Command`. Both are an envelope:

```
Request / Command: req:u32, kind:u8, the kind's fields
Reply:             req:u32, outcome:Outcome
```

Each side counts its own `req` from 1 in every session, and each request or command
gets **exactly one** `Reply` with its number. A `kind` the reader does not know is
read with its fields skipped (the envelope runs to the end of the message) and answered
`Failed{UNSUPPORTED}`, so this holds between any two versions. A reply with a `req`
nobody asked is skipped and logged. A new field for a kind makes a new kind, never a
longer old one; a new `Outcome` comes only with a new kind.

**The state before the reply.** The side that answers first sends the parts the
request changed, then the `Reply`, in the same lane: when the headset reads
`Added{id}`, it has the `Displays` with `id` already. The same the other way: the
headset answers `Command FocusMode` after its `Prefs`, and `Command Profile` once the
profile is in place (after the host's reply to the `SetDisplays` it sent for it).

**Partial success.** A request that makes displays answers with all it made, also
those that did not start: they stand in `Displays` as `Failed` and keep their place.
It fails only when nothing was made (`LIMIT`, `BAD_SIZE`). The host may answer before
its desktop made them (implementation-defined): they stand in `Displays` as `Starting`
with the reply, and turn `Running` or `Failed` later. A request whose answer only the
desktop has (`DetachWindow`, `WindowHere`, `Screenshot`, `FindMouse`) is answered once
the desktop did it, or with `DESKTOP` when it does not within 20 s.

A command the headset has not answered within 30 s is closed on the host as "the
headset does not answer" (implementation-defined); a late reply is skipped.

### Capabilities

`HostCaps.features` and `HeadsetCaps.features` name what each side can do (numbers a
reader does not know are ignored). The rule: **a value of an enum goes on the wire only
when the other side named the capability it needs**; otherwise the sender falls back as
the table says. A new value later comes with a capability and a fallback of its own.

| Host features | | Headset features | |
|---:|---|---:|---|
| 1 | moves the mouse | 1 | takes commands |
| 2 | a screenshot of one display | 2 | one volume with the PC (see [Volume](#volume)) |
| 3 | window panels | 3 | the microphone as an up stream |
| 4 | displays of the PC itself | 4 | the camera |
| 5 | one volume with the headset (see [Volume](#volume)) | 5 | poses |
| 6 | notifications | 6 | says where the user looks (see [Gaze](#gaze)) |
| 7 | which display the sound comes from | 7 | shows displays of the PC itself |
| 8 | commands | 8 | shows window panels |
| 9 | "No limits" | 9 | wants a stream's rows bottom first (see [Frames](#frames)) |
| 10 | monitors with a scale | 10 | draws the PC's mouse pointer itself (see [The mouse pointer](#the-mouse-pointer)) |
| 11 | the microphone as an up stream | | |
| 12 | the camera | | |
| 13 | poses | | |
| 14 | takes `Gaze` (see [Gaze](#gaze)) | | |
| 15 | takes mouse buttons, the wheel and touches (see [Input from the headset](#input-from-the-headset)), named only to a headset with the input right | | |
| 16 | tells the mouse pointer apart from the pictures (see [The mouse pointer](#the-mouse-pointer)) | | |
| 17 | changes a monitor's scale in place; "Auto" (see [Scale in place and Auto](#scale-in-place-and-auto)) | | |

| Enum | Value | Needs | Without it |
|---|---|---|---|
| `Codec` | `H264` | nothing | |
| | `H265`, `Av1` | the codec in the headset's `decoders` | not in `Encoding.choices`, no stream with it; `Encoding.wanted.codec` is none when the saved choice is a codec this headset does not decode |
| `DisplayKind` | `Virtual` | nothing | |
| | `Window` | headset 8 | panels are not in `Displays` and do not stream; `DetachWindow` fails `UNSUPPORTED` |
| | `Physical` | headset 7 | displays of the PC are not in `Displays` and do not stream; `primary` is none when one of them is primary |
| `UpKind`, `UpFormat` | `Mic` / `Pcm16` | host 11 and headset 3 | no `UpWanted`, no microphone |
| | `Camera` / `Video`, `Poses` | host 12 / 13 and headset 4 / 5 | the same (not used yet) |
| `Primary` | `Display` | headset 7 (it names a display of the PC) | |
| `Outcome` | all | the request's kind | |
| `VolumeAsk` | | host 5 and headset 2, and only to the main headset | the PC's volume and the headset's are separate |
| `Gaze` | | host 14 | the headset sends none |
| `Frame` rows | bottom first | headset 9, on every headset with a session | top first |
| `Command` | | headset 1 | no commands |
| `MouseButton`, `MouseScroll`, `Touch` (and `Button`, `TouchPhase`) | all | host 15 | the headset sends none of them; `MouseTo` alone moves the mouse (host 1) |
| `CursorShape`, `CursorAt` | | host 16, and headset 10 named by every headset with a session | the pointer is in the pictures, as in every older build (see [The mouse pointer](#the-mouse-pointer)) |

Soft values need no capability: `role`, `permission` and `os` read an unknown value as
a guest, not known and another OS; unknown bits of `perms` and unknown `features` are
ignored; an unknown `ErrorCode` reads as `OTHER` and an unknown goodbye reason as `0`;
a `Request` or `Command` of an unknown kind gets `UNSUPPORTED`, an unknown preset
`BAD_ARGUMENT`.

### Rights

The host checks the rights of the headset (`Access`, told to it as a part). Without a
line of its own in the host's `settings.txt` a headset is the main one with every
right, so a single headset can do everything; a line of its own with a word that is
no role or right gives it no rights (a guest), never every right. A request without its right fails
`NOT_ALLOWED`; anything else without its right is dropped and logged.

One session at a time is the main one. Of several headsets whose line makes them the
main one, the session that was main already stays so, else the first connected; the
others are told `role` guest (with the rights of their line) until it ends, and then
the next is told `role` main. Where several headsets have a session at once (a host
may take only one), each gets every display and its own state; the host's commands
go to the main headset, else to the first connected.

| What | Needs |
|---|---|
| `AddDisplay`, `RemoveDisplay`, `SetDisplays`, `SetPrimary`, `ShowPhysical`, `SetScale`, `SetScaleAuto`, `SetEncoding`, `SetSound` | changes |
| `AutoScale` | changes, and the main headset (a guest's gets `NOT_ALLOWED`) |
| `DetachWindow` | changes and private |
| `WindowHere`, `MouseTo`, `MouseButton`, `MouseScroll`, `Touch`, `Prefs.gaze_mouse_ms` | input |
| `Screenshot`, `Prefs.notifications` | private |
| `FindMouse`, `RequestKeyframe` | nothing (keyframes at most every 300 ms for a display, from all sessions together) |
| `Gaze` | nothing, and the headset sends it only to a host that listed feature 14 |
| `UpStart` | the microphone (or the camera, by kind) |
| `Arrange`, `Volume` | the main headset: a guest's arrangement does not move the desktop, its volume does not move the PC's |
| `Presence` | anyone: the host sends no frames and no sound to a headset not worn, pauses the desktop once no headset is worn, and ends the session of one taken off (see [End of session](#end-of-session)) |
| `Prefs.no_limits` | changes the host's `Limits` only from the main headset |
| sent by the host | `Notice` and window titles only with private (else the title is empty) |

### Volume

One volume for the PC's slider and the headset's buttons, where both sides name the
capability (the host names it only to the main headset: a guest's volume is its own).
The current host names it (feature 5) when it has a sound device to follow it with; the
current headset names it (feature 2) only where its system lets the app set the media
volume and the level could be read at the start (some headsets fix it). The headset
owns the level (its system volume is the sound); `level` is a percent, `0..=100`, and
`muted` is the mute. Two counters keep the two from fighting: the headset's `seq` counts
only its own changes (its buttons, its system menu), the host's `ask` counts its
requests.

- **The headset changes it**: `seq + 1`, and `Volume{level, muted, seq, ask: 0}`. The
  host sets its sound device to it and remembers what it set.
- **The PC's device moves**: its first change after the host set it, to exactly that
  level and within a second, is the write's echo, and nothing happens; so is a change
  to where the device already was. Every other change is the person's, a 1 % one too.
  It asks once it passed the headset's level the way it went (up: above it, down:
  below it; the way from the person's previous move within a second, or from where
  the device stood, whichever is nearer), or changes the mute, or while a request is
  unanswered: `ask + 1`, and
  `VolumeAsk{level, muted, seen: the last seq from the headset, ask}`; while a slider
  is dragged, at most every 50 ms, with the newest level. Short of the headset's
  level, nothing is asked, and the device goes back to the headset's level once it
  rests: a slider dragged slowly past a headset's steps does not flip between two.
- **The headset gets `VolumeAsk`**: if `seen` is at least its own last `seq` (the host
  saw every change of the headset's), it takes the level, keeps `seq`, and answers
  at once `Volume{the level it took, seq, ask}`. Otherwise its own change wins, and it
  answers `Volume{its level, seq, ask}`. A headset whose volume goes in steps takes the
  next step the way the slider moved, so a slider moved by 1 % is not stuck between two
  steps (the rounding below); requests of one drag (less than 100 ms apart) go on from
  the level the last one asked for. The steps are the headset's own: they are not on
  the wire. Should its system then hold another level (a limit of its own), that is a
  change of the headset's own.
- **The host gets `Volume`**: if `ask` is older than its newest request not answered
  yet, it leaves the device alone (the slider is still moving, it does not jump back).
  Otherwise it sets the device to `level`, so the slider rests on the headset's step.
- `Volume` is in `Open`, so the PC takes the headset's level at the start. A host with
  other headsets in a session does not take a new one's level: it asks the new headset
  to take the PC's, so the sound the others hear does not jump.

**Rounding.** A headset whose system volume has `n` steps above silence says the level
of step `i` as `round(100 * i / n)` percent (halves up) and puts a level `p` at the
step `round(p * n / 100)` when it is not a move of the PC's slider. A request of the
PC's slider is rounded with its direction: asked higher than the headset's level, to
the step at or above the asked level (`ceil(asked * n / 100)`) and always at least one
step above where the headset is; asked lower, to the step at or below
(`floor(asked * n / 100)`) and at least one below; asked the same, nowhere. So a slider
moved by 1 % always moves a headset of 15 steps, and one dragged never goes back.
`volume::level_of`, `index_of` and `toward` in `crates/prostir-proto/src/volume.rs` are
these rules.

**Echo.** Writing a level to the PC's sound device makes the device report it. The
host takes the first change of the device after its write, to exactly the level it
wrote and within a second, as the write's echo, and asks nothing; so is a change to
where the device already was. A change that is not an echo (the person's, a 1 % one
too, from the slider, the volume keys or another program) is the person's, asked of
the headset as above. When the PC's own settings window sets the level, the device
goes there unmuted and the headset is asked for it as for a move of the slider, and
that write's echo asks nothing. Before the host has read its device, the level it
last wrote is the device's level, so a new main headset's sound device is not taken
for the person's move.

**Mute.** Muting is a change like any other. A headset whose system says step 0 while
it is muted (as Android does, and mutes when step 0 is set) says a muted volume at the
level last heard unmuted, with `muted: true`, so the PC keeps its slider where it was
and unmuting brings it back. Silence the app was asked for (level 0, not muted) is said
as `level: 0, muted: false`, so an unmute then stays unmuted. A request is put in
place in this order: the level, then the mute. The headset reads its volume every
250 ms while a session is open and every second without one (so `Open` says it as it
is), and not while it is not worn; a change it did not make itself is one of the
headset's own (`seq + 1`). These are implementation-defined.

`crates/prostir-proto/src/volume.rs` has both sides of this as plain functions.

### Gaze

`Gaze{display, u, v}` tells the host which of its displays the user looks at. It is for
a host that can use it, for example to give the display looked at the most bits. The
host says so with feature 14 (`HostCaps`), and the headset sends `Gaze` only to a host
that did; a host that did not name 14 never gets one. The headset names feature 6 for
what it can say, and the host does not need it. No right is needed.

- `display` is a display of this host's `Displays`, and `u`, `v` are where on its
  picture the user looks, from `0` to `65535` from the top left. `display` none, with
  `u` and `v` `0`, says the user looks at none of this host's displays (at another PC's
  monitor, or at nothing).
- It is a change, not a stream. The current headset says it only when the display
  looked at has changed and stayed the one looked at for 150 ms (a gaze that flickers
  over the seam between two monitors says nothing), and at most one message every
  250 ms, four a second. While the user looks at one display nothing is sent, so `u`
  and `v` are where the gaze was as it settled there.
- Each PC hears only about its own displays. When the gaze goes from one PC's monitor to
  another's, the first PC hears "none" and the second hears its display. A PC that is
  gone by then is not told, and a message to it does not count against the pace of the
  next one. A new session starts as if nothing was told, so a headset that is looking
  at a monitor says so soon after it.
- The headset's "where the user looks" is the head's forward ray, the same one its
  menu pointing and the mouse that follows the gaze work from, not eye tracking. It is
  worked out only while the headset app has the focus.
- The current host gives the display looked at every frame and the others fewer (half
  their rate, 5 a second where every headset on the head has `Prefs.focus_mode`); a
  display the gaze left keeps every frame for 2 s. Until a headset that names feature
  6 sends its first `Gaze`, and for a headset that does not name it, every display gets
  every frame. None of this changes a message: only the frames come less often. The
  rate and the 150 ms are the current headset's (implementation-defined); a host must
  take any rate.

### Steady state

Right after `Snapshot` every stream starts with `StreamStart` and a keyframe (see
[Streams](#6-streams-sound-and-microphone)). The host sends a part again whenever it
changes: `Focus` whenever the display under the mouse changes (checked every 150 ms),
`Displays` when a display comes, goes, moves on the desktop or changes its state.

The current client sends `Arrange` once the displays are placed and the arrangement
has held still for 800 ms, and a part of its state whenever it changes. Everything
else is sent when the user does something.

Both sides send `Ping{seq}` every second once the session is open (the host from
`Snapshot` on, the client from reading it), with `seq` counting up from 0, and answer
a `Ping` at once with a `Pong` carrying its `seq`. A peer must answer pings; it may
send its own at another rate or not at all, but then the other side sees silence
(below) when nothing else flows. The current apps answer from their reading thread, so
the round trip measures the network and not a busy main loop, and log it every 10 s.

The host ignores (and logs) known client messages that have no meaning in the session,
such as `Open` or `PairBegin`. The client ignores host pairing messages, `Welcome` and
`Incompatible` in the session.

### End of session

A side that ends a session on purpose sends `Goodbye{reason}` in the control lane,
after the control messages it still had to send and instead of any more sound or
video, and then sends nothing more. The other side closes the connection once it read
it. The side that said goodbye stops sending (a TCP half close) but still reads and
drops what arrives, until the other side closed the connection or for a short while
(1 s in the current apps), and then closes too: closing with unread data would reset
the connection, which can throw away the goodbye on its way (over `adb reverse` it
does). A reason a reader does not know reads as `0`.

| Code | From the host | From the headset |
|---:|---|---|
| `0` | Other | Other |
| `1` | The PC app is closing (tray, signal, the PC shutting down) | The headset app is closing |
| `2` | The headset was not worn for a while; it may connect again once it is worn | The user chose another PC |
| `3` | The PC forgot this headset (the tray's "Forget paired headsets"): pair again | The user forgot this PC in the headset |
| `4` | A new connection of this headset replaced this one | |
| `5` | The displays or encoders stopped; an `Error` with a text came first | |

What the current client shows: "The PC app was closed" for `1`, the pairing offer
for `3` ("The PC no longer knows this headset"), "The PC ended the connection" for
`0` and unknown codes, and nothing for `2`, `4` and `5` (whose `Error` it showed
already). The current host logs the headset's reason; its monitors stay for
`--linger` as after any session.

Without a goodbye, implementation-defined details of the current apps:

- Both: no record at all for 5 s ends the session (the other side pings every
  second, so only a dead link or a stopped peer is that quiet). The client then shows
  "No connection to the PC" and looks for the PC again.
- Both: a write blocked for 5 s ends the session.
- Host: TCP keepalive (idle 4 s, interval 2 s, 3 probes) and `TCP_USER_TIMEOUT` 10 s
  also notice a headset that vanished, already while pairing.
- Host: a headset reported as not worn (`Presence{worn: false}`) for 5 s has its
  session ended (with goodbye `2`). A headset that also said `Asleep` (and has not
  said `Presence{worn: true}` since) has it ended after 1 s instead. The host looks
  every 0.5 s, so either may take up to 0.5 s longer. The other sessions go on. When
  it was the last one, the host then removes its virtual monitors until a headset
  connects again.
- Host: after a session it keeps the virtual monitors for 30 s (`--linger`).

### Forward compatibility

- **Unknown tags must be skipped.** A reader that gets a tag it does not know reads
  and discards `length` bytes and goes on. Both sides do this, also before the session.
  The same holds for a part with an unknown tag inside `Open` or `Snapshot`.
- Unknown values inside a known message (an enum tag, an `opt` byte) are errors, not
  skipped, except the soft values of [Capabilities](#capabilities). A new value of an
  enum therefore comes with a capability, and is sent only to a peer that names it.
- A new field of a part makes a new part next to it (say `DisplayHdr{id, …}` next to
  `Displays`), never a longer old one; a new field of a request or command makes a
  new kind.
- So new optional things need no new protocol version: older peers skip them.
- Raise `MIN_PROTOCOL` (and `PROTOCOL_VERSION` with it) only for a change an older peer
  cannot just skip: a different layout or meaning of an existing message, a message the
  other side must understand for the session to work, or a change to framing, the
  handshake or pairing. Peers whose ranges no longer overlap then get `Incompatible`
  instead of misreading each other.
- Never reuse a tag number for a different message, and never change the
  [frozen messages](#the-version-handshake-frozen). Tags of messages of development
  builds of version 2 that are gone (`RETIRED_HOST_TAGS`, `RETIRED_CLIENT_TAGS`) are
  never used again.

## 5. Message reference

Payload notation: fields in order, `name:type`, with the types of
[Encoding](#encoding); the types below are defined in [Types](#types). Frozen messages
are marked as such (their bytes never change). "Part" marks a part of the state (see
[State](#state)). Lanes are those of the current apps.

### Host → headset

| Tag | Name | Payload | Meaning |
|---:|---|---|---|
| 1 | `Welcome` | `version:u32, language:str*` | Frozen. The versions fit. |
| 6 | `Error` | `text:str*` | Frozen. Something failed before or instead of the session (pairing, opening it, an incompatible version); shown in the headset menu. |
| 17 | `Incompatible` | `protocol:u32, app:str*` | Frozen. The versions do not fit; the host closes after it. |
| 9 | `PairingRequired` | (empty) | The host does not know this headset: pair first (the answer to `Open`). |
| 10 | `PairMsg` | `msg:bytes[33]` | The host's SPAKE2 message. |
| 11 | `PairResult` | `tag:opt<bytes[32]>` | Whether the codes matched: the host's confirmation tag if they did. |
| 28 | `Snapshot` | `parts:list<part>(max 65535)` | The session started: every part of the host's state. |
| 31 | `Displays` | `displays:list<Display>(max 32), primary:opt<DisplayId>` | Part. Every display there is (see [Displays](#displays)). |
| 32 | `Encoding` | `wanted:Wanted, using:opt<(Codec, bool)>, choices:list<(Codec, bool)>(max 64)` | Part. How the host encodes: what is wanted, what it encodes with now (`bool`: on the graphics card), what it could encode with. |
| 33 | `Sound` | `to_headset:bool, mic:MicDevice` | Part. Whether the PC's sound plays in the headset, and the PC's end of the headset microphone. |
| 34 | `Limits` | `virtual_max:u8, windows_max:u8, physical_max:u8, streams_max:u8, min:(u32, u32), max:(u32, u32), sizes:list<Mode>(max 64), scales:list<u16>(max 32)` | Part. What the host allows this headset: how many displays of each kind, how many streams, the smallest and largest monitor, the sizes and scales it offers. |
| 35 | `Access` | `role:u8, perms:u32` | Part. This headset's role (`0` main, `1` guest, anything else a guest) and rights (see [Rights](#rights)). |
| 36 | `PcInfo` | `name:str(64), app:str(32), language:str(8), os:u8, desktop:str(32)` | Part. The PC's name, app version, interface language, OS (`1` Linux, `2` Windows) and desktop (`KDE Plasma`, `Hyprland`). |
| 37 | `HostCaps` | `features:list<u16>(max 65535)` | Part, in `Snapshot` only. What the host can do (see [Capabilities](#capabilities)). |
| 16 | `Focus` | `display:opt<DisplayId>` | Part. The display the mouse is on; none: none of them. |
| 23 | `SoundFrom` | `display:opt<DisplayId>` | Part, while `Prefs.sound_from_screen` is on. The display whose window plays the sound; none: not known. |
| 40 | `UpWanted` | `kind:UpKind, on:bool` | Part, one per kind. A program on the PC records from the headset's microphone now: the headset streams it while its own switch is on. |
| 44 | `ScaleAuto` | `auto:list<DisplayId>(max 32)` | Part, from a host with feature 17 (an older headset skips it). The virtual monitors in "Auto": their scale is the main headset's choice (see [Scale in place and Auto](#scale-in-place-and-auto)). Never a display of the PC or a window panel. |
| 39 | `VolumeAsk` | `level:u8, muted:bool, seen:u32, ask:u32` | The PC's slider moved: take this level (see [Volume](#volume)). |
| 29 | `Reply` | `req:u32, outcome:Outcome` | The answer to the headset's `Request` `req`, after the parts it changed. |
| 30 | `Command` | `req:u32, kind:u8, fields` | Something the PC asks the headset to do (see [Commands](#commands)). Only to a headset with feature 1. |
| 38 | `Problem` | `error:ErrorCode, detail:str(512), display:opt<DisplayId>` | Something went wrong that nobody asked for and that does not last (what lasts is in the state: a display or the microphone `Failed`). |
| 2 | `StreamStart` | `id:DisplayId, generation:u32, w:u32, h:u32, codec:Codec` | The stream of display `id` starts, or starts over: a new generation. In the stream's lane. |
| 3 | `Frame` | `id:DisplayId, generation:u32, seq:u32, keyframe:bool, pts_us:u64, data:bytes*` | One encoded access unit, number `seq` of its generation from 0. In the stream's lane. |
| 4 | `StreamEnd` | `id:DisplayId, generation:u32` | The stream of this generation stopped. Never "the display is gone": only `Displays` says that. |
| 12 | `Audio` | `pcm:bytes*` | A slice of PC sound. Lane 1. |
| 20 | `Notice` | `text:str(8192)` | A desktop notification of the PC, while `Prefs.notifications` is on and the headset has the private right. |
| 41 | `FindSecret` | `secret:[u8; 32]` | Right after `Snapshot` in every session: the PC's find secret, which the headset makes its discovery tags for this PC with (see [Discovery](#discovery)); kept with the PC's key, replacing the one before. |
| 25 | `Ping` | `seq:u32` | Every second in the session; answer with `Pong` at once. |
| 26 | `Pong` | `seq:u32` | The answer to the headset's `Ping`. |
| 27 | `Goodbye` | `reason:u8` | The host ends the session on purpose (see [End of session](#end-of-session)). |
| 42 | `CursorShape` | `shape:u32, w:u16, h:u16, hot_x:u16, hot_y:u16, rgba:bytes*` | A look of the mouse pointer, numbered `shape` (not 0): `w`x`h` pixels (1 to 256 each way) of 8-bit RGBA in sRGB, not premultiplied, top row first; `hot_x`, `hot_y` (on the picture) is where it points. See [The mouse pointer](#the-mouse-pointer). |
| 43 | `CursorAt` | `display:opt<DisplayId>, u:u16, v:u16, shape:u32` | Where the pointer points: `u`, `v` across the picture of `display` (0 to 65535 from the top left, as in `MouseTo`), looking as `shape`; none: on no display streamed to this headset; `shape` 0: hidden. |

### Headset → host

| Tag | Name | Payload | Meaning |
|---:|---|---|---|
| 101 | `Hello` | `version:u32, min:u32, app:str*` | Frozen. The first message of every connection. |
| 109 | `PairBegin` | `headset_shows:bool, name:str(64)` | Start pairing; `headset_shows`: the headset displays the code and the PC asks for it, otherwise the other way round. |
| 110 | `PairMsg` | `msg:bytes[33]` | The headset's SPAKE2 message. |
| 111 | `PairConfirm` | `tag:bytes[32]` | Proof that the headset derived the same pairing key. |
| 127 | `Open` | `parts:list<part>(max 65535)` | Start the session, with every part of the headset's state (`HeadsetCaps` always). |
| 138 | `HeadsetCaps` | `features:list<u16>(max 65535), decoders:list<Decoder>(max 64), streams_max:u8, layers_max:u8, model:str(32)` | Part, in `Open` only. What the headset can do: features, a decoder for each codec with the widest and the tallest picture, streams it decodes at once, compositor layers it shows at once, its model. |
| 144 | `DecoderPixels` | `budgets:list<PixelBudget>(max 64)` | Part, in `Open` only (from 0.2.0 B6; an older host skips it and goes by the decoders' sides alone). How many pixels a picture may have for each decoder to decode 60 of them a second: a decoder's limit is pixels per frame and per second, not a box. The host sends a picture of `w`x`h` in a codec only when `w <= max_w`, `h <= max_h` and, where the codec has a budget, `w * h <= max_pixels`. |
| 134 | `Prefs` | `notifications:bool, sound_from_screen:bool, focus_mode:bool, gaze_mouse_ms:opt<u16>, no_limits:bool` | Part. The headset's settings the PC acts on: pass notifications, say where the sound comes from, fewer frames for displays out of focus, the mouse follows the gaze after resting this long, "No limits". |
| 115 | `Presence` | `worn:bool` | Part. Whether the headset is worn; streaming pauses while it is not. |
| 119 | `Battery` | `percent:u8, charging:bool` | Part. For the PC's tray. |
| 135 | `Volume` | `level:u8, muted:bool, seq:u32, ask:u32` | Part. The headset's volume (see [Volume](#volume)). |
| 136 | `UpState` | `kind:UpKind, switch:bool, permission:u8` | Part, one per kind. The headset's switch for its microphone, and whether the system lets it record: `0` not known, `1` yes, `2` no, `3` the system is asking the user now. |
| 143 | `Profiles` | `list:list<ProfileName>(max 32), active:opt<u8>` | Part (from 0.2.0 step 7.10; an older host skips it). The profiles saved in the headset, in the order its menu lists them, each `ProfileName` a `name:str(64)`, empty for one the menu calls by its number ("Profile 2"); `active`: the one in use (switched to or saved last, until something is changed by hand), by its place in `list`. Commands name a profile by its name or by its number from 1 in this order. |
| 139 | `UpStart` | `up:u8, kind:UpKind, format:UpFormat` | Up stream `up` starts. Lane 1. |
| 140 | `Up` | `up:u8, data:bytes*` | A piece of up stream `up`. Lane 1. |
| 141 | `UpEnd` | `up:u8` | Up stream `up` ended. Lane 1. |
| 133 | `Arrange` | `rows:list<Row>(max 32)` | How the displays stand around the user (see [Layout](#7-layout)). |
| 137 | `Gaze` | `display:opt<DisplayId>, u:u16, v:u16` | The display the user looks at, and where on it (0 to 65535 from the top left); none (with `u` and `v` 0): none of this host's displays. The headset sends it only to a host that listed feature 14, only when the display looked at has changed and rested a moment (not for a move across one display), and at most four times a second (see [Gaze](#gaze)). The current host gives the display looked at more frames. |
| 131 | `Request` | `req:u32, kind:u8, fields` | A change of the host's state (see [Requests](#requests)). |
| 132 | `Reply` | `req:u32, outcome:Outcome` | The answer to the host's `Command` `req`, after the parts it changed. |
| 124 | `MouseTo` | `id:DisplayId, u:u16, v:u16` | Put the mouse on display `id`, at `u`, `v` across its picture (0 to 65535). |
| 145 | `MouseButton` | `id:DisplayId, u:u16, v:u16, button:Button, down:bool` | Mouse button `button` down or up, the mouse at `u`, `v` across display `id` first. Only to a host with feature 15 (see [Input from the headset](#input-from-the-headset)). |
| 146 | `MouseScroll` | `id:DisplayId, dx:i32, dy:i32` | The wheel turned over display `id`, where the mouse is, in 1/120 of a notch: `dy` above 0 scrolls down, `dx` above 0 right. Only to a host with feature 15. |
| 147 | `Touch` | `id:DisplayId, slot:u8, u:u16, v:u16, phase:TouchPhase` | Finger `slot` (from 0, one number for each finger touching at once) at `u`, `v` across display `id`. Only to a host with feature 15. |
| 102 | `RequestKeyframe` | `id:DisplayId, generation:u32` | The decoder lost frames of this generation: a keyframe, please. |
| 103 | `FrameShown` | `id:DisplayId, generation:u32, seq:u32` | The decoder handed frame `seq` of this generation to the compositor. |
| 128 | `Ping` | `seq:u32` | As the host's. |
| 129 | `Pong` | `seq:u32` | The answer to the host's `Ping`. |
| 130 | `Goodbye` | `reason:u8` | The headset ends the session on purpose. |
| 142 | `Asleep` | | Right after `Presence{worn: false}`: the headset went to sleep, or the system put the app away, so it does not come back by itself soon; the next `Presence{worn: true}` undoes it. Not a part. |

### Types

| Type | Bytes | Notes |
|---|---|---|
| `DisplayId` | `u32` | A display of the PC. The host gives numbers out from a counter that never goes back (kept in its `screens.txt`), so a number is never another display's, in this run or a later one. |
| `Gen` (`generation`) | `u32` | One counter for every stream of a run of the host app, from 1, a new one at every `StreamStart`. |
| `ErrorCode` | `u16` | See [Error codes](#error-codes). |
| `Mode` | `w:u32, h:u32, scale:u16` | Pixels, and the scale in percent (100: one pixel of the picture is one of the desktop; 150: "larger text"). |
| `DeskRect` | `x:i32, y:i32, w:u32, h:u32` | Where a display is on the PC's desktop, in logical pixels. |
| `Display` | `id:DisplayId, kind:DisplayKind, name:str(64), mode:Mode, desk:opt<DeskRect>, state:DisplayState` | `desk` none: nowhere on the desktop now. |
| `DisplayKind` | `0 Virtual`, `1 Window{title:str(128)}`, `2 Physical{builtin:bool, connector:str(32), shown:bool}` | A virtual monitor; a small output for one window, shown as a panel (the title empty without the private right); a display of the PC itself (`builtin`: the laptop's own panel), streamed while `shown`. |
| `DisplayState` | `0 Starting`, `1 Running`, `2 Failed{error:ErrorCode, detail:str(512)}`, `3 Absent` | `Failed`: it could not start, keeps its place and is tried again. `Absent`: a display of the PC known but not connected now (the lid closed, the cable out); it keeps its number and place. |
| `Wanted` | `bitrate:u16, codec:opt<Codec>, software:bool, sharp:bool` | Mbit/s for each monitor; the codec chosen (none: one this headset does not decode); on the CPU; the slower preset for clearer small text. |
| `Codec` | `0 H264`, `1 H265`, `2 Av1` | |
| `Button` | `0 Left`, `1 Right`, `2 Middle`, `3 Back`, `4 Forward` | A mouse button. |
| `TouchPhase` | `0 Down`, `1 Move`, `2 Up`, `3 Cancel` | `Up`: the finger lifted, and what it did counts; `Cancel`: gone without counting (the hand lost, the finger through the display). |
| `MicDevice` | `0 Off`, `1 Ready`, `2 Failed{error:ErrorCode, detail:str(512)}` | The virtual microphone programs on the PC record from. |
| `UpKind` | `0 Mic`, `1 Camera`, `2 Poses` | |
| `UpFormat` | `0 Pcm16{rate:u32, channels:u8}`, `1 Video{codec:Codec, w:u32, h:u32}`, `2 Poses{hz:u8}` | The microphone: 48000 Hz, 1 or 2 channels. |
| `Decoder` | `codec:u8, max_w:u32, max_h:u32` | The codec as `Codec`'s number, kept raw so a codec the host does not know simply does not match; the widest and the tallest picture. |
| `PixelBudget` | `codec:u8, max_pixels:u32` | The codec as in `Decoder`; the most pixels of a picture its decoder takes at 60 frames a second. |
| `Row` | `entries:list<Placed>(max 32)` | |
| `Placed` | `id:DisplayId, center:opt<i32>, middle:opt<i32>` | See [Layout](#7-layout). |
| `Primary` | `0 Index{index:u8}`, `1 Display{id:DisplayId}` | The monitor made for `modes[index]`, or a display of the PC itself. |
| `Outcome` | `0 Done`, `1 Added{id:DisplayId}`, `2 Ids{ids:list<DisplayId>(max 32)}`, `3 Saved{path:str(1024)}`, `4 Pointer{id:DisplayId, u:u16, v:u16}`, `5 Failed{error:ErrorCode, detail:str(512)}` | |
| `part` | `tag:u8, body:list<u8>(max 65535)` | See [State](#state). |

### Displays

`Displays` lists every display there is: first in the order of the rows the headset
arranged (top to bottom, left to right), then those the rows leave out (window panels,
displays of the PC that are hidden or absent). It is the one source of which displays
exist. Within a session, a display leaves the headset only when it leaves this list;
a reconnect or a `Snapshot` never removes a place (numbers are not reused, so an old
place is in nobody's way), and a display of the PC that is not connected stays as
`Absent`. The host keeps the 8 of those seen most recently (implementation-defined); an
older one leaves the list for good. `Displays` never holds more than 32: should there be
more, absent displays of the PC are left out first.

A display of the PC keeps its number by its EDID (make and model as the desktop says
them), else by its connector: another screen in the same connector gets a new number
and a new place. One known by its connector that says its EDID later keeps its number,
and is known by the EDID from then on. Its `name` is what people call it: its maker and
model as it says them (`DELL U2720Q`, from step 7.9; the model alone when it starts
with the maker), else its connector, which `Physical.connector` always holds. A
headset shows the laptop's own panel (`builtin`) by its own words for it.

### Requests

| `kind` | Request | Fields | Success | Right | Typical failures |
|---:|---|---|---|---|---|
| 1 | `AddDisplay` | `mode:Mode` | `Added` | changes | `LIMIT`, `BAD_SIZE` |
| 2 | `RemoveDisplay` | `id:DisplayId` | `Done` | changes | `UNKNOWN_DISPLAY`, `LAST_DISPLAY` |
| 3 | `SetDisplays` | `modes:list<Mode>(max 32), primary:opt<Primary>, shown_physical:list<DisplayId>(max 32)` | `Ids`, in the order of `modes` | changes | `LIMIT`, `BAD_SIZE`, `UNKNOWN_DISPLAY` |
| 4 | `SetPrimary` | `id:DisplayId` | `Done` | changes | `UNKNOWN_DISPLAY` |
| 5 | `ShowPhysical` | `id:DisplayId, shown:bool` | `Done` | changes | `UNKNOWN_DISPLAY`, `LIMIT`, `CAPTURE` |
| 6 | `SetEncoding` | `bitrate:opt<u16>, codec:opt<Codec>, software:opt<bool>, sharp:opt<bool>` (none: as it is) | `Done` (what came of it is in `Encoding`: a codec another headset with a session does not decode stays chosen, in `wanted`, while the host encodes H.264, in `using`, until every headset decodes it) | changes | `ENCODER`, `BAD_ARGUMENT`, `BAD_SIZE` (a display streaming now would be larger than the headset decodes in the new codec: nothing changes) |
| 7 | `SetSound` | `to_headset:bool` | `Done` | changes | `SOUND` |
| 9 | `DetachWindow` | | `Added` | changes, private | `NO_WINDOW`, `LIMIT` |
| 10 | `WindowHere` | `id:DisplayId` | `Done` | input | `NO_WINDOW`, `UNKNOWN_DISPLAY` |
| 11 | `Screenshot` | `id:DisplayId` | `Saved` | private | `CAPTURE` |
| 12 | `FindMouse` | | `Pointer` | none | `NOT_ON_DISPLAYS` |
| 13 | `SetScale` | `id:DisplayId, scale:u16` | `Done`: a host with feature 17 gives the scale in place (the monitor stays `Running`, its stream and windows as they are); an older one makes the monitor anew at `scale` under the same number (`Starting` in the list that comes with the reply, then `Running`). The monitor leaves "Auto". | changes | `BAD_SIZE` (a scale not in `Limits.scales`, or one the desktop does not give that size), `UNKNOWN_DISPLAY` |
| 14 | `AutoScale` | `id:DisplayId, scale:u16` | `Done`: the main headset's choice for a monitor in "Auto", given in place; for one not in Auto nothing changes | changes, main headset | `BAD_SIZE`, `BAD_ARGUMENT` (a display of the PC or a window panel), `UNKNOWN_DISPLAY`, `NOT_ALLOWED` |
| 15 | `SetScaleAuto` | `id:DisplayId, on:bool` | `Done` ("Auto" on or off; `ScaleAuto` comes with the reply) | changes | `BAD_ARGUMENT` (a display of the PC or a window panel), `UNKNOWN_DISPLAY` |

`SetDisplays` sets the monitors of a profile, left to right: monitors of a size that
is there already are kept with their windows; `shown_physical` are the displays of the
PC to show, the others are hidden. `SetScale` is from 0.2.0 step 7.10: a host before it
answers it `UNSUPPORTED`, as any kind it does not know; a headset offers it only to a
host that names capability 10 (monitors with a scale). `AutoScale` and `SetScaleAuto`
are from 0.2.0 too, sent only to a host that names capability 17; an older host
answers them `UNSUPPORTED`. Kind 8 is retired
(`RETIRED_REQUEST_KINDS`) and never used.

A display of the host seen for the first time is shown in the headset, while the
person wants the host's displays there, fewer than the host's `physical_max`
without "No limits" are shown, and a headset connected then decodes one more stream:
at first every display of the host shows, standing to the others as on its desktop
(`Display.desk`; side by side where they cover each other, as a cloned desktop's do),
and the person hides those not needed.

### Scale in place and Auto

A host that names feature 17 changes a virtual monitor's scale in place: its output,
its stream (no new generation) and its windows stay, and the desktop lays the outputs
out again for the new size in the same step. It keeps for each virtual monitor whether
it is in "Auto" (`ScaleAuto`): a monitor made from then on is, until a scale is chosen
by hand (`SetScale`, the PC's window or the desktop's own display settings); monitors
kept from a version before are not. The headset turns Auto on or off with
`SetScaleAuto`.

For a monitor in Auto, the main headset chooses (implementation-defined; the current
headset): `k`, headset pixels per logical pixel of the desktop, is the headset pixels a
pixel of the stream takes where the monitor is shown, times the scale. It asks with
`AutoScale` for the smallest of `Limits.scales` (100 % and above) at which `k` is at
least 0.85; where none is, the largest. Monitors glued edge to edge in the headset get
scales chosen together, so their `k` is as close as the steps allow (one scale for
neighbours of one size), the seam between them seamless. It asks only after the
arrangement held for 0.8 s with nothing in a hand, goes up only once `k` fell below
0.82 and down only where `k` stays at 0.88 or more, asks a monitor at most every 3 s,
and does not ask again a scale the host did not give while the monitor keeps its size.
A headset whose pixels per degree it does not know asks nothing; a guest asks nothing.
A monitor it adds in Auto it asks for at the scale its place wants already
(`AddDisplay`'s `mode.scale`), so it needs no second change; one added at a scale chosen
by hand it takes out of Auto (`SetScaleAuto`) once made. A profile keeps which of its
monitors were in Auto: switched to, the headset turns Auto on for those and gives the
others the profile's scale.

The host gives a scale its desktop does not take for that size as the nearest it takes
among `Limits.scales`, the larger of two as near (Hyprland takes only scales that make
whole logical pixels: 2560x1440 at 150 % is not one). A scale the desktop refuses
leaves the monitor at the one it has, which `Displays` shows, and the layout goes on
without it.

### Commands

| `kind` | Command | Fields | What the headset does |
|---:|---|---|---|
| 1 | `Recenter` | | Brings everything in front of the user. |
| 2 | `Profile` | `name:str(64)` | Switches to a profile: `name` is a number only, its number from 1 as the headset's `Profiles` lists them; else the name it was given, exactly; else the number after the word the headset's menu calls a profile by ("Profile 2") in a language it has (`NO_SUCH_PROFILE`); answers once it is in place. |
| 3 | `Cinema` | `on:bool` | Cinema mode. |
| 4 | `FocusMode` | `on:bool` | "Focus" (changes `Prefs`). |
| 6 | `Preset` | `preset:u8` | A preset arrangement (`BAD_ARGUMENT` for one it does not know). |
| 7 | `LineUp` | | "Line up". |
| 8 | `ShowMouse` | | Shows where the mouse is. |
| 10 | `Follow` | `on:bool` | Follow the head. |
| 11 | `ResetArrangement` | | The monitors as the chosen preset places them, in front of the user, at their standard size (cinema makes its view anew); answered after they moved. |
| 12 | `NoLimits` | `on:bool` | "No limits" (changes `Prefs`). |
| 13 | `RenameProfile` | `profile:str(64), name:str(64)` | Gives profile `profile` (named as for `Profile`) the name `name`, on one line, without spaces around it or characters that turn text around; an empty one gives it back its number (`NO_SUCH_PROFILE`; `BAD_ARGUMENT` for a number only, which names a profile by its place; `NOT_ALLOWED` from a host the headset's menu does not act on). The headset's `Profiles` part shows it before the answer. From 0.2.0 step 7.10; an older headset answers `UNSUPPORTED`. |
| 16 | `Lock` | `on:bool` | "Locked" on or off: the monitors stay where they are (from 0.2.0 step 8; `UNSUPPORTED` in the current client until its step). |

Kind 9 is retired (`RETIRED_COMMAND_KINDS`): it set the headset's microphone switch in
development builds, and is never used again. The switch is the user's, changed only in
the headset; a headset answers kind 9 `UNSUPPORTED` like any kind it does not know.

### Error codes

A code a reader does not know reads as `OTHER`, so a new code needs no new protocol
version. The headset shows its own text for a code it knows, in its language, with the
`detail` (technical, in the PC's language) under it; for `OTHER`, only the detail.

| Code | Name | Meaning |
|---:|---|---|
| 0 | `OTHER` | Something else: only the detail says what. |
| 1 | `UNSUPPORTED` | A request or command this side does not know or cannot do. |
| 2 | `NOT_ALLOWED` | This headset has no right to it. |
| 3 | `UNKNOWN_DISPLAY` | No such display (any more). |
| 4 | `LIMIT` | As many as the host allows. |
| 5 | `BAD_SIZE` | A size or scale the host does not make, or a picture larger than the headset decodes. |
| 6 | `LAST_DISPLAY` | The last monitor cannot be removed. |
| 7 | `NO_WINDOW` | No active window on the PC. |
| 8 | `DESKTOP` | The desktop did not do it (an output not made, KScreen stuck). |
| 9 | `ENCODER` | The encoder could not start. |
| 10 | `CAPTURE` | The picture of a display could not be taken. |
| 11 | `SOUND` | Sound could not be routed. |
| 12 | `NOT_ON_DISPLAYS` | The mouse is on none of the displays. |
| 13 | `NO_SUCH_PROFILE` | No profile saved in that slot. |
| 14 | `BAD_ARGUMENT` | An argument out of range. |

## 6. Streams, sound and microphone

### Streams and generations

Every display in `Displays` that is `Running` streams its picture: a virtual monitor,
a window panel, or a display of the PC itself while it is shown. A stream is named by
its display's id and a **generation**: the host counts generations from 1 over a run
of its app, a new one at every `StreamStart`. A frame, its acknowledgement and a
request for a keyframe carry the generation, so nothing of an earlier start of a
stream is taken for the new one. The host ignores `RequestKeyframe` and `FrameShown`
of a generation that is not the current one; the headset ignores `Frame` and
`StreamEnd` of a generation it did not see start.

The host sends `StreamStart` before the first `Frame` of a stream on this connection,
and again (a new generation) when the encoded size or the codec changes, and for every
stream at the start of each session. A client that gets `StreamStart` for a display it
streams already replaces that stream (new decoder, new size). `StreamStart`,
`StreamEnd` and the frames of a stream go in a lane of their own, which the host picks
from the free lanes `2` to `31`, so they never overtake a frame of the old
configuration on its way. Video frames still waiting on the host are discarded when
`StreamStart` or `StreamEnd` is queued, so no frame of the old configuration follows.

The first `Frame` of a generation is a keyframe, and `seq` counts its frames from 0. A
keyframe that a newer one replaced on the host is not sent (see
[When frames are late](#when-frames-are-late)), so the first frame to arrive may have a
higher `seq`; numbers only go up.

**Lanes are not ordered with each other.** `Displays` goes in the control lane, a
stream in its own, so the host holds a `StreamStart` back (and drops the frames before
it) until the `Displays` it sent this session lists the display. A client still
tolerates a `StreamStart` for a display it does not know: it may set up the decoder
and show nothing until `Displays` lists it.

**No stream larger than the headset decodes.** The host never sends a `StreamStart`
larger than the headset's decoder for its codec (`HeadsetCaps.decoders`; H.264, which
every headset decodes, at any size when the headset names no decoder for it). A
display whose picture would be larger (a display of the PC, a monitor kept from a
session in H.265 when the PC now encodes H.264) stands in `Displays` as
`Failed{BAD_SIZE}` with its size in the detail, sends nothing, and is tried again with
the next headset or codec; a window panel that large is closed, with a
`Problem{BAD_SIZE}`. With several headsets this is each headset's own: a display
another headset decodes keeps streaming to that one and stands `Failed{BAD_SIZE}`
only in the `Displays` of the headset that does not; the host encodes in a codec every
headset with a session decodes (H.264 when one does not decode the one chosen). A client that still gets a stream it cannot decode (from an older
host) shows nothing of that display and goes on with the others.

At most `Limits.streams_max` streams go at once (30 at most: a lane each; never more
than the headset's `HeadsetCaps.streams_max`): a request that would make more displays
stream (a monitor, a window panel, a display of the PC shown) fails `LIMIT`. Displays
that stream beyond what a headset takes (more made by another headset, or kept from
one that takes more) stand `Failed{LIMIT}` in that headset's `Displays`, in the order
of the list, with no `StreamStart` for it; they stream to it once fewer are ahead of
them. A display
whose stream cannot start (its output or encoder fails) stands in `Displays` as
`Failed`, without a `StreamStart`, keeps its place and is tried again; a display
removed leaves `Displays`, and its `StreamEnd` and the `Displays` without it may come
in either order.

### Frames

- `data` is exactly one access unit as an **Annex B** byte stream (start codes, no
  length prefixes).
- Keyframes carry the parameter sets in-band: SPS and PPS for H.264, VPS, SPS and PPS
  for H.265, in front of the IDR picture.
- `keyframe` is `1` for frames that can be decoded on their own.
- No B-frames: decode order is display order.
- Rows go top first, except to a headset that names capability 9: its compositor shows
  a stream's rows bottom first (an Android surface without a way to turn a layer
  over, which the current headset finds out from the compositor, not from its model), so
  the host sends the picture upside down, the bottom row first, and it shows the right
  way up there. The picture is a decoded one like any other: the headset does nothing
  else to it, and where the user points at it is the same. One picture goes to every
  headset with a session, so the host turns it over only when all of them name 9; when
  that changes (a headset that does not name it joins or leaves), the encoders start
  again and the streams begin with a new `StreamStart`. The host turns the picture over
  in its colour conversion (implementation-defined): where an encoder's converter
  cannot do it (a VA-API card whose converter does not keep the direction), that
  encoder is not used for such a headset and the host takes the next way to encode.
- `pts_us` is the presentation timestamp in microseconds from the host's capture clock
  (0 if the encoder gave none). Its starting point is implementation-defined; use it to
  order frames and for latency figures, not as wall-clock time.
- Keyframes come mostly on request (at session start, after loss, after
  `RequestKeyframe`); otherwise the encoders put at most 600 frames between two
  keyframes (implementation-defined).

### Feedback from the headset

- **`FrameShown{id, generation, seq}`**: the current client sends it for a decoded
  frame at most every 100 ms per stream. The host treats every earlier frame of that
  generation as done too, and uses the acknowledgements only for its latency figures
  in the log. They are not flow control: a host must not wait for them.
- **`RequestKeyframe{id, generation}`**: the client sends it when it has had to drop
  frames (its queue for that decoder, 32 frames in the current client, was full) or
  its decoder is waiting for a keyframe; it then drops non-key frames of that stream
  until a keyframe arrives. The current client asks at most every 300 ms per stream,
  and the host takes at most one request every 300 ms per display, from all sessions
  together. The host asks its encoder for a keyframe.

### When frames are late

The host picks every packet it sends (implementation-defined, `crates/prostir/src/outbox.rs`),
in strict priority:

1. control messages (everything except `Audio`, `Frame`, `StreamStart`, `StreamEnd`
   and `CursorAt`), in order, in lane `0`; never dropped, but over 4 MiB of them
   waiting means the headset reads nothing, and the session ends;
2. `CursorAt`, once no control message waits, in lane `0` in one packet: only the
   newest waits, a newer one takes its place (so after a slow moment the pointer jumps
   to where it is, never replaying where it was), and a `CursorShape` queued before it
   always arrives first;
3. sound (`Audio`), in lane `1`, at most 8 slices waiting; the oldest is dropped when a
   newer one arrives;
4. video, each stream in a lane of its own, the streams taking turns a packet each.

A message it started is always finished; one of a higher class goes between its
packets. So sound waits behind at most one packet (16 KiB) of a big keyframe, and one
stream's keyframe does not hold up the others.

A non-key video frame that has waited more than 120 ms is not sent. The host then
drops every later frame of that stream until the next keyframe (a decoder cannot use
them without the lost one) and asks its encoder for a keyframe, at most every 300 ms.
Keyframes are never dropped for being late, and a keyframe queued drops the frames of
its stream still waiting before it (it replaces them). If more than 16 MiB of video is
waiting, new non-key frames are refused the same way. Other streams are not affected.

The host also paces the connection (`SO_MAX_PACING_RATE`, `crates/prostir/src/pacing.rs`), so
a keyframe does not land in the Wi-Fi access point's queue all at once with sound
stuck behind it, where nothing can overtake it any more. TCP hides lost packets behind
retransmissions, so the sign of a full link is that queue: the round trip (TCP's, and
the pings') rising clearly over its lowest of the last 10 s (by 10 ms, or half the
lowest if that is more) while the pace held the host back. The host is not paced until
that happens; then it paces 15 % under what the network delivered just then, and holds
that. Every 3 s it tries a pace a tenth higher; once the network delivered that for
200 ms with the round trip low, it holds the new pace and tries the next tenth, and a
rise brings back the pace that was fine. Whatever the link seems to carry, it never
paces under a fifth more than what was queued for the headset over the last 2 s (and
never under 20 Mbit/s): a link slower than the video is for the bitrate to adapt to,
not for pacing to starve. Nothing is paced over USB. It keeps little unsent data in the
kernel (`TCP_NOTSENT_LOWAT` 32 KiB) for the same reason.

These values are implementation-defined; the rule a client can rely on is that
**after a gap in a stream, the next frame it gets for that stream is a keyframe**.

### The mouse pointer

A host that can tell the mouse pointer apart from the pictures (its desktop says how
it looks and where it is) names feature 16. The picture of a display is one for every
headset, so the host leaves the pointer out of it only while **every** headset with a
session names feature 10; otherwise the pointer is drawn into the pictures as in every
older build, and no `CursorShape` or `CursorAt` goes to anyone. A headset without 10
coming in the middle of a session brings the pointer back into the pictures: the
others get `CursorAt` with `display` none and `shape` 0 first. Desktops that choose
this when a stream is made start their streams over (a new generation) to switch.

While the pointer is apart, the host sends:

- `CursorShape` for a look the headset does not have: before the first `CursorAt`
  naming it, and again once the headset may have let it go. The headset keeps the 64
  looks that came last for each host (the one that came first goes, whatever was
  used); the host counts the same, so the headset can trust that a look named in
  `CursorAt` came. A look named but not known is drawn as nothing. The same picture
  keeps its number for the session (current host: a hash of it), so an animated
  pointer sends its pictures once, then only numbers.
- `CursorAt` when the pointer moves, changes its look, hides or shows (at most as
  often as the display's frames, 60 to 120 times a second): `u`, `v` across the
  display's picture as streamed, the point of the look's `hot_x`, `hot_y` there.
  `display` none when it is on no display streamed to this headset (a display of
  the PC not shown, another output, a window panel: a panel's frames keep the
  pointer); `shape` 0 when the PC hides it (a video full screen, a game, touch).
  Right after `Snapshot` (or once the pointer goes apart) the host sends the look and
  place it has now. A headset not worn is sent nothing of the pointer, and the place
  it has now once it is worn again.
- A look in the pixels of the picture as streamed: over a picture the host scales
  down to stream, a look scaled down with it (a look of its own, with its own
  number).

The headset draws the look over the display at that point, the size it has in the
picture (`w` pixels of the picture's width), and hides it with the display. It can
draw the pointer where its own `MouseTo` sent it until `CursorAt` catches up
(implementation-defined; the current client: 200 ms after its last `MouseTo`).

### Input from the headset

A host that can make input on its desktop names feature 1 (`MouseTo` moves the mouse)
and, to a headset with the input right, feature 15: then the headset may send mouse
buttons (`MouseButton`), the wheel (`MouseScroll`) and fingers on a display (`Touch`),
in the same lane as `MouseTo` and in the order they happen. Points are pixels of the
display's picture as in `MouseTo` and `CursorAt`.

- **Buttons.** `MouseButton` moves the mouse to its point, then presses or lets go.
  A press of a button already down, or a release of one not down, changes nothing.
- **The wheel** turns where the mouse is, in 1/120 of a notch, so a smooth scroll
  sends parts of one. A desktop without finer steps takes whole notches and keeps
  what is left for the next (implementation-defined).
- **Fingers.** `Down`, any number of `Move`, then `Up` or `Cancel`, each finger by its
  `slot`; a slot is free again after `Up` or `Cancel`. Where the desktop has touch,
  the host makes touches of them, and the programs on the PC tap, scroll and hold by
  themselves; as on a touch screen, the pointer then hides (`CursorAt` with `shape`
  0) from a finger down until the mouse moves again. Where it has none, the host makes mouse actions of the first finger
  (implementation-defined; the current host: a tap is a left click, moving at once
  scrolls with the finger and goes on a little after it lifts, holding still for
  0.6 s a right click, holding and then moving drags with the left button). Other
  fingers wait for gestures of their own (later).
- **Nothing stays pressed.** The host lets go of everything a headset holds (buttons,
  fingers) when its session ends, when it says it is not worn, and when it says
  nothing at all for 2 s while it holds something: a headset holding a button or a
  finger still sends where it is (`MouseTo` or `Touch` `Move`) at least once a
  second.
- **Not worn, nothing pressed:** input from a headset that says it is not worn is
  dropped.
- **The desktop asks first** where it does (KDE Plasma and GNOME, through the
  RemoteDesktop portal): once, the first time input is needed, and it keeps the
  answer. Until then input goes nowhere. Where the person says no, or later stops the
  desktop's remote control, the host sends the headsets with the input right a
  `Problem` with `NOT_ALLOWED` and does not ask again until it starts again; where the
  desktop's input does not open, a `Problem` with `DESKTOP` (each once, until input
  works again). Meanwhile a new session is not offered feature 15, nor feature 1
  after a no.

### Sound

PC sound (`Audio`, lane 1) is 48 kHz (`AUDIO_RATE`), 2 channels (`AUDIO_CHANNELS`),
interleaved signed 16-bit little-endian, with no header or timestamp: each message is a
whole number of sample frames, to be played in arrival order. Slices are 10 ms in the
current host (1920 bytes), but the size is implementation-defined and a receiver must
accept any length. It flows while the headset is worn and `Sound.to_headset` is on,
and while there is sound: after 200 ms of slices that are all zero (the PC plays
nothing) the current host sends no more until a slice is not silent, and that one goes
at once, after two slices of silence (20 ms) that fill the player's empty buffer, so
the sound does not start with a click. A gap in `Audio` is silence to play, not a
fault; the receiver needs no notice of it (the current client's player counts the
gap as one underrun in its log, which it is not). Clients from 0.1 on behave so already: the player
waits for the next slice, and its device plays nothing meanwhile.

### Up streams: the microphone

What goes from the headset to the PC as a stream (the microphone now; the camera and
poses later) is an **up stream**: `UpStart{up, kind, format}`, then `Up{up, data}`
pieces, then `UpEnd{up}`, all in lane 1 so that none overtakes another. `up` is the
headset's number for the stream: the current client gives each recording a new one
(counting up, round after 255), and the host takes a new `UpStart` of a kind as the
one stream of that kind, so the `UpEnd` of a recording before it ends nothing. The
microphone is `Pcm16` at 48000 Hz with 1 channel (`MIC_CHANNELS`) or 2, interleaved
signed 16-bit little-endian, 10 ms in a piece in the current client. The current host
feeds its end of the microphone in mono: 2 channels become their mean; another format
is not taken (the stream is dropped and logged).

- The headset tells its switch and whether the system lets it record in `UpState`
  (a part, in `Open` and on every change). The host makes its end of the microphone
  (a virtual input programs record from; its state in `Sound.mic`) while the switch of
  a headset with the microphone right is on, and shows in its tray when the system does
  not let the headset record. Of several headsets one only is that microphone: the
  main headset if it has the right, else the first connected with the right. The
  others are told `Sound.mic` off and never `UpWanted{Mic, on: true}`, and what they
  stream does not reach the PC.
- While a program on the PC records from it, the host says `UpWanted{Mic, on: true}`;
  the headset then streams its microphone while its own switch is on, and ends the
  stream with `UpEnd` once `UpWanted` is off or the switch goes off. The switch is the
  user's, in the headset: no message of the host changes it.
- `UpStart` needs the microphone right; the host drops a stream without it.

The current client sends control messages first, then up streams and `FrameShown` in
lane 1; while the network is stuck it drops `Up` pieces and `FrameShown` beyond about
2.5 s waiting, never `UpStart` or `UpEnd`, and never a control message.

### Measuring the latency from the PC to the eye (test mode)

Not part of the protocol: nothing on the wire changes. A host started with
`prostir serve --time-code` (a hidden test switch) draws into every monitor's frames, as it captures them, the time
it captured each, as a code in the top left corner of the picture; a client in a
measuring mode reads it back from what it decoded and compares it with the time it
shows the picture, both on the PC's clock. `prostir_proto::timecode` draws and reads
it, so both sides agree; the format:

- A row of 42 square cells of 16 pixels on the luma plane, from the top left pixel
  (in 4:2:0 a cell covers whole chroma samples): cell 0 white and cell 1 black (where
  the code starts), cells 2 to 33 the value, most significant bit first, cells 34 to
  41 a check, the four bytes of the value added modulo 256, also most significant bit
  first. White (luma 235) is 1, black (16) is 0; a reader looks at the middle of each
  cell (row 8, column 8 of it) and takes luma above 128 as white.
- The value is the PC's `CLOCK_MONOTONIC` in milliseconds when the frame was captured
  (its GStreamer timestamp on the pipeline's clock), its low 32 bits.
- A check that does not match, or no white and black cell at the start, means no
  code (a picture made of two frames, a monitor too small for 672 pixels across).

To turn the PC's time into the headset's, the client needs the offset of the two
clocks. The current client's selftest against a real host (`pc=usb`,
`scripts/selftest-headset.sh --pc`) takes it from a small clock the script serves on
the PC (`CLOCK_MONOTONIC` in microseconds, one line for each line asked, reversed to
the headset's `127.0.0.1:47931` over USB), outside this protocol; it decodes one
stream into a reader instead of the compositor, reads the code of each picture as it
comes out of the decoder, and reports `end_to_end_ms` from the capture on the PC to
there (the compositor's own part, about one display period, is not in it).

## 7. Layout

`Arrange` tells the host how the displays stand around the user, so the PC desktop can
be arranged the same way and the mouse crosses from display to display where the eyes
expect it. Only the main headset's `Arrange` moves the desktop.

### Encoding

```
Arrange: rows:list<Row>(max 32)
Row:     entries:list<Placed>(max 32)
Placed:  id:DisplayId, center:opt<i32>, middle:opt<i32>
```

Rows go from top to bottom, entries in a row from left to right. An empty list means no
rows. An `Arrange` holds at most 32 displays in all its rows together and lists each
once; a reader rejects any other as invalid, and the connection ends.

- `id`: a virtual monitor, or a display of the PC itself if the headset placed it.
  Window panels are not part of the layout.
- `center`: across, where the centre of the display's place on the PC desktop is in
  the client's plan of it (see Units); none: not known.
- `middle`: the same **downwards**; none: not known.

### Units

`center` and `middle` are in pixels of the displays' own streams. Only differences
between values matter; the origin is arbitrary.

The current client plans the PC desktop itself, from how it shows the displays (the
same code the host runs, `crates/prostir-layout/src/desk/`), and sends that plan: each display's
place on it, a rectangle of its stream's size, as its top-left corner plus half its
size, rounded down (`x + w / 2`, `y + h / 2` in whole pixels). So a host that plans
from these rectangles gets the client's plan back to the pixel, and the client's map
shows what the PC desktop will be. The rows and the plan follow what the eyes see:

- A row is a chain of displays side by side as seen; a display joins its neighbour
  when their facing edges share at least half of the shorter one, and stays while they
  share a quarter, so a hand's tremor never moves a display from one row to another.
- Displays stand on the desktop as they are seen along the edges they share: offset
  as seen, so the mouse and a dragged window cross where the displays meet, sideways
  and up or down. A step between neighbours in a row within 10 % of the shorter edge
  is a hand's and is lined up (across, between rows, within 3 %); a larger one is kept,
  cut back only so that neighbours share at least 200 px.
- A changed arrangement is sent only when it has held for 0.8 s and nothing is being
  moved; a plan that fits what is seen as well as a new one, give or take a hand's
  tremor, is kept.

This method is implementation-defined; what the host relies on is that the values are
rectangles' centres in one pixel space, comparable between displays and to the
displays' own pixel sizes.

### What the host does with it

(Implementation-defined; `crates/prostir/src/displays/` and `crates/prostir-layout/src/desk/`, a crate
the client uses too to plan the desktop it sends and shows on its map.)

1. Clamps every known `center` and `middle` to ±100000.
2. Keeps the rows, drops ids that are neither a current monitor nor a display of the
   PC, keeps the spot of a display of the PC that is not connected now (it is not in
   the headset's rows while absent), and appends monitors missing from the rows to the
   end of the last row, with no positions. The rows are the one source of the order:
   `Displays` lists the displays in it, and the host keeps the rows for the next run.
3. Lays the desktop out, in logical pixels (pixel size divided by the output's scale),
   once however many changes came in at once, and only with outputs the desktop lists
   already. Each display with both values is a rectangle of its stream's size centred
   there; the plan is made of the rows and those rectangles:
   - When the rectangles already are a plan (the client's: every display of the rows
     placed, in whole pixels, each the size of its output on the desktop, nothing
     overlapping, rows in order and side by side, every display in reach of the mouse),
     it is taken as it is.
   - Otherwise the row with the primary display goes side by side from `0,0`, each
     display offset from its left neighbour as its rectangle is (within 10 % lined up,
     sharing at least 200 px); every other row, outwards from it, is cut where another
     row's display stands between two of its displays, and each piece rests on what is
     placed where its rectangles meet it. A plan worked out this way replaces the one
     the desktop has only when it is better by more than a hand's tremor.
   - A display of the PC goes under the primary monitor unless the headset placed it;
     window panels go in a column right of everything.
   Nothing overlaps, every output can be reached by the mouse, and the top-left corner
   of everything is at `0,0`. Where each display ended up is in `Displays` (`desk`).

`SetDisplays` replaces the layout with one row in the order of its `modes`, with no
positions, until the next `Arrange`.

## 8. What a headset decides for itself

Nothing here is on the wire as a rule: it is what the current headset does with what
the host sends, written down for someone who writes another client. Everything in this
section is implementation-defined, and the host relies on none of it except what
[Capabilities](#capabilities) and [Streams](#6-streams-sound-and-microphone) say.

**What it says it can do** (`HeadsetCaps`). Nothing comes from a headset's name: the
features and sizes are read from what the system and the compositor offer, and `model`
is only a label (the host shows it in its local API).

- `decoders`: for each codec the headset plays, the widest picture and the tallest one
  at that width, stepped down through standard heights (8K, 5K, 4K, 1600p, 1440p,
  1200p, 1080p, 720p) until the system says it decodes it at 60 frames a second;
  `DecoderPixels` has the pixels a picture may have for that. A decoder that gives no
  size is told as the sizes taken for granted for its codec (see
  [The version handshake](#the-version-handshake-frozen)).
- `layers_max`: the compositor's limit of layers in a frame, at least the 16 every
  OpenXR runtime takes. `streams_max`: the least of what each decoder takes at once
  (16 where it does not say) and `layers_max` less 7, at least 1. The 7 are kept for
  the app's own layers (the background, the menu, its button, the hands, the dots of
  the two pointing rays and the toast), so pointing or a message never pushes a monitor
  out. The host does not use `layers_max`; it counts streams by `streams_max`.
- Features 1 (commands), 3 (microphone), 6 (gaze), 7 (displays of the PC) and 8 (window
  panels) always; 2 (volume) where the system lets the app set the media volume; 9
  (rows bottom first) where the compositor cannot turn a layer over.

**What it shows.** Each stream is a layer of its own, with the room and the menu around
it. When the compositor takes fewer layers than the headset would draw, it leaves out,
in this order: the words over a monitor that still shows its picture, the farthest
monitors, the sign of a monitor being added and the toast, the keyboard window and the
background, and last the pointing dots, the menu and the hands. The user is told once
when monitors are left out. Nothing of this reaches the host.

**Video that breaks.** The headset never tells the host that a stream failed; it asks
for a keyframe (`RequestKeyframe`, at most every 300 ms per stream) and goes on.

- A passing failure (a codec error, a broken frame, a surface gone for a moment) makes
  the decoder again after 0.2, 0.5, 1, 2 and then 5 s at most, asking for a keyframe
  each time; after 3 in a row the compositor's surface is made again with it. It never
  gives up on a stream that has shown a picture in its generation. A picture that does not come for 2 s after a
  failure shows "recovering" over the monitor.
- A lasting failure (no decoder for the codec in `decoders`, a size it refuses, no
  surface for the stream) is not tried again until the host starts the stream anew
  (a new generation). A stream that gave no picture in its generation and fails for
  30 s is given up the same way, with a reason of its own.
- A monitor that cannot be shown keeps its place in the headset's arrangement (and in
  the `Arrange` it sends) and in its menu, with the reason drawn in its place, so a
  display that comes back is where it was.

**How large a monitor looks.** The host knows a monitor only in pixels; how large it is
in the room is the headset's. The current headset makes a new monitor as large as its
own pixels, one pixel of the PC for one of the headset's, from a table of the pixels
per degree of the headsets it knows, and a monitor added beside another is as much
larger or smaller than its own size as that one is; on a headset it does not know the
size is a standard one. It also decides how a monitor's picture is filtered (smoothed
only while shown smaller than the headset's pixels, sharpened always).

**The room.** The background that shows the room is made the first way the
runtime offers: the passthrough extension of its maker, then that of another maker, and
last the compositor's alpha blend (with a black layer to dim it); glasses that add light
always show the room and offer no such background. None of this is known to the host.
