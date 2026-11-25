//! The feed against PipeWire itself: a node of the test's own (a test picture through
//! `pipewiresink`, standing in for KWin's), read into an `appsrc` as the encoder's.
//! Skipped without a PipeWire daemon (CI has none) or gst-plugin-pipewire.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

use super::*;

/// Whether a PipeWire daemon answers.
fn pipewire_runs() -> bool {
    let Ok(main) = pw::main_loop::MainLoopRc::new(None) else {
        return false;
    };
    let Ok(context) = pw::context::ContextRc::new(&main, None) else {
        return false;
    };
    context.connect_rc(None).is_ok()
}

/// The id of the node called `name`, once PipeWire lists it.
fn node_id(name: &str) -> Option<u32> {
    let main = pw::main_loop::MainLoopRc::new(None).ok()?;
    let context = pw::context::ContextRc::new(&main, None).ok()?;
    let core = context.connect_rc(None).ok()?;
    let registry = core.get_registry_rc().ok()?;
    let found = Rc::new(std::cell::Cell::new(None));
    let _listener = {
        let (found, name) = (found.clone(), name.to_owned());
        registry
            .add_listener_local()
            .global(move |g| {
                if g.props.and_then(|p| p.get("node.name")) == Some(name.as_str()) {
                    found.set(Some(g.id));
                }
            })
            .register()
    };
    let until = Instant::now() + Duration::from_secs(5);
    while found.get().is_none() && Instant::now() < until {
        main.loop_()
            .iterate(pw::loop_::Timeout::Finite(Duration::from_millis(50)));
    }
    found.get()
}

