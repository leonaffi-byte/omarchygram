pub mod anim;
pub mod auth;
pub mod cards;
pub mod avatar;
pub mod bots;
pub mod call;
pub mod chatlist;
pub mod contacts;
pub mod forward;
pub mod icons;
pub mod info_panel;
pub mod keys;
pub mod keys_view;
/// Wave 6D animated (.tgs) stickers (see specs/spec-wave6.md §5.2).
pub mod lottie;
pub mod markup;
pub mod menus;
pub mod messages;
pub mod player;
pub mod newgroup;
pub mod poll;
pub mod polldialog;
pub mod locationdialog;
mod geolocation;
pub mod scheduled;
pub mod recorder;
pub mod settings_view;
pub mod shell;
pub mod stickers;
pub mod switcher;
pub mod topics;
pub mod viewer;
pub mod profile;
pub mod virtual_chat;
/// Wave 6D rasterizer (orchestrator-owned; see specs/spec-wave6.md §5.1).
pub mod lottie_backend;
/// Wave 6F: stories strip and viewer.
pub mod stories;
/// Wave 6F: video note recorder.
pub mod videonote;

mod video_stream;

/// An optional UI callback; owners capture each other weakly where necessary.
pub type CallbackCell<F> = std::cell::RefCell<Option<std::rc::Rc<F>>>;
pub type CallbackSlot<F> = std::rc::Rc<CallbackCell<F>>;
