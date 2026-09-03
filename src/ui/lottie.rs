//! Animated (.tgs) stickers (specs/spec-wave6.md §5.2).
//!
//! ThorVG is `!Send`, so ONE render thread ("lottie-render", started lazily)
//! owns the engine and every `Animation`; the UI thread only turns finished
//! frames into `gdk::MemoryTexture`s. Frames are coalesced (latest wins per
//! sticker) so the UI can never fall behind, at most six stickers animate at
//! once, and playback pauses when the owner says so (row scrolled out of
//! view, picker cell not hovered), when `media.animated_stickers` is off or
//! when GTK animations are off.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::rc::{Rc, Weak};
use std::sync::mpsc::{self, RecvTimeoutError, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gtk::gdk;
use gtk::gio;
use gtk::glib;
use gtk::glib::subclass::prelude::ObjectSubclassIsExt;
use gtk::prelude::*;
use gtk4 as gtk;

use super::lottie_backend::{Animation, Engine};

/// Sticker edge length in a message bubble, in logical pixels.
pub const BUBBLE_SIZE: i32 = 256;
/// Sticker edge length in a picker cell, in logical pixels.
pub const CELL_SIZE: i32 = 96;
/// At most this many stickers animate at once; the rest keep their first
/// frame until a slot frees (oldest waiting first).
const MAX_ANIMATING: usize = 6;
/// 60 Hz cap for the render thread.
const MIN_INTERVAL: Duration = Duration::from_millis(16);
/// Rasterizing above this edge length is pointless on a 256px bubble.
const MAX_PIXELS: i32 = 512;

enum Command {
    Load { id: u64, bytes: Vec<u8>, size: u32 },
    Play(u64),
    Pause(u64),
    Drop(u64),
}

struct Frame {
    index: usize,
    size: u32,
    data: Vec<u8>,
}

#[derive(Default)]
struct Pending {
    /// Latest frame per sticker: an unconsumed frame is replaced, never
    /// queued, so a slow UI thread can never fall behind.
    frames: HashMap<u64, Frame>,
    errors: Vec<(u64, String)>,
}

struct Bus {
    pending: Mutex<Pending>,
    wake: async_channel::Sender<()>,
}

impl Bus {
    fn frame(&self, id: u64, index: usize, size: u32, data: &[u8]) {
        if let Ok(mut pending) = self.pending.lock() {
            pending.frames.insert(
                id,
                Frame {
                    index,
                    size,
                    data: data.to_vec(),
                },
            );
        }
        let _ = self.wake.try_send(());
    }

    fn error(&self, id: u64, message: String) {
        if let Ok(mut pending) = self.pending.lock() {
            pending.frames.remove(&id);
            pending.errors.push((id, message));
        }
        let _ = self.wake.try_send(());
    }
}

struct Live {
    anim: Animation<'static>,
    frames: usize,
    playhead: usize,
    interval: Duration,
    due: Instant,
    playing: bool,
}

/// The render thread: owns the engine and every animation, sleeps until the
/// earliest next frame is due, and never touches GTK.
fn render_thread(commands: mpsc::Receiver<Command>, bus: Arc<Bus>) {
    // Leaked on purpose: the engine must outlive every Animation made from
    // it and there is exactly one per process.
    let engine: Option<&'static Engine> = match Engine::new() {
        Ok(engine) => Some(Box::leak(Box::new(engine))),
        Err(error) => {
            eprintln!("omarchygram: animated stickers unavailable: {error}");
            None
        }
    };
    let mut live: HashMap<u64, Live> = HashMap::new();
    // Playback can be asked for before the file is even read, so the wish is
    // remembered until the animation exists.
    let mut wanted: HashSet<u64> = HashSet::new();
    loop {
        let now = Instant::now();
        let next = live
            .values()
            .filter(|animation| animation.playing)
            .map(|animation| animation.due)
            .min();
        let command = match next {
            Some(due) if due > now => commands.recv_timeout(due - now),
            Some(_) => match commands.try_recv() {
                Ok(command) => Ok(command),
                Err(TryRecvError::Empty) => Err(RecvTimeoutError::Timeout),
                Err(TryRecvError::Disconnected) => Err(RecvTimeoutError::Disconnected),
            },
            None => commands.recv().map_err(|_| RecvTimeoutError::Disconnected),
        };
        match command {
            Ok(command) => {
                apply(command, engine, &mut live, &mut wanted, &bus);
                continue;
            }
            Err(RecvTimeoutError::Timeout) => {}
            // The UI side is gone (process shutting down).
            Err(RecvTimeoutError::Disconnected) => return,
        }
        let now = Instant::now();
        for (id, animation) in live.iter_mut() {
            if !animation.playing || animation.due > now {
                continue;
            }
            animation.playhead = (animation.playhead + 1) % animation.frames;
            let index = animation.playhead;
            let size = animation.anim.size();
            match animation.anim.render(index) {
                Ok(frame) => bus.frame(*id, index, size, frame),
                Err(error) => {
                    animation.playing = false;
                    bus.error(*id, error);
                }
            }
            animation.due = (animation.due + animation.interval).max(now);
        }
    }
}

