use super::*;

const KEY: &str = "0123456789abcdef";

fn caps(backend: Backend, codecs: &[(Codec, u32)]) -> EncoderCaps {
    EncoderCaps {
        backend,
        adapter: None,
        codecs: codecs
            .iter()
            .map(|&(codec, max)| CodecCaps {
                codec,
                max_w: max,
                max_h: max,
            })
            .collect(),
        sessions: None,
    }
}

fn sample() -> Catalog {
    let mut nvenc = caps(Backend::Nvenc, &[(Codec::H265, 8192), (Codec::H264, 4096)]);
    nvenc.sessions = Some(3);
    Catalog {
        encoders: vec![
            caps(Backend::Va, &[(Codec::H264, 4096)]),
            nvenc,
            caps(Backend::X264, &[(Codec::H264, 16384)]),
        ],
        missing: vec![(
            Backend::Va,
            Codec::H265,
            "vah265enc is not installed".into(),
        )],
        retry: vec![(
            Backend::Nvenc,
            None,
            Codec::H264,
            "did not answer in 3 s".into(),
        )],
        key: KEY.into(),
        ..Catalog::default()
    }
}

/// A catalog kept in a file of its own, for the tests that save.
fn kept(name: &str) -> (Catalog, PathBuf) {
    let dir = std::env::temp_dir().join(format!("prostir-{name}-{}", std::process::id()));
    let path = dir.join("encoders.txt");
    let _ = std::fs::remove_dir_all(&dir);
    let catalog = Catalog {
        path: Some(path.clone()),
        ..sample()
    };
    (catalog, path)
}

fn read(path: &Path) -> Catalog {
    Catalog::parse(&std::fs::read_to_string(path).unwrap(), KEY).unwrap()
}

const NVENC: Kind = (Backend::Nvenc, None);
const VA: Kind = (Backend::Va, None);

fn output(n: u32) -> ScreenId {
    ScreenId::saved(n)
}

fn failure(short: bool, resumed: bool) -> NotOpened {
    NotOpened {
        why: "no".into(),
        short,
        resumed,
    }
}

/// What the cache file says comes back the same, under the same fingerprint only.
#[test]
fn the_cache_reads_back_what_it_wrote() {
    let catalog = sample();
    let text = catalog.text();
    let back = Catalog::parse(&text, KEY).unwrap();
    assert_eq!(back.encoders, catalog.encoders);
    assert_eq!(back.missing, catalog.missing);
    assert_eq!(back.retry, catalog.retry);
    assert_eq!(back.text(), text);
    // Another GStreamer, card or driver: probed again.
    assert!(Catalog::parse(&text, "fedcba9876543210").is_none());
    // A file of another version, or half of one, is no catalog.
    assert!(Catalog::parse("encoder va - h264 4096x4096\n", KEY).is_none());
    assert!(Catalog::parse(&format!("{text}what is this\n"), KEY).is_none());
    let half = format!("key {KEY}\nencoder va - h264 4096\n");
    assert!(Catalog::parse(&half, KEY).is_none());
    // No limit below one session.
    let none = format!("key {KEY}\nencoder nvenc - h264 4096x4096\nsessions nvenc - 0\n");
    assert_eq!(
        Catalog::parse(&none, KEY).unwrap().encoders[0].sessions,
        Some(1)
    );
}

