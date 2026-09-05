//! A GTK paintable with an explicitly owned GStreamer pipeline. GTK's bundled
//! GstPlay backend can finalize on its own dispatch thread and deadlock; direct
//! pipeline ownership makes close/reuse deterministic without retaining threads.
use std::cell::{Cell, RefCell};
use std::path::Path;
use std::time::Duration;

use gst::prelude::*;
use gstreamer as gst;
use gtk::{gdk, glib, prelude::*, subclass::prelude::*};
use gtk4 as gtk;

struct Frame {
    bytes: gst::MappedBuffer<gst::buffer::Readable>,
    width: i32,
    height: i32,
    stride: usize,
    aspect_ratio: f64,
}

fn frame(
    sink: &gstreamer_app::AppSink,
    preroll: bool,
    sender: &async_channel::Sender<Frame>,
) -> Result<gst::FlowSuccess, gst::FlowError> {
    let sample = if preroll {
        sink.pull_preroll()
    } else {
        sink.pull_sample()
    }
    .map_err(|_| gst::FlowError::Eos)?;
    let caps = sample.caps().ok_or(gst::FlowError::Error)?;
    let info = gstreamer_video::VideoInfo::from_caps(caps).map_err(|_| gst::FlowError::Error)?;
    let buffer = sample.buffer_owned().ok_or(gst::FlowError::Error)?;
    let bytes = buffer
        .into_mapped_buffer_readable()
        .map_err(|_| gst::FlowError::Error)?;
    let frame = Frame {
        bytes,
        width: info.width() as i32,
        height: info.height() as i32,
        stride: info.stride()[0] as usize,
        aspect_ratio: f64::from(info.width()) * f64::from(info.par().numer())
            / (f64::from(info.height()) * f64::from(info.par().denom())),
    };
    // At most two frames in flight. A busy UI drops a frame rather than queuing
    // unbounded decoded video. Bytes own the mapped GstBuffer: no pixel copy.
    let _ = sender.try_send(frame);
    Ok(gst::FlowSuccess::Ok)
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct VideoStream {
        pub pipeline: RefCell<Option<gst::Pipeline>>,
        pub texture: RefCell<Option<gdk::Texture>>,
        pub bus: RefCell<Option<gst::bus::BusWatchGuard>>,
        pub frames: RefCell<Option<glib::JoinHandle<()>>>,
        pub clock: RefCell<Option<glib::SourceId>>,
        pub pending_seek: Cell<Option<i64>>,
        pub aspect_ratio: Cell<f64>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for VideoStream {
        const NAME: &'static str = "OmgVideoStream";
        type Type = super::VideoStream;
        type ParentType = gtk::MediaFile;
        type Interfaces = (gdk::Paintable,);
    }
    impl ObjectImpl for VideoStream {
        fn dispose(&self) {
            self.close_pipeline();
        }
    }
    impl PaintableImpl for VideoStream {
        fn snapshot(&self, snapshot: &gdk::Snapshot, width: f64, height: f64) {
            if let Some(texture) = self.texture.borrow().as_ref() {
                texture.snapshot(snapshot, width, height);
            }
        }
        fn current_image(&self) -> gdk::Paintable {
            self.texture
                .borrow()
                .as_ref()
                .map(|t| t.clone().upcast())
                .unwrap_or_else(|| gdk::Paintable::new_empty(1, 1))
        }
        fn intrinsic_width(&self) -> i32 {
            self.texture.borrow().as_ref().map_or(0, |t| t.width())
        }
        fn intrinsic_height(&self) -> i32 {
            self.texture.borrow().as_ref().map_or(0, |t| t.height())
        }
        fn intrinsic_aspect_ratio(&self) -> f64 {
            self.aspect_ratio.get()
        }
    }
    impl MediaFileImpl for VideoStream {
        fn open(&self) {
            if let Err(error) = self.open_pipeline() {
                self.close_pipeline();
                self.obj()
                    .set_error(glib::Error::new(gtk::gio::IOErrorEnum::Failed, &error));
            }
        }
        fn close(&self) {
            self.close_pipeline();
            if self.obj().is_prepared() {
                self.obj().stream_unprepared();
            }
        }
    }
    impl MediaStreamImpl for VideoStream {
        fn play(&self) -> bool {
            let result = self
                .pipeline
                .borrow()
                .as_ref()
                .is_some_and(|p| p.set_state(gst::State::Playing).is_ok());
            if result {
                self.start_clock();
            }
            result
        }
        fn pause(&self) {
            self.stop_clock();
            if let Some(pipeline) = self.pipeline.borrow().as_ref() {
                let _ = pipeline.set_state(gst::State::Paused);
            }
        }
        fn seek(&self, timestamp: i64) {
            self.pending_seek.set(Some(timestamp.max(0)));
            self.apply_seek();
        }
        fn update_audio(&self, muted: bool, volume: f64) {
            if let Some(pipeline) = self.pipeline.borrow().as_ref() {
                pipeline.set_property("mute", muted);
                pipeline.set_property("volume", volume.powi(3));
            }
        }
        fn realize(&self, _surface: gdk::Surface) {}
        fn unrealize(&self, _surface: gdk::Surface) {}
    }
    impl VideoStream {
        fn open_pipeline(&self) -> Result<(), String> {
            gst::init().map_err(|e| e.to_string())?;
            let file = self.obj().file().ok_or("Media file unavailable")?;
            let pipeline = gst::ElementFactory::make("playbin3")
                .build()
                .map_err(|e| e.to_string())?
                .downcast::<gst::Pipeline>()
                .map_err(|_| "Media pipeline unavailable")?;
            let (sender, receiver) = async_channel::bounded(2);
            let other = sender.clone();
            let sink = gstreamer_app::AppSink::builder()
                .caps(
                    &gst::Caps::builder("video/x-raw")
                        .field("format", "RGBA")
                        .build(),
                )
                .max_buffers(2)
                .drop(true)
                .sync(true)
                .callbacks(
                    gstreamer_app::AppSinkCallbacks::builder()
                        .new_sample(move |sink| frame(sink, false, &sender))
                        .new_preroll(move |sink| frame(sink, true, &other))
                        .build(),
                )
                .build();
            pipeline.set_property("video-sink", &sink);
            pipeline.set_property("uri", file.uri());
            pipeline.set_property("mute", self.obj().is_muted());
            pipeline.set_property("volume", self.obj().volume().powi(3));
            let weak = self.obj().downgrade();
            *self.frames.borrow_mut() =
                Some(glib::MainContext::default().spawn_local(async move {
                    while let Ok(frame) = receiver.recv().await {
                        let Some(stream) = weak.upgrade() else { break };
                        let imp = stream.imp();
                        let resize = imp.intrinsic_width() != frame.width
                            || imp.intrinsic_height() != frame.height;
                        let resize = resize
                            || imp.aspect_ratio.replace(frame.aspect_ratio) != frame.aspect_ratio;
                        let bytes = glib::Bytes::from_owned(frame.bytes);
                        let texture = gdk::MemoryTexture::new(
                            frame.width,
                            frame.height,
                            gdk::MemoryFormat::R8g8b8a8,
                            &bytes,
                            frame.stride,
                        );
                        *imp.texture.borrow_mut() = Some(texture.upcast());
                        if resize {
                            stream.invalidate_size();
                        }
                        stream.invalidate_contents();
                    }
                }));
            let weak = self.obj().downgrade();
            let bus = pipeline.bus().ok_or("Media bus unavailable")?;
            *self.bus.borrow_mut() = Some(
                bus.add_watch_local(move |_, message| {
                    let Some(stream) = weak.upgrade() else {
                        return glib::ControlFlow::Break;
                    };
                    let imp = stream.imp();
                    match message.view() {
                        gst::MessageView::Error(error) => {
                            stream.set_error(error.error());
                            stream.pause();
                        }
                        gst::MessageView::AsyncDone(_) => {
                            if !stream.is_prepared() {
                                let duration = imp
                                    .pipeline
                                    .borrow()
                                    .as_ref()
                                    .and_then(|p| p.query_duration::<gst::ClockTime>())
                                    .map_or(0, |t| t.useconds() as i64);
                                stream.stream_prepared(true, true, duration > 0, duration);
                            }
                            if stream.is_seeking() {
                                stream.seek_success();
                            }
                            imp.apply_seek();
                            imp.update_position();
                        }
                        gst::MessageView::Eos(_) => {
                            if stream.is_loop() {
                                imp.pending_seek.set(Some(0));
                                imp.apply_seek();
                            } else {
                                stream.stream_ended();
                                imp.stop_clock();
                            }
                        }
                        _ => {}
                    }
                    glib::ControlFlow::Continue
                })
                .map_err(|e| e.to_string())?,
            );
            *self.pipeline.borrow_mut() = Some(pipeline.clone());
            pipeline
                .set_state(gst::State::Paused)
                .map_err(|_| "Could not prepare video")?;
            Ok(())
        }
        pub(super) fn apply_seek(&self) {
            if !self.obj().is_prepared() {
                return;
            }
            let Some(timestamp) = self.pending_seek.take() else {
                return;
            };
            let ok = self.pipeline.borrow().as_ref().is_some_and(|p| {
                p.seek_simple(
                    gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE,
                    gst::ClockTime::from_useconds(timestamp as u64),
                )
                .is_ok()
            });
            if !ok {
                self.obj().seek_failed();
            }
        }
        fn update_position(&self) {
            if !self.obj().is_prepared() {
                return;
            }
            if let Some(position) = self
                .pipeline
                .borrow()
                .as_ref()
                .and_then(|p| p.query_position::<gst::ClockTime>())
            {
                self.obj().update(position.useconds() as i64);
            }
        }
        fn start_clock(&self) {
            self.stop_clock();
            let weak = self.obj().downgrade();
            *self.clock.borrow_mut() = Some(glib::timeout_add_local(
                Duration::from_millis(100),
                move || {
                    let Some(stream) = weak.upgrade() else {
                        return glib::ControlFlow::Break;
                    };
                    stream.imp().update_position();
                    glib::ControlFlow::Continue
                },
            ));
        }
        fn stop_clock(&self) {
            if let Some(source) = self.clock.borrow_mut().take() {
                source.remove();
            }
        }
        fn close_pipeline(&self) {
            self.stop_clock();
            self.bus.borrow_mut().take();
            if let Some(task) = self.frames.borrow_mut().take() {
                task.abort();
            }
            let pipeline = self.pipeline.borrow_mut().take();
            if let Some(pipeline) = pipeline {
                if let Some(bus) = pipeline.bus() {
                    bus.set_flushing(true);
                }
                let _ = pipeline.set_state(gst::State::Null);
            }
            self.texture.borrow_mut().take();
            self.pending_seek.set(None);
        }
    }
}

glib::wrapper! {
    pub struct VideoStream(ObjectSubclass<imp::VideoStream>)
        @extends gtk::MediaFile, gtk::MediaStream,
        @implements gdk::Paintable;
}

pub fn new() -> gtk::MediaFile {
    glib::Object::new::<VideoStream>().upcast()
}
pub fn for_filename(path: &Path) -> gtk::MediaFile {
    let stream = new();
    stream.set_filename(Some(path));
    stream
}

pub fn seek_when_ready(media: &gtk::MediaFile, position: i64) {
    if let Some(stream) = media.downcast_ref::<VideoStream>() {
        stream.imp().pending_seek.set(Some(position.max(0)));
        stream.imp().apply_seek();
    }
}