fn apply(
    command: Command,
    engine: Option<&'static Engine>,
    live: &mut HashMap<u64, Live>,
    wanted: &mut HashSet<u64>,
    bus: &Bus,
) {
    match command {
        Command::Load { id, bytes, size } => {
            let Some(engine) = engine else {
                bus.error(id, "animated stickers are unavailable".into());
                return;
            };
            let mut anim = match engine.load(&bytes, size) {
                Ok(anim) => anim,
                Err(error) => return bus.error(id, error),
            };
            let frames = anim.frame_count().max(1);
            let interval =
                Duration::from_secs_f64(1.0 / anim.fps().clamp(1.0, 120.0)).max(MIN_INTERVAL);
            let size = anim.size();
            // The first frame is always rendered: it is what a paused, an
            // off-screen and a settings-disabled sticker shows.
            match anim.render(0) {
                Ok(frame) => bus.frame(id, 0, size, frame),
                Err(error) => return bus.error(id, error),
            }
            live.insert(
                id,
                Live {
                    anim,
                    frames,
                    playhead: 0,
                    interval,
                    due: Instant::now() + interval,
                    playing: wanted.contains(&id),
                },
            );
        }
        Command::Play(id) => {
            wanted.insert(id);
            if let Some(animation) = live.get_mut(&id) {
                if !animation.playing {
                    animation.playing = true;
                    animation.due = Instant::now() + animation.interval;
                }
            }
        }
        Command::Pause(id) => {
            wanted.remove(&id);
            if let Some(animation) = live.get_mut(&id) {
                animation.playing = false;
            }
        }
        Command::Drop(id) => {
            wanted.remove(&id);
            live.remove(&id);
        }
    }
}

/// The paintable behind every sticker `gtk::Picture`: one object per sticker
/// whose texture is swapped per frame. Replacing the picture's paintable
/// instead would queue a resize 30 times a second (relayouting the whole
/// message list and breaking offscreen snapshots mid-layout); a paintable with
/// `PaintableFlags::SIZE` only ever redraws.
mod frame_paintable {
    use std::cell::{Cell, RefCell};

    use gtk::gdk;
    use gtk::glib;
    use gtk::graphene;
    use gtk::prelude::*;
    use gtk::subclass::prelude::*;
    use gtk4 as gtk;

    #[derive(Default)]
    pub struct LottieFrame {
        pub texture: RefCell<Option<gdk::Texture>>,
        pub size: Cell<(i32, i32)>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for LottieFrame {
        const NAME: &'static str = "OmgLottieFrame";
        type Type = super::LottieFrame;
        type Interfaces = (gdk::Paintable,);
    }

    impl ObjectImpl for LottieFrame {}

    impl PaintableImpl for LottieFrame {
        fn flags(&self) -> gdk::PaintableFlags {
            gdk::PaintableFlags::STATIC_SIZE
        }

        fn intrinsic_width(&self) -> i32 {
            self.size.get().0
        }

        fn intrinsic_height(&self) -> i32 {
            self.size.get().1
        }

        fn snapshot(&self, snapshot: &gdk::Snapshot, width: f64, height: f64) {
            let Some(texture) = self.texture.borrow().clone() else {
                return;
            };
            let Some(snapshot) = snapshot.downcast_ref::<gtk::Snapshot>() else {
                return;
            };
            snapshot.append_texture(
                &texture,
                &graphene::Rect::new(0.0, 0.0, width as f32, height as f32),
            );
        }
    }
}

glib::wrapper! {
    pub struct LottieFrame(ObjectSubclass<frame_paintable::LottieFrame>) @implements gdk::Paintable;
}

impl LottieFrame {
    fn new(size: i32) -> LottieFrame {
        let frame: LottieFrame = glib::Object::new();
        frame.imp().size.set((size, size));
        frame
    }