/// A probe that failed or did not end is not kept as missing: what works is used,
/// and it is probed again at the next start; so is an encoder this build has that
/// the file does not know. What works and what is not installed are not.
#[test]
fn failures_are_probed_again_and_only_they() {
    let mut catalog = sample();
    let again = catalog.to_probe();
    assert!(
        again.contains(&(Backend::Nvenc, None, Codec::H264)),
        "{again:?}"
    );
    assert!(
        !again.contains(&(Backend::Va, None, Codec::H265)),
        "missing stays"
    );
    assert!(
        !again.contains(&(Backend::Va, None, Codec::H264)),
        "works stays"
    );
    assert!(!again.contains(&(Backend::X264, None, Codec::H264)));
    // It works this time: no longer to try again.
    catalog.add(
        (Backend::Nvenc, None, Codec::H264),
        Probed::Works {
            max_w: 4096,
            max_h: 4096,
        },
    );
    assert!(catalog.retry.is_empty());
    assert!(catalog.to_probe().is_empty());
    assert_eq!(catalog.encoders[1].codecs.len(), 2);
    assert_eq!(
        catalog.encoders[1].codecs[0].codec,
        Codec::H265,
        "sharpest first"
    );
    // It fails: what it encoded before is no longer counted on.
    catalog.add(
        (Backend::Va, None, Codec::H264),
        Probed::Failed("hung".into()),
    );
    assert!(catalog.encoders[0].codecs.is_empty());
    assert_eq!(catalog.retry.len(), 1);
    // A file without some encoder of this build: that one is probed.
    let older = Catalog {
        encoders: Vec::new(),
        missing: Vec::new(),
        retry: Vec::new(),
        ..sample()
    };
    assert_eq!(older.to_probe(), everything());
}

