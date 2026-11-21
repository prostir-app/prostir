use std::time::{Duration, Instant};

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;

use super::{AFTER, Nudger};

/// A source, the queue the frames are sent again into, and what comes out.
struct Line {
    pipeline: gst::Pipeline,
    src: gst_app::AppSrc,
    out: gst_app::AppSink,
    nudger: Nudger,
}

impl Line {
    /// `counted`: frames are counted into an "encoder" (the appsink's pad), and out of
    /// it only as [`super::Handle::left`] says.
    fn new(counted: bool) -> Self {
        gst::init().unwrap();
        let pipeline = gst::parse::launch(
            "appsrc name=src is-live=true format=time \
             caps=video/x-raw,format=GRAY8,width=4,height=4,framerate=0/1 ! \
             queue name=q max-size-buffers=2 leaky=downstream ! appsink name=out sync=false",
        )
        .unwrap()
        .downcast::<gst::Pipeline>()
        .unwrap();
        let src = pipeline.by_name("src").unwrap().downcast().unwrap();
        let out = pipeline.by_name("out").unwrap().downcast().unwrap();
        let pad = pipeline.by_name("q").unwrap().static_pad("sink").unwrap();
        let encoder = out_pad(&pipeline);
        let nudger = Nudger::start(&pad, counted.then_some(&encoder), "test", AFTER).unwrap();
        pipeline.set_state(gst::State::Playing).unwrap();
        Self {
            pipeline,
            src,
            out,
            nudger,
        }
    }

    /// A new picture (memory of its own), taken at `pts`.
    fn picture(pts_ms: u64) -> gst::Buffer {
        let mut b = gst::Buffer::from_mut_slice(vec![0u8; 16]);
        b.get_mut()
            .unwrap()
            .set_pts(gst::ClockTime::from_mseconds(pts_ms));
        b
    }

    fn push(&self, buffer: &gst::Buffer) {
        self.src.push_buffer(buffer.clone()).unwrap();
    }

    /// What comes out within `time`: each frame's memory, time and when it came.
    fn take_for(&self, time: Duration) -> Vec<(usize, u64, Instant)> {
        let until = Instant::now() + time;
        let mut got = Vec::new();
        loop {
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return got;
            }
            if let Some(s) = self
                .out
                .try_pull_sample(gst::ClockTime::from_nseconds(left.as_nanos() as u64))
            {
                let b = s.buffer().unwrap();
                got.push((
                    b.peek_memory(0).as_ptr() as usize,
                    b.pts().unwrap().mseconds(),
                    Instant::now(),
                ));
            }
        }
    }
}

impl Drop for Line {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

fn out_pad(pipeline: &gst::Pipeline) -> gst::Pad {
    pipeline.by_name("out").unwrap().static_pad("sink").unwrap()
}

fn memory(b: &gst::Buffer) -> usize {
    b.peek_memory(0).as_ptr() as usize
}

/// A new picture with nothing after it comes out once more, about 30 ms later, in
/// the same memory and later in time; then nothing more.
#[test]
fn a_new_picture_is_sent_again_once() {
    let line = Line::new(false);
    let first = Line::picture(1000);
    let pushed = Instant::now();
    line.push(&first);
    let got = line.take_for(Duration::from_millis(400));
    assert_eq!(got.len(), 2, "{got:?}");
    assert!(got.iter().all(|(m, ..)| *m == memory(&first)));
    let (_, pts, came) = got[1];
    assert!(pts >= 1000 + 25, "later in time: {pts}");
    let after = came.duration_since(pushed);
    assert!(
        after >= Duration::from_millis(25) && after < Duration::from_millis(150),
        "{after:?}"
    );
}

/// A repeat (the source's keepalive, the same memory) is not sent again; a keyframe
/// asked for makes it count as new.
#[test]
fn a_repeat_is_sent_again_only_for_a_keyframe() {
    let line = Line::new(false);
    let picture = Line::picture(1000);
    line.push(&picture);
    assert_eq!(line.take_for(Duration::from_millis(200)).len(), 2);
    let mut keepalive = picture.copy();
    keepalive
        .make_mut()
        .set_pts(gst::ClockTime::from_mseconds(3000));
    line.push(&keepalive);
    assert_eq!(
        line.take_for(Duration::from_millis(200)).len(),
        1,
        "no repeat"
    );
    line.nudger.handle().next_is_new();
    let mut keepalive = picture.copy();
    keepalive
        .make_mut()
        .set_pts(gst::ClockTime::from_mseconds(5000));
    line.push(&keepalive);
    assert_eq!(
        line.take_for(Duration::from_millis(200)).len(),
        2,
        "keyframe"
    );
}

/// Pictures that keep coming are not sent again; only the last one, once they stop.
#[test]
fn only_the_last_of_a_moving_screen_is_sent_again() {
    let line = Line::new(false);
    let pictures: Vec<gst::Buffer> = (0..5).map(|i| Line::picture(1000 + i * 10)).collect();
    for p in &pictures {
        line.push(p);
        std::thread::sleep(Duration::from_millis(10));
    }
    let got = line.take_for(Duration::from_millis(300));
    assert_eq!(got.len(), 6, "{got:?}");
    let last = memory(pictures.last().unwrap());
    assert_eq!(got.iter().filter(|(m, ..)| *m == last).count(), 2);
}

/// Dropped, it stops at once, a picture armed or not.
#[test]
fn it_stops_with_a_picture_armed() {
    let line = Line::new(false);
    line.push(&Line::picture(1000));
    let started = Instant::now();
    drop(line);
    assert!(started.elapsed() < Duration::from_millis(500));
}

/// While the encoder still keeps the new picture, it is sent again 30 ms later, three
/// times at most; once it gave it out, no more.
#[test]
fn it_is_sent_again_while_the_encoder_keeps_it() {
    let line = Line::new(true);
    line.push(&Line::picture(1000));
    // Nothing comes out of this "encoder": three times.
    assert_eq!(line.take_for(Duration::from_millis(400)).len(), 4);

    let line = Line::new(true);
    let handle = line.nudger.handle();
    line.push(&Line::picture(1000));
    // Each frame is out at once.
    let out = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let seen = out.clone();
    out_pad(&line.pipeline).add_probe(gst::PadProbeType::BUFFER, move |_, _| {
        handle.left();
        seen.store(true, std::sync::atomic::Ordering::Relaxed);
        gst::PadProbeReturn::Ok
    });
    assert_eq!(line.take_for(Duration::from_millis(400)).len(), 2);
    assert!(out.load(std::sync::atomic::Ordering::Relaxed));
}