    fn set_texture(&self, texture: &impl IsA<gdk::Texture>) {
        *self.imp().texture.borrow_mut() = Some(texture.as_ref().clone());
        self.invalidate_contents();
    }
}

struct StickerInner {
    id: u64,
    frame_paintable: LottieFrame,
    /// The owner wants playback (row visible / picker cell hovered).
    wants: Cell<bool>,
    /// A slot was granted and the render thread is ticking this sticker.
    animating: Cell<bool>,
    ready: Cell<bool>,
    frame: Cell<usize>,
    frames: Cell<u64>,
    error: RefCell<Option<String>>,
    ready_callback: RefCell<Option<Rc<dyn Fn()>>>,
    error_callback: RefCell<Option<Rc<dyn Fn(String)>>>,
}

impl Drop for StickerInner {
    fn drop(&mut self) {
        with_registry(|registry| registry.forget(self.id));
    }
}

/// One animated sticker: a caller-owned `gtk::Picture` whose paintable is
/// replaced with a fresh texture per frame. Dropping the last handle stops
/// the animation and frees it on the render thread.
#[derive(Clone)]
pub struct Sticker {
    inner: Rc<StickerInner>,
}

impl Sticker {
    /// Starts loading `path` off the UI thread (`gio::spawn_blocking`) and
    /// renders into `picture` at `size` logical pixels (scaled for the
    /// display). Playback starts only once the owner calls `set_playing`.
    pub fn new(picture: &gtk::Picture, path: &Path, size: i32) -> Sticker {
        let host = registry();
        let id = host.next_id.get();
        host.next_id.set(id.wrapping_add(1));
        let scale = picture.scale_factor().clamp(1, 3);
        let pixels = (size.max(8).saturating_mul(scale)).clamp(8, MAX_PIXELS) as u32;
        let frame_paintable = LottieFrame::new(pixels as i32);
        picture.set_paintable(Some(&frame_paintable));
        let inner = Rc::new(StickerInner {
            id,
            frame_paintable,
            wants: Cell::new(false),
            animating: Cell::new(false),
            ready: Cell::new(false),
            frame: Cell::new(0),
            frames: Cell::new(0),
            error: RefCell::new(None),
            ready_callback: RefCell::new(None),
            error_callback: RefCell::new(None),
        });
        host.stickers
            .borrow_mut()
            .insert(id, Rc::downgrade(&inner));
        let path = path.to_path_buf();
        glib::MainContext::default().spawn_local(async move {
            let read = gio::spawn_blocking(move || std::fs::read(&path)).await;
            let active = registry();
            // The row (or picker cell) may be gone already; never hand the
            // render thread an animation nobody will ever drop.
            if !active.is_alive(id) {
                return;
            }
            match read {
                Ok(Ok(bytes)) => {
                    let _ = active.commands.send(Command::Load {
                        id,
                        bytes,
                        size: pixels,
                    });
                }
                Ok(Err(error)) => active.deliver_error(id, format!("sticker unavailable: {error}")),
                Err(_) => active.deliver_error(id, "sticker unavailable".into()),
            }
        });
        Sticker { inner }
    }

    /// Runs `callback` when the first frame is on screen (or right away if it
    /// already is). One callback per sticker.
    pub fn connect_ready(&self, callback: impl Fn() + 'static) {
        let callback: Rc<dyn Fn()> = Rc::new(callback);
        if self.inner.ready.get() {
            callback();
            return;
        }
        *self.inner.ready_callback.borrow_mut() = Some(callback);
    }

    /// Runs `callback` when the sticker cannot be loaded or rendered (or
    /// right away if it already failed). One callback per sticker.
    pub fn connect_error(&self, callback: impl Fn(String) + 'static) {
        let callback: Rc<dyn Fn(String)> = Rc::new(callback);
        let error = self.inner.error.borrow().clone();
        if let Some(error) = error {
            callback(error);
            return;
        }
        *self.inner.error_callback.borrow_mut() = Some(callback);
    }