/// Short of sessions once: the same encoder once more. Twice: the limit is the
/// least number seen open, kept, and the output goes on; it rises when more are
/// seen open.
#[test]
fn a_session_limit_takes_two_failures_and_rises() {
    let (mut catalog, path) = kept("limit");
    catalog.encoders[1].sessions = None;
    let first = catalog.did_not_open(NVENC, Codec::H265, output(1), &failure(true, false), 4);
    assert_eq!(first, Next::Again);
    let wanted = Wanted {
        codec: Codec::H265,
        software: false,
    };
    let four = |_: Kind| 4;
    let ranked = catalog.rank(Memory::DmaBuf, wanted, output(1), &four);
    assert!(
        ranked.choices.iter().any(|c| c.backend == Backend::Nvenc),
        "not yet"
    );
    let second = catalog.did_not_open(NVENC, Codec::H265, output(1), &failure(true, false), 3);
    assert_eq!(second, Next::Another);
    assert_eq!(read(&path).encoders[1].sessions, Some(3));
    let three = |_: Kind| 3;
    let ranked = catalog.rank(Memory::DmaBuf, wanted, output(2), &three);
    assert!(ranked.choices.iter().all(|c| c.backend != Backend::Nvenc));
    catalog.seen_open(NVENC, 5);
    assert_eq!(read(&path).encoders[1].sessions, Some(5));
    let ranked = catalog.rank(Memory::DmaBuf, wanted, output(2), &four);
    assert_eq!(
        ranked.choices[0].backend,
        Backend::Va,
        "VA-API first on DMA-BUF"
    );
    assert!(ranked.choices.iter().any(|c| c.backend == Backend::Nvenc));
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

/// A limit kept from earlier runs is a hint: the encoder is tried past it, and one
/// failure is enough to hold it again this run.
#[test]
fn a_kept_limit_is_tried_past_and_held_after_one_failure() {
    let (mut catalog, path) = kept("kept-limit");
    let hardware = Wanted {
        codec: Codec::H264,
        software: false,
    };
    let three = |_: Kind| 3;
    let ranked = catalog.rank(Memory::System, hardware, output(1), &three);
    assert!(ranked.choices.iter().any(|c| c.backend == Backend::Nvenc));
    let next = catalog.did_not_open(NVENC, Codec::H264, output(1), &failure(true, false), 3);
    assert_eq!(next, Next::Another);
    let ranked = catalog.rank(Memory::System, hardware, output(2), &three);
    assert!(ranked.choices.iter().all(|c| c.backend != Backend::Nvenc));
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

/// Learned while encoders opened again after a pause: held this run, not kept.
#[test]
fn a_limit_learned_on_a_resume_is_not_kept() {
    let (mut catalog, path) = kept("resume");
    catalog.encoders[1].sessions = None;
    catalog.did_not_open(NVENC, Codec::H264, output(1), &failure(true, true), 2);
    catalog.did_not_open(NVENC, Codec::H264, output(1), &failure(true, true), 2);
    assert_eq!(catalog.limits.get(&NVENC), Some(&2));
    assert!(!path.exists() || read(&path).encoders[1].sessions.is_none());
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

/// No limit of VA-API (its drivers count none), of an encoder alone, or of an
/// error that is not one of sessions: the output's failure, for that output only,
/// for a while; probed again at the next start, the file kept.
#[test]
fn other_failures_are_the_outputs() {
    let (mut catalog, path) = kept("failure");
    let wanted = Wanted {
        codec: Codec::H264,
        software: false,
    };
    for (kind, short, open) in [(VA, true, 3), (NVENC, true, 0), (NVENC, false, 2)] {
        let next = catalog.did_not_open(kind, Codec::H264, output(1), &failure(short, false), open);
        assert_eq!(next, Next::Another);
    }
    assert!(catalog.limits.is_empty());
    let none = |_: Kind| 0;
    let one = catalog.rank(Memory::DmaBuf, wanted, output(1), &none);
    assert_eq!(one.choices[0].backend, Backend::X264, "{:?}", one.choices);
    let other = catalog.rank(Memory::DmaBuf, wanted, output(2), &none);
    assert_eq!(other.choices[0].backend, Backend::Va);
    let back = read(&path);
    assert!(
        back.retry
            .iter()
            .any(|r| r.0 == Backend::Va && r.2 == Codec::H264)
    );
    assert!(
        !back.encoders[0].codecs.is_empty(),
        "what works stays usable"
    );
    // Long ago: tried for that output again.
    for f in &mut catalog.failures {
        f.at = Instant::now() - FORGET_FAILURE - Duration::from_secs(1);
    }
    let later = catalog.rank(Memory::DmaBuf, wanted, output(1), &none);
    assert_eq!(later.choices[0].backend, Backend::Va);
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

/// What failed on an output is forgotten when asked (a new session, a retry by
/// hand): only for that output.
#[test]
fn an_outputs_failures_are_forgotten_when_asked() {
    let mut catalog = sample();
    let wanted = Wanted {
        codec: Codec::H264,
        software: false,
    };
    for n in [1, 2] {
        catalog.did_not_open(VA, Codec::H264, output(n), &failure(false, false), 0);
    }
    let none = |_: Kind| 0;
    let first =
        |c: &Catalog, n| c.rank(Memory::DmaBuf, wanted, output(n), &none).choices[0].backend;
    assert_ne!(first(&catalog, 1), Backend::Va);
    catalog.forget_output(output(1));
    assert_eq!(first(&catalog, 1), Backend::Va, "tried again");
    assert_ne!(first(&catalog, 2), Backend::Va, "the other output's stays");
}

/// The headset's menu: each codec on the graphics card and on the CPU, as found.
#[test]
fn codecs_offered_are_those_found() {
    assert_eq!(
        sample().available(),
        vec![
            (Codec::H265, true),
            (Codec::H264, true),
            (Codec::H264, false)
        ]
    );
    assert!(Catalog::default().available().is_empty());
}

/// Without regard to where frames are: the first card's encoder in the codec asked
/// for, else in the next; x264 with `--software` and when there is nothing else.
#[test]
fn the_pick_for_the_pc_as_before() {
    let catalog = sample();
    let pick = |codec, software| catalog.pick(codec, software).unwrap();
    assert_eq!(pick(Codec::H265, false), (Backend::Va, Codec::H264));
    assert_eq!(pick(Codec::H264, false), (Backend::Va, Codec::H264));
    assert_eq!(pick(Codec::H265, true), (Backend::X264, Codec::H264));
    let cpu = Catalog {
        encoders: vec![caps(Backend::X264, &[(Codec::H264, 16384)])],
        ..Catalog::default()
    };
    assert_eq!(
        cpu.pick(Codec::H265, false).unwrap(),
        (Backend::X264, Codec::H264)
    );
    assert!(Catalog::default().pick(Codec::H264, false).is_err());
}

/// The fingerprint is a short, stable hash.
#[test]
fn the_fingerprint_is_stable() {
    assert_eq!(fnv1a(b""), 0xcbf2_9ce4_8422_2325);
    assert_eq!(fnv1a(b"a"), 0xaf63_dc4c_8601_ec8c);
    assert_ne!(fnv1a(b"1.28.9"), fnv1a(b"1.28.8"));
}