/// A PipeWire video node of the test's own giving `caps` frames (a test picture, not
/// the desktop), and its id; it goes with the pipeline. None (skipped) without
/// PipeWire or its GStreamer plugin.
fn test_node(caps: &str) -> Option<(gst::Element, u32)> {
    gst::init().unwrap();
    if gst::ElementFactory::find("pipewiresink").is_none() || !pipewire_runs() {
        eprintln!("skipped: no PipeWire daemon or no pipewiresink");
        return None;
    }
    static N: AtomicU32 = AtomicU32::new(0);
    let name = format!(
        "prostir-feed-test-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    );
    let provider = gst::parse::launch(&format!(
        "videotestsrc is-live=true pattern=ball ! {caps} ! pipewiresink mode=provide \
         stream-properties=\"props,node.name={name},media.class=Video/Source,node.virtual=true,priority.session=0\""
    ))
    .unwrap();
    provider.set_state(gst::State::Playing).unwrap();
    let id = node_id(&name).expect("the test's node is listed");
    Some((provider, id))
}

/// An encoder's input (`appsrc`, as on KDE) with a converter to I420 after it, and the
/// memory of each frame that came through.
fn consumer() -> (gst::Pipeline, gst_app::AppSrc, Arc<Mutex<Vec<usize>>>) {
    let pipeline = gst::parse::launch(
        "appsrc name=src is-live=true format=time do-timestamp=true max-buffers=2 \
         leaky-type=downstream ! queue max-size-buffers=2 leaky=downstream \
         ! identity name=enc_in ! videoconvert ! video/x-raw,format=I420 \
         ! fakesink sync=false",
    )
    .unwrap()
    .downcast::<gst::Pipeline>()
    .unwrap();
    let appsrc = pipeline
        .by_name("src")
        .unwrap()
        .downcast::<gst_app::AppSrc>()
        .unwrap();
    let frames = Arc::new(Mutex::new(Vec::new()));
    let kept = frames.clone();
    pipeline
        .by_name("enc_in")
        .unwrap()
        .static_pad("sink")
        .unwrap()
        .add_probe(gst::PadProbeType::BUFFER, move |_, info| {
            if let Some(b) = info.buffer() {
                kept.lock()
                    .unwrap()
                    .push(b.peek_memory(0).as_ptr() as usize);
            }
            gst::PadProbeReturn::Ok
        });
    pipeline.set_state(gst::State::Playing).unwrap();
    (pipeline, appsrc, frames)
}

fn count_in(frames: &Mutex<Vec<usize>>, time: Duration) -> Vec<usize> {
    let before = frames.lock().unwrap().len();
    std::thread::sleep(time);
    frames.lock().unwrap()[before..].to_vec()
}

/// A node's frames reach the encoder's input in the format it takes, the last one
/// goes again while the node sends nothing, nothing goes while paused, and resumed
/// the stream starts over and frames come again.
#[test]
fn a_pipewire_nodes_frames_reach_the_encoder() {
    let Some((provider, node)) =
        test_node("video/x-raw,format=BGRx,width=320,height=240,framerate=30/1")
    else {
        return;
    };
    let (pipeline, appsrc, frames) = consumer();
    let pause = Arc::new(Pause::default());
    let feed = Feed::start(
        node,
        "test",
        appsrc.clone(),
        pause.clone(),
        Duration::from_millis(300),
    )
    .expect("the feed starts");
    let caps = appsrc.caps().expect("caps set from the stream's format");
    let s = caps.structure(0).unwrap();
    assert_eq!(s.get::<&str>("format").unwrap(), "BGRx", "{caps}");
    assert_eq!(s.get::<i32>("width").unwrap(), 320);

    let came = count_in(&frames, Duration::from_secs(1));
    assert!(came.len() >= 10, "{} frames in 1 s", came.len());
    // This node says no pointer and no changed parts: all of each picture changed.
    let seen = feed.sightings();
    assert_eq!(seen.take_damage(), meta::Damage::Whole);
    assert_eq!(seen.pointer(), (None, None));

    // The node goes still: its last picture goes again, every 300 ms.
    provider.set_state(gst::State::Paused).unwrap();
    std::thread::sleep(Duration::from_millis(200));
    let still = count_in(&frames, Duration::from_millis(1000));
    assert!(
        (2..=5).contains(&still.len()),
        "{} repeats in 1 s",
        still.len()
    );
    assert!(still.windows(2).all(|w| w[0] == w[1]), "the same picture");
    // Asked again: at once.
    let before = frames.lock().unwrap().len();
    pause.picture_again();
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(frames.lock().unwrap().len(), before + 1);
    provider.set_state(gst::State::Playing).unwrap();

    // Paused: nothing at all.
    pause.set(true);
    std::thread::sleep(Duration::from_millis(200));
    assert!(count_in(&frames, Duration::from_millis(700)).is_empty());
    // Resumed: the stream starts over and frames come.
    pause.set(false);
    let came = count_in(&frames, Duration::from_secs(1));
    assert!(
        came.len() >= 10,
        "{} frames in 1 s after resuming",
        came.len()
    );

    drop(feed);
    let left = frames.lock().unwrap().len();
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(frames.lock().unwrap().len(), left, "stopped: nothing more");
    pipeline.set_state(gst::State::Null).unwrap();
    provider.set_state(gst::State::Null).unwrap();
}

/// KWin's node gone: the start fails as the source's at once, linked to no other
/// node.
#[test]
fn a_gone_node_fails_at_once_as_the_source() {
    gst::init().unwrap();
    if !pipewire_runs() {
        eprintln!("skipped: no PipeWire daemon");
        return;
    }
    let (pipeline, appsrc, frames) = consumer();
    let started = Instant::now();
    let result = Feed::start(
        4_000_000,
        "gone",
        appsrc,
        Arc::new(Pause::default()),
        Duration::from_secs(2),
    );
    let took = started.elapsed();
    let Err(e) = result else {
        panic!("started on a node that is not there")
    };
    eprintln!("failed after {took:?}: {e:#}");
    assert!(e.downcast_ref::<SourceFailed>().is_some(), "{e:#}");
    assert!(took < FIRST_WORD, "{took:?}");
    assert!(frames.lock().unwrap().is_empty());
    pipeline.set_state(gst::State::Null).unwrap();
}