    /// The owner's wish: play while the row is visible / the cell is hovered.
    /// Whether it actually animates also depends on the settings, the GTK
    /// animation master switch and a free animation slot.
    pub fn set_playing(&self, playing: bool) {
        if self.inner.wants.replace(playing) == playing {
            return;
        }
        registry().sync(self.inner.id);
    }

    pub fn is_ready(&self) -> bool {
        self.inner.ready.get()
    }

    /// True while the render thread is ticking this sticker.
    pub fn is_animating(&self) -> bool {
        self.inner.animating.get()
    }

    pub fn frame_index(&self) -> usize {
        self.inner.frame.get()
    }

    /// Frames put on screen since the sticker was created (probe helper: a
    /// paused sticker's count stops growing).
    pub fn frames_shown(&self) -> u64 {
        self.inner.frames.get()
    }

    pub fn error(&self) -> Option<String> {
        self.inner.error.borrow().clone()
    }
}

struct Registry {
    commands: mpsc::Sender<Command>,
    next_id: Cell<u64>,
    stickers: RefCell<HashMap<u64, Weak<StickerInner>>>,
    /// Ids holding an animation slot, in grant order.
    slots: RefCell<Vec<u64>>,
}

impl Registry {
    fn start() -> Rc<Registry> {
        let (commands, command_receiver) = mpsc::channel();
        let (wake, wakeups) = async_channel::bounded(1);
        let bus = Arc::new(Bus {
            pending: Mutex::new(Pending::default()),
            wake,
        });
        let thread_bus = bus.clone();
        if let Err(error) = std::thread::Builder::new()
            .name("lottie-render".into())
            .spawn(move || render_thread(command_receiver, thread_bus))
        {
            eprintln!("omarchygram: lottie render thread: {error}");
        }
        let registry = Rc::new(Registry {
            commands,
            next_id: Cell::new(1),
            stickers: RefCell::new(HashMap::new()),
            slots: RefCell::new(Vec::new()),
        });
        {
            let weak = Rc::downgrade(&registry);
            glib::MainContext::default().spawn_local(async move {
                while wakeups.recv().await.is_ok() {
                    let Some(registry) = weak.upgrade() else { return };
                    let (frames, errors) = {
                        let Ok(mut pending) = bus.pending.lock() else {
                            continue;
                        };
                        (
                            std::mem::take(&mut pending.frames),
                            std::mem::take(&mut pending.errors),
                        )
                    };
                    for (id, frame) in frames {
                        registry.deliver_frame(id, frame);
                    }
                    for (id, message) in errors {
                        registry.deliver_error(id, message);
                    }
                }
            });
        }
        if let Some(settings) = gtk::Settings::default() {
            let weak = Rc::downgrade(&registry);
            settings.connect_gtk_enable_animations_notify(move |_| {
                if let Some(registry) = weak.upgrade() {
                    registry.refresh();
                }
            });
        }
        registry
    }

    fn upgrade(&self, id: u64) -> Option<Rc<StickerInner>> {
        let weak = self.stickers.borrow().get(&id).cloned();
        weak.and_then(|weak| weak.upgrade())
    }

    fn is_alive(&self, id: u64) -> bool {
        self.upgrade(id).is_some()
    }

    fn deliver_frame(&self, id: u64, frame: Frame) {
        let Some(sticker) = self.upgrade(id) else {
            self.forget(id);
            return;
        };
        let bytes = glib::Bytes::from_owned(frame.data);
        let texture = gdk::MemoryTexture::new(
            frame.size as i32,
            frame.size as i32,
            gdk::MemoryFormat::R8g8b8a8Premultiplied,
            &bytes,
            frame.size as usize * 4,
        );
        sticker.frame_paintable.set_texture(&texture);
        sticker.frame.set(frame.index);
        sticker.frames.set(sticker.frames.get().wrapping_add(1));
        if !sticker.ready.replace(true) {
            // No registry borrow may be held while a callback runs: it may
            // drop the sticker or ask for playback.
            let callback = sticker.ready_callback.borrow().clone();
            if let Some(callback) = callback {
                callback();
            }
        }
    }

    fn deliver_error(&self, id: u64, message: String) {
        self.release(id);
        let Some(sticker) = self.upgrade(id) else {
            self.forget(id);
            return;
        };
        *sticker.error.borrow_mut() = Some(message.clone());
        let callback = sticker.error_callback.borrow().clone();
        if let Some(callback) = callback {
            callback(message);
        }
    }

    fn forget(&self, id: u64) {
        self.stickers.borrow_mut().remove(&id);
        let held = {
            let mut slots = self.slots.borrow_mut();
            let before = slots.len();
            slots.retain(|slot| *slot != id);
            before != slots.len()
        };
        let _ = self.commands.send(Command::Drop(id));
        if held {
            self.fill_slots();
        }
    }

    fn enabled(&self) -> bool {
        animated_stickers_enabled()
            && gtk::Settings::default().is_none_or(|settings| settings.is_gtk_enable_animations())
    }

    /// Re-evaluates one sticker after its owner changed its wish.
    fn sync(&self, id: u64) {
        let wants = self.enabled() && self.upgrade(id).is_some_and(|sticker| sticker.wants.get());
        if wants {
            self.grant(id);
        } else {
            self.release(id);
        }
    }

    /// Re-evaluates every sticker (settings or GTK master switch changed).
    fn refresh(&self) {
        if self.enabled() {
            self.fill_slots();
            return;
        }
        let held = self.slots.borrow().clone();
        for id in held {
            self.release(id);
        }
    }

    fn grant(&self, id: u64) {
        {
            let slots = self.slots.borrow();
            if slots.contains(&id) || slots.len() >= MAX_ANIMATING {
                return;
            }
        }
        self.slots.borrow_mut().push(id);
        if let Some(sticker) = self.upgrade(id) {
            sticker.animating.set(true);
        }
        let _ = self.commands.send(Command::Play(id));
    }

    fn release(&self, id: u64) {
        let held = {
            let mut slots = self.slots.borrow_mut();
            let before = slots.len();
            slots.retain(|slot| *slot != id);
            before != slots.len()
        };
        if let Some(sticker) = self.upgrade(id) {
            sticker.animating.set(false);
        }
        if held {
            let _ = self.commands.send(Command::Pause(id));
            self.fill_slots();
        }
    }

    /// Hands free slots to the stickers that have waited longest (ids grow
    /// with creation order).
    fn fill_slots(&self) {
        if !self.enabled() {
            return;
        }
        loop {
            let candidate = {
                let slots = self.slots.borrow();
                if slots.len() >= MAX_ANIMATING {
                    return;
                }
                let stickers = self.stickers.borrow();
                let mut waiting = stickers
                    .iter()
                    .filter(|(id, weak)| {
                        !slots.contains(id)
                            && weak
                                .upgrade()
                                .is_some_and(|sticker| sticker.wants.get())
                    })
                    .map(|(id, _)| *id)
                    .collect::<Vec<_>>();
                waiting.sort_unstable();
                waiting.first().copied()
            };
            match candidate {
                Some(id) => self.grant(id),
                None => return,
            }
        }
    }
}

thread_local! {
    static REGISTRY: RefCell<Option<Rc<Registry>>> = const { RefCell::new(None) };
    /// `settings.media.animated_stickers`; kept outside the registry so
    /// reading settings never starts the render thread.
    static ENABLED: Cell<bool> = const { Cell::new(true) };
}

fn registry() -> Rc<Registry> {
    let existing = REGISTRY.with(|cell| cell.borrow().clone());
    if let Some(registry) = existing {
        return registry;
    }
    let registry = Registry::start();
    REGISTRY.with(|cell| *cell.borrow_mut() = Some(registry.clone()));
    registry
}

fn with_registry(f: impl FnOnce(&Rc<Registry>)) {
    let existing = REGISTRY.with(|cell| cell.borrow().clone());
    if let Some(registry) = existing {
        f(&registry);
    }
}

/// `settings.media.animated_stickers` (§1.8): false → first frame only.
pub fn set_animated_stickers_enabled(enabled: bool) {
    if ENABLED.with(|cell| cell.replace(enabled)) == enabled {
        return;
    }
    with_registry(|registry| registry.refresh());
}

pub fn animated_stickers_enabled() -> bool {
    ENABLED.with(Cell::get)
}

/// How many stickers the render thread is ticking (probe helper).
pub fn animating_count() -> usize {
    let mut count = 0;
    with_registry(|registry| count = registry.slots.borrow().len());
    count
}

/// How many stickers are loaded and not dropped yet (probe helper).
pub fn registered_count() -> usize {
    let mut count = 0;
    with_registry(|registry| count = registry.stickers.borrow().len());
    count
}
