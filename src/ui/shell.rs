use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::time::Duration;

use chrono::{DateTime, Local};
use gtk::gdk;
use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use gtk4 as gtk;

use crate::ai::prompts;
use crate::ai::{ChatMessage, Prefs, Role};
use crate::local::Local as LocalServices;
use crate::os::{self, OsPolicy, Parsed};
use crate::settings::{Settings, SettingsStore};
use crate::tg::{
    AuthState, BackendFlags, CallEndReason, CallInfo, CallPhase, ChatInfo, ChatKind, ChatSummary,
    Event, Me, MediaKind, Msg, MuteMode, SharedKind, StoryPeer, StoryRing, Tg, Topic, msg_in_chat,
    split_topic_chat_id, topic_chat_id,
};
use crate::uistate::UiState;

use super::anim::{Effects, RadioGroup, apply_full_phosphor, group_ids, select_radio};
use super::auth::{AuthAction, AuthView};
use super::avatar::Avatar;
use super::call::{CallAction, CallView};
use super::chatlist::{ChatList, SearchRetry, SidebarMode, UnreadUpdate};
use super::contacts::{ContactsAction, ContactsDialog};
use super::forward::{ForwardAction, ForwardDialog, ForwardRequest};
use super::icons;
use super::info_panel::{InfoAction, InfoLayout, InfoPanel};
use super::keys;
use super::lottie;
use super::menus::{self, ChatAction, MainMenuAction, PopoverSlot};
use super::messages::{MediaState, MessageAction, MessagesView};
use super::polldialog::{PollDialog, PollDialogAction};
use super::locationdialog::{LocationDialog, LocationDialogAction};
use super::newgroup::{NewGroupAction, NewGroupDialog};
use super::scheduled::SendLaterPopover;
use super::player;
use super::profile::{ProfileAction, ProfileDialog};
use super::recorder::{
    CancelCommand, RecordTarget, RecorderMachine, StartResolution, StopResolution,
};
use super::settings_view::SettingsView;
use super::stickers::{StickerAction, StickerPicker, StickerSend};
use super::stories::{StoriesStrip, StoryViewer};
use super::switcher::Switcher;
use super::topics::{TopicAction, TopicListView};
use super::viewer::{Viewer, ViewerAction};
use super::virtual_chat::{
    ASSISTANT_CHAT, AuxState, OMARCHY_CHAT, ReqState, VirtualStore, is_virtual, virtual_title,
};

const LOG_RING_CAPACITY: usize = 128;
const PROBE_API_HASH: &str = "0123456789abcdef0123456789abcdef";

thread_local! {
    static LOG_RING: RefCell<VecDeque<String>> = const { RefCell::new(VecDeque::new()) };
}

fn shell_log(args: fmt::Arguments<'_>) {
    let line = args.to_string();
    LOG_RING.with(|ring| {
        let mut ring = ring.borrow_mut();
        if ring.len() == LOG_RING_CAPACITY {
            ring.pop_front();
        }
        ring.push_back(line.clone());
    });
    eprintln!("{line}");
}

fn log_ring_contains(needle: &str) -> bool {
    LOG_RING.with(|ring| ring.borrow().iter().any(|line| line.contains(needle)))
}

macro_rules! shell_log {
    ($($arg:tt)*) => {
        shell_log(format_args!($($arg)*))
    };
}

pub struct Shell {
    pub widget: gtk::Box,
    inner: Rc<ShellInner>,
}

#[derive(Default)]
struct ReadState {
    epoch: u64,
    in_flight: bool,
    latest: i32,
    sent_through: i32,
}

#[derive(Clone, Copy)]
struct FlagsRequest {
    flags: BackendFlags,
    generation: u64,
    session_epoch: u64,
}

#[derive(Clone, Default)]
struct DraftSnapshot {
    text: String,
    reply_to: Option<i32>,
    cursor: i32,
    revision: u64,
}

#[derive(Default)]
struct DraftState {
    current: DraftSnapshot,
    in_flight: bool,
    queued: Option<DraftSnapshot>,
    dirty: bool,
}

#[derive(Default)]
struct InChatSearchState {
    generation: u64,
    query: String,
    hits: Vec<Msg>,
    index: Option<usize>,
    exhausted: bool,
    in_flight: bool,
    retry_before: Option<i32>,
}

#[derive(Clone)]
struct PendingVoice {
    target: RecordTarget,
    path: PathBuf,
    duration: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct VideoRecorderTarget {
    chat_id: i64,
    chat_epoch: u64,
    session_epoch: u64,
    token: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum VideoRecorderPhase {
    #[default]
    Idle,
    Starting(VideoRecorderTarget),
    Recording(VideoRecorderTarget),
    Cancelling(VideoRecorderTarget),
    Stopping(VideoRecorderTarget),
}

#[derive(Default)]
struct VideoRecorderState {
    token: u64,
    phase: VideoRecorderPhase,
    pending_start: Option<VideoRecorderTarget>,
}

#[derive(Clone, Copy)]
struct RestoredUiState {
    sidebar_width: i32,
    sidebar_collapsed: bool,
    folder_id: i32,
    info_panel_open: bool,
    info_width: i32,
}

struct PendingShellTicket {
    ticket: Option<os::ShellTicket>,
}

impl PendingShellTicket {
    fn new(ticket: os::ShellTicket) -> Self {
        Self {
            ticket: Some(ticket),
        }
    }

    fn command(&self) -> &str {
        self.ticket
            .as_ref()
            .expect("pending shell ticket must exist")
            .command()
    }

    fn consume(mut self) -> os::ShellTicket {
        self.ticket.take().expect("pending shell ticket must exist")
    }
}

impl Drop for PendingShellTicket {
    fn drop(&mut self) {
        if let Some(ticket) = self.ticket.take() {
            os::cancel_shell(ticket);
        }
    }
}

impl ShellInner {
    // ----- wave 6C: polls, location, scheduled (spec-wave6 §4) -----

    fn open_poll_dialog(self: &Rc<Self>) {
        if self.open_chat.get().is_none_or(is_virtual) {
            return;
        }
        self.close_forward();
        self.close_viewer();
        self.close_profile();
        self.close_contacts();
        self.close_new_group();
        self.close_stickers();
        self.close_caption_dialog();
        self.close_location_dialog();
        self.switcher.close();
        self.close_settings();
        self.poll_dialog_generation
            .set(self.poll_dialog_generation.get().wrapping_add(1));
        self.poll_dialog.begin();
        self.apply_info_layout(self.current_window_width());
    }

    fn close_poll_dialog(&self) {
        if self.poll_dialog.is_open() {
            self.poll_dialog_generation
                .set(self.poll_dialog_generation.get().wrapping_add(1));
            self.poll_dialog.close();
            self.messages.focus_composer();
            self.apply_info_layout(self.current_window_width());
        }
    }

    fn open_location_dialog(self: &Rc<Self>) {
        if self.open_chat.get().is_none_or(is_virtual) {
            return;
        }
        self.close_forward();
        self.close_viewer();
        self.close_profile();
        self.close_stories_viewer();
        self.close_contacts();
        self.close_new_group();
        self.close_stickers();
        self.close_caption_dialog();
        self.close_poll_dialog();
        self.switcher.close();
        self.close_settings();
        self.updating_live_msg.set(None);
        let last = self.ui_state.borrow().last_location;
        let map_tiles = self.settings.get().media.map_tiles;
        self.location_dialog_generation
            .set(self.location_dialog_generation.get().wrapping_add(1));
        self.location_dialog.begin(last, map_tiles);
        self.apply_info_layout(self.current_window_width());
    }

    fn open_update_live_dialog(self: &Rc<Self>, msg_id: i32) {
        if self.open_chat.get().is_none_or(is_virtual) {
            return;
        }
        let Some(message) = self.messages.message(msg_id) else {
            return;
        };
        let Some(point) = message.location.as_ref().map(|l| l.point) else {
            return;
        };
        self.close_forward();
        self.close_viewer();
        self.close_profile();
        self.close_stories_viewer();
        self.close_contacts();
        self.close_new_group();
        self.close_stickers();
        self.close_caption_dialog();
        self.close_poll_dialog();
        self.switcher.close();
        self.close_settings();
        self.updating_live_msg.set(Some(msg_id));
        let map_tiles = self.settings.get().media.map_tiles;
        self.location_dialog_generation
            .set(self.location_dialog_generation.get().wrapping_add(1));
        self.location_dialog.begin_update(point, map_tiles);
        self.apply_info_layout(self.current_window_width());
    }

    fn close_location_dialog(&self) {
        if self.location_dialog.is_open() {
            self.location_dialog_generation
                .set(self.location_dialog_generation.get().wrapping_add(1));
            self.updating_live_msg.set(None);
            self.location_dialog.close();
            self.messages.focus_composer();
            self.apply_info_layout(self.current_window_width());
        }
    }

    fn close_stories_viewer(&self) {
        if self.stories_viewer.is_open() {
            self.stories_viewer.close();
            self.messages.focus_composer();
            self.apply_info_layout(self.current_window_width());
        }
    }

    fn handle_poll_dialog_action(self: Rc<Self>, action: PollDialogAction) {
        let draft = match action {
            PollDialogAction::Close => {
                self.close_poll_dialog();
                return;
            }
            PollDialogAction::Create(draft) => draft,
        };
        let Some(chat_id) = self.open_chat.get().filter(|id| !is_virtual(*id)) else {
            self.close_poll_dialog();
            return;
        };
        let Some(session_epoch) = self.begin_mutation() else {
            return;
        };
        let epoch = self.epoch.get();
        let dialog_generation = self.poll_dialog_generation.get();
        let title = self.title_for(chat_id);
        self.poll_dialog.set_busy(true);
        glib::MainContext::default().spawn_local(async move {
            let result = self.tg.send_poll(chat_id, draft).await;
            self.finish_mutation();
            if !self.is_session_current(session_epoch) {
                return;
            }
            match result {
                Ok(message) => {
                    if self.poll_dialog_operation_is_current(chat_id, epoch, dialog_generation) {
                        self.close_poll_dialog();
                    }
                    let message = self.apply_tombstone(message);
                    if !message.deleted {
                        self.remember_last(&message);
                        self.dialog_upsert(
                            chat_id,
                            &title,
                            &message_preview(&message),
                            Some(message.ts),
                            UnreadUpdate::Delta(0),
                        );
                    }
                    if self.is_current(chat_id, epoch) {
                        let inserted = self.messages.merge_event(message);
                        self.post_render(inserted);
                    }
                }
                Err(error) => {
                    shell_log!("send_poll({chat_id}): {error}");
                    if self.poll_dialog_operation_is_current(chat_id, epoch, dialog_generation) {
                        self.poll_dialog.show_error(&error);
                    }
                }
            }
        });
    }

    fn handle_location_dialog_action(self: Rc<Self>, action: LocationDialogAction) {
        let (point, live_secs) = match action {
            LocationDialogAction::Close => {
                self.close_location_dialog();
                return;
            }
            LocationDialogAction::Send { point, live_secs } => (point, live_secs),
        };
        let Some(chat_id) = self.open_chat.get().filter(|id| !is_virtual(*id)) else {
            self.close_location_dialog();
            return;
        };
        let Some(session_epoch) = self.begin_mutation() else {
            return;
        };
        let epoch = self.epoch.get();
        let dialog_generation = self.location_dialog_generation.get();
        let updating_msg = self.updating_live_msg.take();
        if let Some(msg_id) = updating_msg {
            self.location_dialog.set_busy(true);
            let tg = self.tg.clone();
            let this = self.clone();
            glib::MainContext::default().spawn_local(async move {
                let result = tg.update_live_location(chat_id, msg_id, point).await;
                this.finish_mutation();
                if !this.is_session_current(session_epoch) {
                    return;
                }
                match result {
                    Ok(()) => {
                        if this.location_dialog_operation_is_current(
                            chat_id,
                            epoch,
                            dialog_generation,
                        ) {
                            this.close_location_dialog();
                        }
                    }
                    Err(error) => {
                        shell_log!("update_live_location({chat_id}, {msg_id}): {error}");
                        // The dialog remains the same update operation after a
                        // retryable failure; Retry must not become an ordinary
                        // new-location send — but only while this is still the
                        // dialog instance that started the update.
                        if this.location_dialog_operation_is_current(
                            chat_id,
                            epoch,
                            dialog_generation,
                        ) {
                            this.updating_live_msg.set(Some(msg_id));
                            this.location_dialog.show_error(&error);
                        }
                    }
                }
            });
            return;
        }
        self.ui_state.borrow_mut().last_location = Some((point.lat, point.lon));
        self.schedule_ui_save();
        let title = self.title_for(chat_id);
        self.location_dialog.set_busy(true);
        let this = self.clone();
        glib::MainContext::default().spawn_local(async move {
            let result = match live_secs {
                Some(secs) => this.tg.send_live_location(chat_id, point, secs).await,
                None => this.tg.send_location(chat_id, point).await,
            };
            this.finish_mutation();
            if !this.is_session_current(session_epoch) {
                return;
            }
            match result {
                Ok(message) => {
                    if this.location_dialog_operation_is_current(
                        chat_id,
                        epoch,
                        dialog_generation,
                    ) {
                        this.close_location_dialog();
                    }
                    let message = this.apply_tombstone(message);
                    if !message.deleted {
                        this.remember_last(&message);
                        this.dialog_upsert(
                            chat_id,
                            &title,
                            &message_preview(&message),
                            Some(message.ts),
                            UnreadUpdate::Delta(0),
                        );
                    }
                    if let Some(loc) = &message.location
                        && let Some(live) = &loc.live {
                            this.track_own_live_location(chat_id, message.id, live.expires);
                        }
                    if this.is_current(chat_id, epoch) {
                        let inserted = this.messages.merge_event(message);
                        this.post_render(inserted);
                    }
                }
                Err(error) => {
                    shell_log!("send_location({chat_id}): {error}");
                    if this.location_dialog_operation_is_current(
                        chat_id,
                        epoch,
                        dialog_generation,
                    ) {
                        this.location_dialog.show_error(&error);
                    }
                }
            }
        });
    }

    /// Schedule the composer's text (spec §4.4). Nothing lands in the
    /// history: the message shows up in `get_scheduled` and the backend
    /// fires `Event::ScheduledChanged`.
    fn send_text_later(self: Rc<Self>, chat_id: i64, at: DateTime<Local>) {
        if self.composer_operation.get() || self.messages.is_busy() {
            return;
        }
        let text = self.messages.composer_text();
        if text.trim().is_empty() {
            return;
        }
        let Some(session_epoch) = self.begin_mutation() else {
            return;
        };
        let epoch = self.epoch.get();
        let reply_to = self.messages.reply_to();
        let token = self.acquire_composer();
        self.messages.clear_error();
        self.messages.set_busy(true);
        glib::MainContext::default().spawn_local(async move {
            let result = self.tg.send_text_at(chat_id, &text, reply_to, at).await;
            self.finish_mutation();
            if !self.is_session_current(session_epoch) {
                return;
            }
            let owns_composer = self.release_composer(token);
            match result {
                Ok(()) => {
                    let epoch_is_current = self.is_current(chat_id, epoch);
                    self.messages
                        .complete_text_operation(&text, epoch_is_current);
                    self.clear_sent_draft(chat_id);
                }
                Err(error) => {
                    shell_log!("send_text_at({chat_id}): {error}");
                    if self.is_current(chat_id, epoch) {
                        self.messages.show_error(&error);
                        self.effects.error_flash(&self.overlay);
                    }
                }
            }
            if owns_composer {
                self.messages.set_busy(false);
            }
        });
    }

    /// Schedule a file picked in the caption dialog (spec §4.4).
    fn send_file_later(
        self: Rc<Self>,
        path: PathBuf,
        context: FileSendContext,
        caption: String,
        composer_snapshot: String,
        at: DateTime<Local>,
    ) {
        let FileSendContext { chat_id, epoch, session_epoch, token } = context;
        glib::MainContext::default().spawn_local(async move {
            if !self.is_session_current(session_epoch) || self.begin_mutation().is_none() {
                return;
            }
            let result = self.tg.send_file_at(chat_id, path, &caption, at).await;
            self.finish_mutation();
            if !self.is_session_current(session_epoch) {
                return;
            }
            let owns_composer = self.release_composer(token);
            match result {
                Ok(()) => {
                    let epoch_is_current = self.is_current(chat_id, epoch);
                    self.messages
                        .complete_text_operation(&composer_snapshot, epoch_is_current);
                    self.clear_sent_draft(chat_id);
                }
                Err(error) => {
                    shell_log!("send_file_at({chat_id}): {error}");
                    if self.is_current(chat_id, epoch) {
                        self.messages.show_error(&error);
                        self.effects.error_flash(&self.overlay);
                    }
                }
            }
            if owns_composer {
                self.messages.set_busy(false);
            }
        });
    }

    fn send_scheduled_now(self: Rc<Self>, chat_id: i64, ids: Vec<i32>) {
        let Some(session_epoch) = self.begin_mutation() else {
            return;
        };
        let epoch = self.epoch.get();
        glib::MainContext::default().spawn_local(async move {
            let result = self.tg.send_scheduled_now(chat_id, ids).await;
            self.finish_mutation();
            if !self.is_session_current(session_epoch) {
                return;
            }
            if let Err(error) = result {
                shell_log!("send_scheduled_now({chat_id}): {error}");
                if self.is_current(chat_id, epoch) {
                    self.messages.show_error(&error);
                }
            }
        });
    }

    fn delete_scheduled(self: Rc<Self>, chat_id: i64, ids: Vec<i32>) {
        let Some(session_epoch) = self.begin_mutation() else {
            return;
        };
        let epoch = self.epoch.get();
        glib::MainContext::default().spawn_local(async move {
            let result = self.tg.delete_scheduled(chat_id, ids).await;
            self.finish_mutation();
            if !self.is_session_current(session_epoch) {
                return;
            }
            if let Err(error) = result {
                shell_log!("delete_scheduled({chat_id}): {error}");
                if self.is_current(chat_id, epoch) {
                    self.messages.show_error(&error);
                }
            }
        });
    }

    /// Refetch the scheduled list of `chat_id` (open_chat and every
    /// `Event::ScheduledChanged`). A failure leaves the strip alone and only
    /// logs: it is a decoration, never a blocker.
    fn refresh_scheduled(self: Rc<Self>, chat_id: i64) {
        if is_virtual(chat_id) {
            return;
        }
        let session_epoch = self.session_epoch.get();
        let epoch = self.epoch.get();
        let generation = self.scheduled_refresh_generation.get().wrapping_add(1);
        self.scheduled_refresh_generation.set(generation);
        glib::MainContext::default().spawn_local(async move {
            let result = self.tg.get_scheduled(chat_id).await;
            match result {
                Ok(messages) => {
                    self.apply_scheduled_refresh(
                        chat_id,
                        epoch,
                        session_epoch,
                        generation,
                        messages,
                    );
                }
                Err(error)
                    if self.scheduled_refresh_is_current(
                        chat_id,
                        epoch,
                        session_epoch,
                        generation,
                    ) =>
                {
                    shell_log!("get_scheduled({chat_id}): {error}");
                }
                Err(_) => {}
            }
        });
    }

    fn poll_dialog_operation_is_current(
        &self,
        chat_id: i64,
        epoch: u64,
        generation: u64,
    ) -> bool {
        self.is_current(chat_id, epoch)
            && self.poll_dialog.is_open()
            && self.poll_dialog_generation.get() == generation
    }

    fn location_dialog_operation_is_current(
        &self,
        chat_id: i64,
        epoch: u64,
        generation: u64,
    ) -> bool {
        self.is_current(chat_id, epoch)
            && self.location_dialog.is_open()
            && self.location_dialog_generation.get() == generation
    }

    fn scheduled_refresh_is_current(
        &self,
        chat_id: i64,
        epoch: u64,
        session_epoch: u64,
        generation: u64,
    ) -> bool {
        self.is_session_current(session_epoch)
            && self.is_current(chat_id, epoch)
            && self.scheduled_refresh_generation.get() == generation
    }

    fn apply_scheduled_refresh(
        &self,
        chat_id: i64,
        epoch: u64,
        session_epoch: u64,
        generation: u64,
        messages: Vec<Msg>,
    ) -> bool {
        if !self.scheduled_refresh_is_current(chat_id, epoch, session_epoch, generation) {
            return false;
        }
        self.messages.set_scheduled_msgs(messages);
        true
    }
}

struct ShellInner {
    widget: gtk::Box,
    tg: Tg,
    local: LocalServices,
    probe: bool,
    stack: gtk::Stack,
    auth: AuthView,
    chatlist: ChatList,
    messages: MessagesView,
    call: CallView,
    topics: TopicListView,
    content_area: gtk::Stack,
    forum_list_open: Cell<bool>,
    /// SwitchInline text waiting for the chat the switcher picks (§6.1).
    pending_inline_query: RefCell<Option<String>>,
    forum_topics: RefCell<Vec<Topic>>,
    /// Only the newest get_topics completion may replace the visible list.
    forum_topics_generation: Cell<u64>,
    info: InfoPanel,
    profile: ProfileDialog,
    contacts: ContactsDialog,
    new_group: NewGroupDialog,
    stickers: StickerPicker,
    forward: ForwardDialog,
    viewer: Viewer,
    playback_generation: Cell<u64>,
    poll_dialog: Rc<PollDialog>,
    location_dialog: Rc<LocationDialog>,
    /// Per-open tokens. Late completions must not close, unlock, or put an
    /// error into a dialog that was cancelled and opened again.
    poll_dialog_generation: Cell<u64>,
    location_dialog_generation: Cell<u64>,
    /// Orders concurrent `get_scheduled` calls; only the newest may paint.
    scheduled_refresh_generation: Cell<u64>,
    stories_strip: Rc<StoriesStrip>,
    stories_viewer: Rc<StoryViewer>,
    stories_peers: RefCell<Vec<StoryPeer>>,
    stories_generation: Cell<u64>,
    updating_live_msg: Cell<Option<i32>>,
    own_live_locations: RefCell<Vec<(i64, i32, DateTime<Local>)>>,
    live_expiry_timer: RefCell<Option<glib::SourceId>>,
    /// Tokenized by chat and session. Only one local video start/cancel/stop
    /// command is dispatched at a time; a restart waits in `pending_start`.
    video_recorder: RefCell<VideoRecorderState>,
    /// Probe-only delay after `video_start` has created its process and before
    /// the receiver is attached. This makes cancel/restart-during-start
    /// deterministic. Always zero outside `--probe`.
    probe_video_start_attach_delay: Cell<u64>,
    paned: gtk::Paned,
    content_paned: gtk::Paned,
    effects: Rc<Effects>,
    overlay: gtk::Overlay,
    switcher: Switcher,
    settings: Rc<SettingsStore>,
    settings_view: SettingsView,
    clock_source: RefCell<Option<glib::SourceId>>,
    theme_monitor: RefCell<Option<gio::FileMonitor>>,
    theme_switch_timeout: RefCell<Option<glib::SourceId>>,
    dialogs_error_box: gtk::Box,
    dialogs_error: gtk::Label,
    dialogs_spinner: gtk::Spinner,
    dialogs_retry: gtk::Button,
    epoch: Cell<u64>,
    open_chat: Cell<Option<i64>>,
    session_ready: Cell<bool>,
    presence_last_activity: Cell<std::time::Instant>,
    presence_online: Cell<Option<bool>>,
    session_epoch: Cell<u64>,
    event_loop_started: Cell<bool>,
    started: Cell<bool>,
    dialogs_loaded: Cell<bool>,
    dialogs_in_flight: Cell<bool>,
    dialogs_refresh_again: Cell<bool>,
    dialogs_revision: Cell<u64>,
    dialogs_reload_timeout: RefCell<Option<glib::SourceId>>,
    window_hooked: Cell<bool>,
    composer_operation: Cell<bool>,
    /// Identifies the operation that currently owns `composer_operation`
    /// (C5/A7). A completion may only release the lock it still owns, so a
    /// late start/stop/send resolution cannot unlock a composer that another
    /// operation took over after a chat switch.
    composer_token: Cell<u64>,
    /// The composer token the recorder machine holds from `begin_start` (or a
    /// send retry) until its final resolution.
    recorder_token: Cell<u64>,
    /// Last text `SelectionCopy` put on the clipboard (probe hook).
    probe_copied: RefCell<String>,
    /// Probe hook: milliseconds to hold `record_start` before awaiting it, so
    /// the A8 cancel-during-Starting path can be exercised deterministically.
    /// Always 0 outside `--probe`.
    probe_record_start_delay: Cell<u64>,
    mutations_in_flight: Cell<u32>,
    pending_message_id: Cell<i32>,
    mark_reads: RefCell<HashMap<i64, ReadState>>,
    last_by_chat: RefCell<HashMap<i64, Msg>>,
    settings_gen: Cell<u64>,
    last_applied_settings: RefCell<Settings>,
    tombstones: RefCell<HashMap<i64, HashSet<i32>>>,
    virtual_stores: RefCell<HashMap<i64, VirtualStore>>,
    aux: RefCell<AuxState>,
    transcription_active: RefCell<HashSet<(i64, i32)>>,
    recent_real_chats: RefCell<Vec<i64>>,
    recent_incoming: RefCell<HashMap<i64, DateTime<Local>>>,
    flags_initialized: Cell<bool>,
    desired_flags: Cell<BackendFlags>,
    anti_reload_pending: Cell<bool>,
    flags_in_flight: Cell<bool>,
    flags_pending: RefCell<Option<FlagsRequest>>,
    typing_timeout: RefCell<Option<glib::SourceId>>,
    search_timeout: RefCell<Option<glib::SourceId>>,
    ui_state: RefCell<UiState>,
    ui_save_timeout: RefCell<Option<glib::SourceId>>,
    layout_tick: RefCell<Option<gtk::TickCallbackId>>,
    /// True while the frame-clock tick callback drives the layout. A27
    /// reparents the info panel between the Paned column and the Overlay
    /// sheet; doing that inside the frame cycle is not safe, so the tick
    /// only ever schedules the transition for the next idle.
    in_layout_tick: Cell<bool>,
    info_layout_idle: RefCell<Option<glib::SourceId>>,
    /// Set once in `wire`, so `&self` methods can schedule idle work.
    weak_self: RefCell<Weak<Self>>,
    probe_window_width: Cell<Option<i32>>,
    applying_sidebar_layout: Cell<bool>,
    applying_info_layout: Cell<bool>,
    effective_sidebar_collapsed: Cell<bool>,
    main_menu: PopoverSlot,
    me: RefCell<Option<Me>>,
    me_loading: Cell<bool>,
    chat_info: RefCell<HashMap<i64, ChatInfo>>,
    drafts: RefCell<HashMap<i64, DraftState>>,
    draft_timeouts: RefCell<HashMap<i64, glib::SourceId>>,
    pending_mutes: RefCell<HashMap<i64, bool>>,
    manual_unread_hold: RefCell<HashSet<i64>>,
    read_outbox: RefCell<HashMap<i64, i32>>,
    quote_requests: RefCell<HashSet<(i64, u64, i32)>>,
    in_chat_search: RefCell<InChatSearchState>,
    pinned_generation: Cell<u64>,
    available_reactions_generation: Cell<u64>,
    forward_generation: Cell<u64>,
    viewer_generation: Cell<u64>,
    reaction_generations: RefCell<HashMap<(i64, i32), u64>>,
    message_change_generations: RefCell<HashMap<(i64, i32), u64>>,
    reaction_retry: RefCell<Option<(i64, i32, String)>>,
    recorder: RefCell<RecorderMachine>,
    voice_retry: RefCell<Option<PendingVoice>>,
    caption_dialog: RefCell<Option<gtk::Window>>,
    probe_notifications: Cell<u64>,
    probe_notification_avatar: RefCell<Option<(i64, PathBuf)>>,
    notification_sequence: Cell<u64>,
    pending_notifications: RefCell<HashMap<i64, u64>>,
    probe_restored_ui_state: Option<RestoredUiState>,
    smoke_hook_done: Cell<bool>,
    probe_started: Cell<bool>,
    auth_probe_started: Cell<bool>,
    probe_answer: Cell<Option<usize>>,
    probe_media_launches: Cell<u64>,
    probe_uri_launches: Cell<u64>,
}

impl Shell {
    pub fn is_ready(&self) -> bool { self.inner.session_ready.get() }
    pub fn new(tg: Tg, probe: bool) -> Shell {
        let auth = AuthView::new();
        let settings = SettingsStore::new();
        let effects = Effects::new(settings.clone());
        let chatlist = ChatList::new(effects.clone(), tg.clone());
        let messages = MessagesView::new(effects.clone());
        let header_group = gtk::SizeGroup::new(gtk::SizeGroupMode::Vertical);
        header_group.add_widget(&chatlist.top_bar());
        if let Some(header) = messages.widget.first_child() { header_group.add_widget(&header); }
        messages.set_player_settings(settings.clone());
        messages.set_probe(probe);
        let call = CallView::new(tg.clone(), probe, settings.get().calls.ringtone, settings.clone());
        let topics = TopicListView::new();
        let content_area = gtk::Stack::new();
        content_area.set_transition_type(gtk::StackTransitionType::None);
        content_area.add_named(&messages.widget, Some("messages"));
        content_area.add_named(&topics.widget, Some("topics"));
        content_area.set_visible_child_name("messages");
        let info = InfoPanel::new(tg.clone());
        let profile = ProfileDialog::new();
        let contacts = ContactsDialog::new(tg.clone());
        let new_group = NewGroupDialog::new();
        let stickers = StickerPicker::new();
        let forward = ForwardDialog::new();
        let viewer = Viewer::new();
        // §2.2: video fullscreen is a shell overlay, not a second window.
        let video_fullscreen = player::Fullscreen::new();
        player::install_fullscreen(video_fullscreen.clone());
        let switcher = Switcher::new();
        let last_applied_settings = settings.get();
        let settings_view = SettingsView::new(settings.clone(), effects.clone());
        let local = LocalServices::spawn();

        let main = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let dialogs_error_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        dialogs_error_box.set_halign(gtk::Align::Center);
        dialogs_error_box.set_margin_start(8);
        dialogs_error_box.set_margin_end(8);
        dialogs_error_box.set_margin_top(8);
        dialogs_error_box.set_margin_bottom(8);
        dialogs_error_box.set_visible(false);
        let dialogs_spinner = gtk::Spinner::new();
        dialogs_spinner.set_visible(false);
        dialogs_error_box.append(&dialogs_spinner);
        let dialogs_error = gtk::Label::new(None);
        dialogs_error.add_css_class("omg-error");
        dialogs_error.set_wrap(true);
        dialogs_error_box.append(&dialogs_error);
        let dialogs_retry = gtk::Button::with_label("Retry");
        dialogs_retry.add_css_class("omg-primary");
        dialogs_error_box.append(&dialogs_retry);
        main.append(&dialogs_error_box);

        let (ui_state, probe_restored_ui_state) = load_shell_ui_state(probe);
        let paned = gtk::Paned::new(gtk::Orientation::Horizontal);
        paned.add_css_class("omg-handle");
        paned.set_hexpand(true);
        paned.set_vexpand(true);
        paned.set_start_child(Some(&chatlist.widget));
        let content_paned = gtk::Paned::new(gtk::Orientation::Horizontal);
        content_paned.add_css_class("omg-handle");
        content_paned.set_hexpand(true);
        content_paned.set_vexpand(true);
        content_paned.set_start_child(Some(&content_area));
        content_paned.set_resize_start_child(true);
        content_paned.set_shrink_start_child(false);
        content_paned.set_resize_end_child(false);
        content_paned.set_shrink_end_child(false);
        paned.set_end_child(Some(&content_paned));
        paned.set_resize_start_child(false);
        paned.set_shrink_start_child(ui_state.sidebar_collapsed);
        paned.set_position(if ui_state.sidebar_collapsed {
            64
        } else {
            ui_state.sidebar_width
        });
        chatlist.set_collapsed(ui_state.sidebar_collapsed);
        main.append(&paned);

        let stack = gtk::Stack::new();
        stack.set_transition_type(gtk::StackTransitionType::None);
        stack.add_named(&auth.widget, Some("auth"));
        stack.add_named(&main, Some("main"));
        stack.add_named(&settings_view.widget, Some("settings"));
        stack.set_visible_child_name("auth");

        // Effects live in a nested overlay. The switcher belongs to the outer
        // overlay, so atmosphere/launch layers can never paint over Ctrl+K.
        let poll_dialog = PollDialog::new();
        let location_dialog = LocationDialog::new(tg.clone());
        let stories_strip = Rc::new(StoriesStrip::new(tg.clone()));
        stories_strip.set_compact(ui_state.sidebar_collapsed);
        chatlist.set_stories_strip(stories_strip.widget.upcast_ref());
        let stories_viewer = StoryViewer::new(tg.clone());
        let overlay = gtk::Overlay::new();
        overlay.set_child(Some(&stack));
        overlay.add_overlay(&info.widget);
        overlay.add_overlay(&profile.widget);
        overlay.add_overlay(&viewer.widget);
        overlay.add_overlay(&video_fullscreen.widget);
        overlay.add_overlay(&stories_viewer.widget);
        overlay.add_overlay(&forward.widget);
        overlay.add_overlay(&contacts.widget);
        overlay.add_overlay(&new_group.widget);
        overlay.add_overlay(&poll_dialog.widget);
        overlay.add_overlay(&location_dialog.widget);
        overlay.add_overlay(&topics.dialog);
        overlay.add_overlay(&call.widget);
        overlay.set_hexpand(true);
        overlay.set_vexpand(true);
        let shell_overlay = gtk::Overlay::new();
        shell_overlay.set_child(Some(&overlay));
        shell_overlay.add_overlay(&switcher.widget);
        shell_overlay.set_hexpand(true);
        shell_overlay.set_vexpand(true);

        let widget = gtk::Box::new(gtk::Orientation::Vertical, 0);
        widget.set_hexpand(true);
        widget.set_vexpand(true);
        widget.append(&shell_overlay);

        let mut virtual_stores = HashMap::new();
        virtual_stores.insert(ASSISTANT_CHAT, VirtualStore::default());
        virtual_stores.insert(OMARCHY_CHAT, VirtualStore::default());
        let inner = Rc::new(ShellInner {
            widget: widget.clone(),
            tg,
            local,
            probe,
            stack,
            auth,
            chatlist,
            messages,
            call,
            topics,
            content_area,
            forum_list_open: Cell::new(false),
            pending_inline_query: RefCell::new(None),
            forum_topics: RefCell::new(Vec::new()),
            forum_topics_generation: Cell::new(0),
            info,
            profile,
            contacts,
            new_group,
            stickers,
            forward,
            viewer,
            playback_generation: Cell::new(0),
            poll_dialog,
            location_dialog,
            poll_dialog_generation: Cell::new(0),
            location_dialog_generation: Cell::new(0),
            scheduled_refresh_generation: Cell::new(0),
            stories_strip,
            stories_viewer,
            stories_peers: RefCell::new(Vec::new()),
            stories_generation: Cell::new(0),
            updating_live_msg: Cell::new(None),
            own_live_locations: RefCell::new(Vec::new()),
            live_expiry_timer: RefCell::new(None),
            video_recorder: RefCell::new(VideoRecorderState::default()),
            probe_video_start_attach_delay: Cell::new(0),
            paned,
            content_paned,
            effects,
            overlay,
            switcher,
            settings,
            settings_view,
            clock_source: RefCell::new(None),
            theme_monitor: RefCell::new(None),
            theme_switch_timeout: RefCell::new(None),
            dialogs_error_box,
            dialogs_error,
            dialogs_spinner,
            dialogs_retry: dialogs_retry.clone(),
            epoch: Cell::new(0),
            open_chat: Cell::new(None),
            session_ready: Cell::new(false),
            presence_last_activity: Cell::new(std::time::Instant::now()),
            presence_online: Cell::new(None),
            session_epoch: Cell::new(0),
            event_loop_started: Cell::new(false),
            started: Cell::new(false),
            dialogs_loaded: Cell::new(false),
            dialogs_in_flight: Cell::new(false),
            dialogs_refresh_again: Cell::new(false),
            dialogs_revision: Cell::new(0),
            dialogs_reload_timeout: RefCell::new(None),
            window_hooked: Cell::new(false),
            composer_operation: Cell::new(false),
            composer_token: Cell::new(0),
            recorder_token: Cell::new(0),
            probe_copied: RefCell::new(String::new()),
            probe_record_start_delay: Cell::new(0),
            mutations_in_flight: Cell::new(0),
            pending_message_id: Cell::new(i32::MAX),
            mark_reads: RefCell::new(HashMap::new()),
            last_by_chat: RefCell::new(HashMap::new()),
            settings_gen: Cell::new(0),
            last_applied_settings: RefCell::new(last_applied_settings),
            tombstones: RefCell::new(HashMap::new()),
            virtual_stores: RefCell::new(virtual_stores),
            aux: RefCell::new(AuxState::default()),
            transcription_active: RefCell::new(HashSet::new()),
            recent_real_chats: RefCell::new(Vec::new()),
            recent_incoming: RefCell::new(HashMap::new()),
            flags_initialized: Cell::new(false),
            desired_flags: Cell::new(BackendFlags::default()),
            anti_reload_pending: Cell::new(false),
            flags_in_flight: Cell::new(false),
            flags_pending: RefCell::new(None),
            typing_timeout: RefCell::new(None),
            search_timeout: RefCell::new(None),
            ui_state: RefCell::new(ui_state),
            ui_save_timeout: RefCell::new(None),
            layout_tick: RefCell::new(None),
            in_layout_tick: Cell::new(false),
            info_layout_idle: RefCell::new(None),
            weak_self: RefCell::new(Weak::new()),
            probe_window_width: Cell::new(None),
            applying_sidebar_layout: Cell::new(false),
            applying_info_layout: Cell::new(false),
            effective_sidebar_collapsed: Cell::new(false),
            main_menu: PopoverSlot::default(),
            me: RefCell::new(None),
            me_loading: Cell::new(false),
            chat_info: RefCell::new(HashMap::new()),
            drafts: RefCell::new(HashMap::new()),
            draft_timeouts: RefCell::new(HashMap::new()),
            pending_mutes: RefCell::new(HashMap::new()),
            manual_unread_hold: RefCell::new(HashSet::new()),
            read_outbox: RefCell::new(HashMap::new()),
            quote_requests: RefCell::new(HashSet::new()),
            in_chat_search: RefCell::new(InChatSearchState::default()),
            pinned_generation: Cell::new(0),
            available_reactions_generation: Cell::new(0),
            forward_generation: Cell::new(0),
            viewer_generation: Cell::new(0),
            reaction_generations: RefCell::new(HashMap::new()),
            message_change_generations: RefCell::new(HashMap::new()),
            reaction_retry: RefCell::new(None),
            recorder: RefCell::new(RecorderMachine::default()),
            voice_retry: RefCell::new(None),
            caption_dialog: RefCell::new(None),
            probe_notifications: Cell::new(0),
            probe_notification_avatar: RefCell::new(None),
            notification_sequence: Cell::new(0),
            pending_notifications: RefCell::new(HashMap::new()),
            probe_restored_ui_state,
            smoke_hook_done: Cell::new(false),
            probe_started: Cell::new(false),
            auth_probe_started: Cell::new(false),
            probe_answer: Cell::new(None),
            probe_media_launches: Cell::new(0),
            probe_uri_launches: Cell::new(0),
        });
        ShellInner::wire(&inner, dialogs_retry);
        Shell { widget, inner }
    }

    pub async fn start(&self) {
        self.inner.clone().start_backend().await;
    }

    pub fn toggle_sidebar(&self) {
        self.inner.toggle_sidebar();
    }

    pub fn focus_search(&self) {
        self.inner.focus_search();
    }

    pub fn open_contacts(&self) {
        self.inner.open_contacts();
    }

    pub fn open_saved_messages(&self) {
        self.inner.clone().open_saved_messages();
    }
}

impl ShellInner {
    fn wire(this: &Rc<Self>, dialogs_retry: gtk::Button) {
        *this.weak_self.borrow_mut() = Rc::downgrade(this);
        {
            let weak = Rc::downgrade(this);
            this.auth.set_action(Rc::new(move |action| {
                if let Some(this) = weak.upgrade() {
                    this.handle_auth_action(action);
                }
            }));
            this.settings_view.set_tg(this.tg.clone());
            let weak = Rc::downgrade(this);
            this.settings_view.set_on_logout(Rc::new(move || {
                if let Some(this) = weak.upgrade() {
                    this.log_out();
                }
            }));
        }
        {
            let weak = Rc::downgrade(this);
            this.chatlist.set_on_open(Rc::new(move |chat_id| {
                if let Some(this) = weak.upgrade() {
                    this.open_chat(chat_id);
                }
            }));
        }
        {
            let weak = Rc::downgrade(this);
            this.chatlist
                .set_on_search(Rc::new(move |query, generation| {
                    if let Some(this) = weak.upgrade() {
                        this.sidebar_search_changed(query, generation);
                    }
                }));
        }
        {
            let weak = Rc::downgrade(this);
            this.chatlist
                .set_on_search_open(Rc::new(move |chat_id, msg_id| {
                    if let Some(this) = weak.upgrade() {
                        this.open_search_result(chat_id, msg_id);
                    }
                }));
        }
        {
            let weak = Rc::downgrade(this);
            this.chatlist
                .set_on_search_retry(Rc::new(move |kind, query, generation| {
                    if let Some(this) = weak.upgrade() {
                        this.retry_sidebar_search(kind, query, generation);
                    }
                }));
        }
        {
            let weak = Rc::downgrade(this);
            this.chatlist.set_on_folder(Rc::new(move |folder_id| {
                if let Some(this) = weak.upgrade() {
                    this.ui_state.borrow_mut().folder_id = folder_id;
                    this.schedule_ui_save();
                }
            }));
        }
        {
            let weak = Rc::downgrade(this);
            this.chatlist.set_on_main_menu(Rc::new(move || {
                if let Some(this) = weak.upgrade() {
                    this.open_main_menu();
                }
            }));
        }
        {
            let weak = Rc::downgrade(this);
            this.chatlist
                .set_on_chat_action(Rc::new(move |chat_id, action| {
                    if let Some(this) = weak.upgrade() {
                        this.handle_chat_action(chat_id, action);
                    }
                }));
        }
        {
            let weak = Rc::downgrade(this);
            this.switcher.set_on_open(Rc::new(move |chat_id| {
                if let Some(this) = weak.upgrade() {
                    this.open_chat(chat_id);
                }
            }));
        }
        {
            let weak = Rc::downgrade(this);
            this.switcher.set_on_cancel(Rc::new(move || {
                if let Some(this) = weak.upgrade() {
                    this.pending_inline_query.borrow_mut().take();
                    this.clear_window_focus();
                    this.messages.focus_composer();
                }
            }));
        }
        {
            let weak = Rc::downgrade(this);
            this.messages.set_action(Rc::new(move |action| {
                if let Some(this) = weak.upgrade() {
                    this.handle_message_action(action);
                }
            }));
        }
        {
            let weak = Rc::downgrade(this);
            this.call.set_action(Rc::new(move |action| {
                if let Some(this) = weak.upgrade() {
                    this.handle_call_action(action);
                }
            }));
            let weak = Rc::downgrade(this);
            this.call.set_on_closed(Rc::new(move || {
                if let Some(this) = weak.upgrade().filter(|this| this.session_ready.get()) {
                    this.messages.focus_composer();
                    this.apply_info_layout(this.current_window_width());
                }
            }));
        }
        {
            let weak = Rc::downgrade(this);
            this.topics.set_action(Rc::new(move |action| {
                let Some(this) = weak.upgrade() else {
                    return;
                };
                match action {
                    TopicAction::OpenTopic(chat_id) => this.clone().open_chat(chat_id),
                    TopicAction::CreateTopic(title) => {
                        this.clone().create_topic_from_dialog(title);
                    }
                }
            }));
        }
        {
            let weak = Rc::downgrade(this);
            this.forward.set_action(Rc::new(move |action| {
                if let Some(this) = weak.upgrade() {
                    this.handle_forward_action(action);
                }
            }));
        }
        {
            let weak = Rc::downgrade(this);
            this.viewer.set_action(Rc::new(move |action| {
                if let Some(this) = weak.upgrade() {
                    this.handle_viewer_action(action);
                }
            }));
        }
        {
            let weak = Rc::downgrade(this);
            this.profile.set_action(Rc::new(move |action| {
                if let Some(this) = weak.upgrade() { this.handle_profile_action(action); }
            }));
        }
        {
            let weak = Rc::downgrade(this);
            this.info.set_action(Rc::new(move |action| {
                if let Some(this) = weak.upgrade() {
                    this.handle_info_action(action);
                }
            }));
        }
        {
            let weak = Rc::downgrade(this);
            this.contacts.set_action(Rc::new(move |action| {
                if let Some(this) = weak.upgrade() {
                    this.handle_contacts_action(action);
                }
            }));
        }
        {
            let weak = Rc::downgrade(this);
            this.new_group.set_action(Rc::new(move |action| {
                if let Some(this) = weak.upgrade() {
                    this.handle_new_group_action(action);
                }
            }));
        }
        {
            let weak = Rc::downgrade(this);
            this.stickers.set_action(Rc::new(move |action| {
                if let Some(this) = weak.upgrade() {
                    this.handle_sticker_action(action);
                }
            }));
            let weak = Rc::downgrade(this);
            this.stickers.popover().connect_closed(move |_| {
                if let Some(this) = weak.upgrade() {
                    this.apply_info_layout(this.current_window_width());
                }
            });
        }
        {
            let weak = Rc::downgrade(this);
            this.poll_dialog.set_action(Rc::new(move |action| {
                if let Some(this) = weak.upgrade() {
                    this.handle_poll_dialog_action(action);
                }
            }));
        }
        {
            let weak = Rc::downgrade(this);
            this.location_dialog.set_action(Rc::new(move |action| {
                if let Some(this) = weak.upgrade() {
                    this.handle_location_dialog_action(action);
                }
            }));
        }
        {
            let weak = Rc::downgrade(this);
            this.stories_strip.set_on_peer_click(move |chat_id| {
                if let Some(this) = weak.upgrade() {
                    this.open_stories_for_peer(chat_id);
                }
            });
        }
        {
            let weak = Rc::downgrade(this);
            this.stories_viewer.set_on_closed(move || {
                if let Some(this) = weak.upgrade() {
                    this.messages.focus_composer();
                }
            });
        }
        {
            let weak = Rc::downgrade(this);
            this.switcher.widget.connect_visible_notify(move |_| {
                if let Some(this) = weak.upgrade() {
                    this.apply_info_layout(this.current_window_width());
                }
            });
        }
        {
            let weak = Rc::downgrade(this);
            dialogs_retry.connect_clicked(move |_| {
                if let Some(this) = weak.upgrade() {
                    this.load_dialogs();
                }
            });
        }
        {
            let weak = Rc::downgrade(this);
            this.settings_view.set_on_close(Rc::new(move || {
                if let Some(this) = weak.upgrade() {
                    this.close_settings();
                }
            }));
        }
        {
            let weak = Rc::downgrade(this);
            this.settings.on_change(move |settings| {
                if let Some(this) = weak.upgrade() {
                    this.apply_settings(settings);
                }
            });
        }
        if let Some(gtk_settings) = gtk::Settings::default() {
            let weak = Rc::downgrade(this);
            gtk_settings.connect_gtk_enable_animations_notify(move |_| {
                if let Some(this) = weak.upgrade() {
                    let settings = this.settings.get();
                    this.update_clock(settings.header_clock || settings.animation("liveclock"));
                    this.messages.refresh_animations();
                    this.chatlist.refresh_animations();
                }
            });
        }

        {
            let weak = Rc::downgrade(this);
            this.paned.connect_position_notify(move |paned| {
                let Some(this) = weak.upgrade() else { return };
                if this.applying_sidebar_layout.get() || this.effective_sidebar_collapsed.get() {
                    return;
                }
                let width = paned.position().max(220);
                this.ui_state.borrow_mut().sidebar_width = width;
                this.schedule_ui_save();
            });
        }
        {
            let weak = Rc::downgrade(this);
            this.content_paned.connect_position_notify(move |paned| {
                let Some(this) = weak.upgrade() else { return };
                if this.applying_info_layout.get()
                    || this.info.layout() != InfoLayout::Column
                    || paned.width() <= 0
                {
                    return;
                }
                let width = (paned.width() - paned.position()).max(280);
                this.ui_state.borrow_mut().info_width = width;
                this.schedule_ui_save();
            });
        }

        let global_action: Rc<dyn Fn(&str)> = {
            let weak = Rc::downgrade(this);
            Rc::new(move |action| {
                let Some(this) = weak.upgrade() else { return };
                if !this.session_ready.get() {
                    return;
                }
                match action {
                    "switcher" => this.open_switcher(),
                    "settings" => this.toggle_settings(),
                    "next_chat" => this.chatlist.select_next(),
                    "prev_chat" => this.chatlist.select_prev(),
                    "search" => this.focus_search(),
                    "search_in_chat" => this.open_in_chat_search(),
                    "toggle_sidebar" => this.toggle_sidebar(),
                    "reply_last" => this.reply_last(),
                    "saved" => this.clone().open_saved_messages(),
                    "chat_info" => this.toggle_info_panel(),
                    "contacts" => this.open_contacts(),
                    "jump_to_date" => this.messages.open_jump_calendar(),
                    _ => {}
                }
            })
        };
        let submit: Rc<dyn Fn()> = {
            let weak = Rc::downgrade(this);
            Rc::new(move || {
                if let Some(this) = weak.upgrade().filter(|this| this.session_ready.get()) {
                    this.submit_composer();
                }
            })
        };
        keys::install(
            this.widget.upcast_ref(),
            &this.messages.composer(),
            &this.settings,
            global_action,
            submit,
        );

        let fixed_keys = gtk::EventControllerKey::new();
        {
            let weak = Rc::downgrade(this);
            fixed_keys.connect_key_pressed(move |_, key, _, _| {
                let Some(this) = weak.upgrade() else {
                    return glib::Propagation::Proceed;
                };
                if key == gdk::Key::Escape {
                    if this.call.escape() {
                    } else if player::fullscreen_open() {
                        player::close_fullscreen();
                    } else if this.stories_viewer.is_open() {
                        this.close_stories_viewer();
                    } else if this.viewer.is_open() {
                        this.close_viewer();
                    } else if this.profile.is_open() {
                        this.close_profile();
                    } else if this.forward.is_open() {
                        this.close_forward();
                    } else if this.contacts.is_open() {
                        this.close_contacts();
                    } else if this.new_group.is_open() {
                        this.close_new_group();
                    } else if this.poll_dialog.is_open() {
                        this.close_poll_dialog();
                    } else if this.location_dialog.is_open() {
                        this.close_location_dialog();
                    } else if this.topics.dialog_is_open() {
                        this.topics.close_dialog();
                    } else if this.stickers.is_open() {
                        this.close_stickers();
                    } else if this.caption_dialog.borrow().is_some() {
                        this.close_caption_dialog();
                    } else if this.messages.video_recorder_visible() {
                        this.cancel_video_note();
                    } else if this.messages.recorder_visible() {
                        // Keyboard-first: Esc cancels an active (or failed)
                        // recording before it touches any panel.
                        this.cancel_recording();
                    } else if this.messages.scheduled_panel_open() {
                        this.messages.close_scheduled_panel();
                        this.messages.focus_composer();
                    } else if this.info.layout() == InfoLayout::Overlay {
                        this.close_info_panel();
                    } else if this.messages.search_is_open() {
                        this.close_in_chat_search();
                    } else if this.switcher.is_open() {
                        this.pending_inline_query.borrow_mut().take();
                        this.clear_window_focus();
                        this.switcher.close();
                        this.messages.focus_composer();
                    } else if this.settings_open() {
                        this.close_settings();
                    } else if !this.messages.cancel_mode() {
                        this.messages.focus_composer();
                    }
                    return glib::Propagation::Stop;
                }
                glib::Propagation::Proceed
            });
        }
        this.widget.add_controller(fixed_keys);
    }

    async fn start_backend(self: Rc<Self>) {
        match self.tg.start().await {
            Ok(state) => self.handle_auth_state(state),
            Err(error) => {
                self.stack.set_visible_child_name("auth");
                self.auth.show_start_error(&error);
            }
        }
    }

    fn handle_auth_action(self: Rc<Self>, action: AuthAction) {
        if matches!(action, AuthAction::RetryStart) {
            glib::MainContext::default().spawn_local(async move {
                self.start_backend().await;
            });
            return;
        }
        glib::MainContext::default().spawn_local(async move {
            let result = match action {
                AuthAction::SubmitPhone(value) => self.tg.submit_phone(&value).await,
                AuthAction::SubmitCode(value) => self.tg.submit_code(&value).await,
                AuthAction::SubmitPassword(value) => self.tg.submit_password(&value).await,
                AuthAction::SubmitCredentials { api_id, api_hash } => {
                    self.tg.submit_credentials(api_id, &api_hash).await
                }
                AuthAction::RetryStart => return,
            };
            match result {
                Ok(state) => self.handle_auth_state(state),
                Err(error) => {
                    self.auth.finish_error(&error);
                }
            }
        });
    }

    fn handle_auth_state(self: &Rc<Self>, state: AuthState) {
        match state {
            AuthState::NeedCredentials => {
                self.clear_window_focus();
                self.stack.set_visible_child_name("auth");
                self.auth.show_credentials();
                if self.probe {
                    self.start_auth_probe();
                }
            }
            AuthState::NeedPhone | AuthState::NeedCode | AuthState::NeedPassword => {
                self.clear_window_focus();
                self.stack.set_visible_child_name("auth");
                self.auth.show_step(state);
                if self.probe && state == AuthState::NeedPhone {
                    self.start_auth_probe();
                }
            }
            AuthState::Ready => self.on_ready(),
        }
    }

    fn on_ready(self: &Rc<Self>) {
        if self.session_ready.replace(true) {
            return;
        }
        let first_ready = !self.started.replace(true);
        self.clear_window_focus();
        self.stack.set_visible_child_name("main");
        self.install_window_hook();
        self.install_sidebar_layout();
        self.install_theme_switch_hook();
        if let Some(window) = self.window() {
            self.effects.bind(&window, &self.overlay);
            self.effects
                .window_focus(window.upcast_ref(), window.is_active());
            self.effects.launched(&self.overlay);
        }
        self.apply_settings(&self.settings.get());
        self.presence_online.set(None);
        self.update_online_status();
        if !self.event_loop_started.replace(true) {
            self.spawn_event_loop();
        }
        self.load_dialogs();
        self.load_folders();
        self.load_me();
        self.load_available_reactions();
        self.reload_stories();
        if first_ready {
            self.start_probe();
        }
    }

    fn log_out(self: Rc<Self>) {
        // This synchronous transition is the mutation barrier: no callback
        // can start another send/attach/delete before the wait loop begins.
        if !self.session_ready.replace(false) {
            return;
        }
        // Story content is account-scoped and sits above the auth stack.
        // Tear it down synchronously before any logout await can yield.
        self.stories_generation
            .set(self.stories_generation.get().wrapping_add(1));
        self.stories_viewer.clear_session();
        self.stories_peers.borrow_mut().clear();
        self.stories_strip.update_peers(Vec::new());
        self.call.clear();
        self.withdraw_call_notification();
        crate::status::set_call(None);
        // Account teardown is a synchronous playback barrier: no audio may
        // survive on the auth screen, and no recorder controls remain mapped
        // while their serialized local cleanup finishes.
        self.messages.reset_players();
        self.messages.hide_video_note();
        self.topics.close_dialog();
        self.cancel_recording();
        self.close_forward();
        self.close_viewer();
        self.close_profile();
        self.close_contacts();
        self.close_new_group();
        self.close_poll_dialog();
        self.close_location_dialog();
        self.close_stickers();
        self.close_caption_dialog();
        if self.messages.search_is_open() {
            self.close_in_chat_search();
        }
        self.pending_notifications.borrow_mut().clear();
        self.probe_notification_avatar.borrow_mut().take();
        self.settings_view.begin_logout();
        self.switcher.close();
        self.main_menu.dismiss();
        // Whatever entry holds keyboard focus (search bars, composer) is
        // about to be unmapped with the main view; drop focus first so GTK
        // delivers its focus-out instead of warning at teardown.
        if let Some(window) = self.window() {
            gtk::prelude::GtkWindowExt::set_focus(&window, None::<&gtk::Widget>);
        }
        self.messages.set_busy(true);
        self.widget.set_sensitive(false);
        for (_, source) in self.draft_timeouts.borrow_mut().drain() {
            source.remove();
        }
        glib::MainContext::default().spawn_local(async move {
            self.local.record_cancel().await;
            while self.mutations_in_flight.get() != 0
                || self.flags_in_flight.get()
                || self.drafts.borrow().values().any(|state| state.in_flight)
                || self
                    .mark_reads
                    .borrow()
                    .values()
                    .any(|state| state.in_flight)
                || self.video_recorder.borrow().phase != VideoRecorderPhase::Idle
            {
                glib::timeout_future(Duration::from_millis(25)).await;
            }
            self.force_release_composer();
            self.messages.stop_send_feedback();
            let next_session = self.session_epoch.get().wrapping_add(1);
            self.session_epoch.set(next_session);
            let epoch = self.bump_epoch();
            let result = self.tg.log_out().await;
            self.messages.set_busy(false);
            self.widget.set_sensitive(true);
            if result.is_ok() {
                // Active media identities are account-scoped. Retire rather
                // than finalize them: GTK 4.22 can deadlock when its media
                // backend loses the final reference.
                self.messages.retire_player_media_session();
                self.me.borrow_mut().take();
                self.chat_info.borrow_mut().clear();
                self.drafts.borrow_mut().clear();
                // Only a logout that succeeded zeroes the bar badge; a failed
                // one keeps the signed-in snapshot.
                crate::status::reset_session();
            }
            match result {
                Ok(AuthState::NeedPhone) => {
                    self.open_chat.set(None);
                    self.info.unbind();
                    self.apply_info_layout(self.current_window_width());
                    self.dialogs_loaded.set(false);
                    self.messages.clear_selection(epoch);
                    self.clear_window_focus();
                    self.stack.set_visible_child_name("auth");
                    self.auth.show_step(AuthState::NeedPhone);
                }
                Ok(state) => self.handle_auth_state(state),
                Err(error) => {
                    self.session_ready.set(true);
                    self.messages.restore_players_after_logout_failure();
                    self.apply_settings(&self.settings.get());
                    self.reload_stories();
                    self.settings_view.account_error(&error);
                    self.messages.show_error(&error);
                }
            }
        });
    }

    /// Push settings into the UI and the backend (clock, ghost pill, message
    /// time format, backend flags). Called on READY and on every change.
    fn apply_settings(self: &Rc<Self>, settings: &Settings) {
        let transcribe_auto_just_enabled = {
            let mut previous = self.last_applied_settings.borrow_mut();
            let just_enabled = !previous.ai.transcribe_auto && settings.ai.transcribe_auto;
            *previous = settings.clone();
            just_enabled
        };
        let generation = self.settings_gen.get().wrapping_add(1);
        self.settings_gen.set(generation);
        self.messages.set_time_format(settings.time_format());
        self.messages.set_map_tiles(settings.media.map_tiles);
        self.messages.set_ghost(settings.ghost_mode);
        self.messages.set_edit_history(settings.edit_history);
        self.messages.set_ai_enabled(settings.ai.enabled);
        self.call.set_ringtone_enabled(settings.calls.ringtone);
        self.chatlist.set_show_avatars(settings.ui.show_avatars);
        self.chatlist.set_compact(settings.ui.compact_list);
        crate::theme::set_text_scale(settings.ui.text_scale);
        // A32: the info panel's own avatar, its member rows and the contacts
        // list are bound at their own logical keys, so a show_avatars flip
        // has to rebind them — placeholders first, then any new download.
        let avatars_changed = self.info.set_show_avatars(settings.ui.show_avatars);
        if self.contacts.set_show_avatars(settings.ui.show_avatars) && self.contacts.is_open() {
            let generation = self.contacts.begin();
            self.load_contacts(generation);
        }
        if avatars_changed {
            self.bind_info_panel();
        }
        self.update_clock(settings.header_clock || settings.animation("liveclock"));
        // Wave 6D: false → animated stickers freeze on their first frame.
        lottie::set_animated_stickers_enabled(settings.media.animated_stickers);
        self.effects.sync();
        self.messages.refresh_animations();
        self.chatlist.refresh_animations();
        self.refresh_virtual_rows(settings);
        let disabled_open = match self.open_chat.get() {
            Some(ASSISTANT_CHAT) => !settings.ai.enabled,
            Some(OMARCHY_CHAT) => !settings.os.enabled,
            _ => false,
        };
        if disabled_open {
            self.close_forward();
            self.close_viewer();
            self.close_profile();
            if self.messages.search_is_open() {
                self.close_in_chat_search();
            }
            let epoch = self.bump_epoch();
            self.open_chat.set(None);
            self.messages.clear_selection(epoch);
        } else if settings.ai.enabled && transcribe_auto_just_enabled {
            self.arm_visible_transcriptions();
        }
        let flags = BackendFlags {
            ghost_mode: settings.ghost_mode,
            anti_delete: settings.anti_delete,
            markdown_send: settings.ui.markdown_send,
        };
        let previous = self.desired_flags.replace(flags);
        let initialized = self.flags_initialized.replace(true);
        if initialized && previous.anti_delete != flags.anti_delete {
            self.anti_reload_pending.set(true);
            if !flags.anti_delete {
                self.tombstones.borrow_mut().clear();
            }
        }
        // Forward on every store change. Besides keeping the backend snapshot
        // explicit, this guarantees a current-generation completion exists if
        // an unrelated setting changes while an anti-delete flip is in flight.
        if self.session_ready.get() {
            self.push_flags(FlagsRequest {
                flags,
                generation,
                session_epoch: self.session_epoch.get(),
            });
        }
    }

    fn refresh_virtual_rows(&self, settings: &Settings) {
        let stores = self.virtual_stores.borrow();
        let mut rows = Vec::new();
        if settings.ai.enabled {
            rows.push((
                ASSISTANT_CHAT,
                "Assistant".to_string(),
                stores
                    .get(&ASSISTANT_CHAT)
                    .and_then(|store| store.msgs.last())
                    .map(|message| last_line(&message.text))
                    .unwrap_or_default(),
            ));
        }
        if settings.os.enabled {
            rows.push((
                OMARCHY_CHAT,
                "Omarchy".to_string(),
                stores
                    .get(&OMARCHY_CHAT)
                    .and_then(|store| store.msgs.last())
                    .map(|message| last_line(&message.text))
                    .unwrap_or_default(),
            ));
        }
        drop(stores);
        self.chatlist.set_virtual(rows);
    }

    /// Coalesced set_flags: flags reach the backend before any anti-delete
    /// reload. Generation checks discard stale completions, while serialization
    /// prevents an older backend call from completing after a newer one (D1).
    fn push_flags(self: &Rc<Self>, request: FlagsRequest) {
        if self.flags_in_flight.get() {
            *self.flags_pending.borrow_mut() = Some(request);
            return;
        }
        self.flags_in_flight.set(true);
        let tg = self.tg.clone();
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let result = tg.set_flags(request.flags).await;
            let Some(this) = weak.upgrade() else { return };
            this.flags_in_flight.set(false);
            match result {
                Ok(())
                    if this.is_session_current(request.session_epoch)
                        && request.generation == this.settings_gen.get() =>
                {
                    if this.anti_reload_pending.replace(false)
                        && let Some(chat_id) = this.open_chat.get() {
                            this.clone().force_reload(chat_id);
                        }
                }
                Ok(()) => {
                    // A newer settings snapshot is queued (or already sent),
                    // so this completion must not reload data.
                }
                Err(error) if this.is_session_current(request.session_epoch) => {
                    shell_log!("set_flags: {error}");
                }
                Err(_) => {}
            }
            let pending = this.flags_pending.borrow_mut().take();
            if let Some(request) =
                pending.filter(|request| this.is_session_current(request.session_epoch))
            {
                this.push_flags(request);
            }
        });
    }

    fn update_clock(self: &Rc<Self>, on: bool) {
        if let Some(source) = self.clock_source.borrow_mut().take() {
            source.remove();
        }
        if !on {
            self.messages.set_clock(None);
            return;
        }
        self.messages.set_clock(Some(&clock_text()));
        let weak = Rc::downgrade(self);
        let source = glib::timeout_add_local(Duration::from_secs(1), move || {
            let Some(this) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            this.messages.set_clock(Some(&clock_text()));
            glib::ControlFlow::Continue
        });
        *self.clock_source.borrow_mut() = Some(source);
    }

    fn settings_open(&self) -> bool {
        self.stack.visible_child_name().as_deref() == Some("settings")
    }

    fn toggle_settings(&self) {
        if self.settings_open() {
            self.close_settings();
        } else {
            self.close_forward();
            self.close_viewer();
            self.close_profile();
            self.close_contacts();
            self.close_new_group();
            self.close_poll_dialog();
            self.close_location_dialog();
            self.close_stickers();
            self.close_caption_dialog();
            self.close_in_chat_search();
            self.clear_window_focus();
            self.switcher.close();
            self.stack.set_visible_child_name("settings");
            self.apply_info_layout(self.current_window_width());
        }
    }

    fn open_switcher(&self) {
        self.close_forward();
        self.close_viewer();
        self.close_profile();
        self.close_contacts();
        self.close_new_group();
        self.close_poll_dialog();
        self.close_location_dialog();
        self.close_stickers();
        self.close_caption_dialog();
        self.close_in_chat_search();
        self.close_settings();
        self.switcher.open(self.chatlist.ordered().into_iter().map(|(id, title)| {
            let identity = self.chatlist.summary(id).map(|chat| chat.identity()).unwrap_or_default();
            (id, if identity.is_empty() { title } else { format!("{title}\n{identity}") })
        }).collect());
    }

    fn close_settings(&self) {
        if self.settings_open() {
            self.clear_window_focus();
            self.settings_view.dismiss_transients();
            self.stack.set_visible_child_name("main");
            self.apply_info_layout(self.current_window_width());
            self.messages.focus_composer();
        } else {
            self.settings_view.dismiss_transients();
        }
    }

    fn install_sidebar_layout(self: &Rc<Self>) {
        if self.layout_tick.borrow().is_some() {
            return;
        }
        let weak = Rc::downgrade(self);
        let tick = self.paned.add_tick_callback(move |paned, _| {
            let Some(this) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            let width = paned
                .root()
                .and_downcast::<gtk::ApplicationWindow>()
                .map(|window| window.width())
                .unwrap_or_else(|| paned.width());
            let width = this.probe_window_width.get().unwrap_or(width);
            this.in_layout_tick.set(true);
            this.apply_sidebar_layout(width);
            this.in_layout_tick.set(false);
            glib::ControlFlow::Continue
        });
        *self.layout_tick.borrow_mut() = Some(tick);
    }

    fn apply_sidebar_layout(&self, window_width: i32) {
        self.apply_info_layout(window_width);
        let state = self.ui_state.borrow();
        let collapsed = state.sidebar_collapsed || window_width < 800;
        let position = if collapsed {
            64
        } else {
            clamp_sidebar_width(state.sidebar_width, window_width)
        };
        drop(state);
        if self.effective_sidebar_collapsed.get() == collapsed && self.paned.position() == position
        {
            return;
        }
        self.applying_sidebar_layout.set(true);
        self.effective_sidebar_collapsed.set(collapsed);
        self.paned.set_shrink_start_child(collapsed);
        self.chatlist.set_collapsed(collapsed);
        self.stories_strip.set_compact(collapsed);
        self.paned.set_position(position);
        self.applying_sidebar_layout.set(false);
    }

    fn apply_info_layout(&self, window_width: i32) {
        let state = self.ui_state.borrow();
        let desired = state.info_panel_open
            && self.open_chat.get().is_some_and(|chat_id| !is_virtual(chat_id));
        let info_width = state.info_width.max(280);
        let sidebar = if state.sidebar_collapsed || window_width < 800 { 64 } else { clamp_sidebar_width(state.sidebar_width, window_width) };
        drop(state);
        let overlay_blocked = self.viewer.is_open()
            || self.profile.is_open()
            || player::fullscreen_open()
            || self.forward.is_open()
            || self.contacts.is_open()
            || self.new_group.is_open()
            || self.poll_dialog.is_open()
            || self.location_dialog.is_open()
            || self.stickers.is_open()
            || self.caption_dialog.borrow().is_some()
            || self.switcher.is_open()
            || self.settings_open()
            || self.call.is_open();
        let target = if !desired {
            InfoLayout::Hidden
        } else if window_width - sidebar - info_width >= 560 {
            InfoLayout::Column
        } else if overlay_blocked {
            InfoLayout::Hidden
        } else {
            InfoLayout::Overlay
        };
        let info_widget = self.info.widget.clone().upcast::<gtk::Widget>();
        let column_attached = self
            .content_paned
            .end_child()
            .is_some_and(|child| child == info_widget);
        if target == self.info.layout() && (target != InfoLayout::Column || column_attached) {
            if target == InfoLayout::Column {
                let width = self.content_paned.width();
                if width > 0 {
                    self.applying_info_layout.set(true);
                    self.content_paned
                        .set_position((width - info_width).max(1));
                    self.applying_info_layout.set(false);
                }
            }
            return;
        }

        // A27: from here on the panel is reparented (Paned end child ⇄
        // Overlay child). The frame-clock tick drives this method on every
        // frame, and unparenting a widget from inside the frame cycle is not
        // safe — hand the transition to the next idle instead. The idle
        // recomputes the target from scratch, so a single pending one is
        // always enough.
        if self.in_layout_tick.get() {
            self.schedule_info_layout();
            return;
        }
        if let Some(source) = self.info_layout_idle.borrow_mut().take() {
            source.remove();
        }

        if let Some(root) = self.info.widget.root()
            && let Some(focus) = root.focus()
                && (focus == self.info.widget.clone().upcast::<gtk::Widget>()
                    || focus.is_ancestor(&self.info.widget))
                {
                    root.set_focus(None::<&gtk::Widget>);
                }
        self.applying_info_layout.set(true);
        if column_attached {
            self.content_paned.set_end_child(None::<&gtk::Widget>);
        }
        if self
            .info
            .widget
            .parent()
            .is_some_and(|parent| parent == self.overlay.clone().upcast::<gtk::Widget>())
        {
            self.overlay.remove_overlay(&self.info.widget);
        }

        match target {
            InfoLayout::Hidden => {
                self.info.set_layout(InfoLayout::Hidden);
            }
            InfoLayout::Column => {
                self.info.widget.set_halign(gtk::Align::Fill);
                self.info.widget.set_valign(gtk::Align::Fill);
                self.info.widget.set_size_request(280, -1);
                self.content_paned.set_end_child(Some(&self.info.widget));
                let width = self.content_paned.width();
                if width > 0 {
                    self.content_paned
                        .set_position((width - info_width).max(1));
                }
                self.info.set_layout(InfoLayout::Column);
            }
            InfoLayout::Overlay => {
                self.info.widget.set_halign(gtk::Align::End);
                self.info.widget.set_valign(gtk::Align::Fill);
                self.info.widget.set_size_request(info_width, -1);
                self.overlay.add_overlay(&self.info.widget);
                self.info.set_layout(InfoLayout::Overlay);
            }
        }
        self.applying_info_layout.set(false);
    }

    /// Queue the A27 info-panel reparent for the next main-loop idle, outside
    /// the frame cycle. At most one is pending: the idle re-derives the
    /// target, so a newer request needs no extra source.
    fn schedule_info_layout(&self) {
        if self.info_layout_idle.borrow().is_some() {
            return;
        }
        let weak = self.weak_self.borrow().clone();
        let source = glib::idle_add_local_once(move || {
            let Some(this) = weak.upgrade() else { return };
            this.info_layout_idle.borrow_mut().take();
            this.apply_info_layout(this.current_window_width());
        });
        *self.info_layout_idle.borrow_mut() = Some(source);
    }

    async fn resize_window_for_probe(
        &self,
        window: &gtk::ApplicationWindow,
        width: i32,
    ) -> ProbeResize {
        self.probe_window_width.set(None);
        self.messages.set_probe_pane_width(None);
        window.set_default_size(width, window.height().max(1));
        if poll_until(600, || window.width() == width).await {
            // One frame observes the new pane allocation; the next applies the
            // queued BubbleClamp resize produced by that allocation.
            wait_for_frame(window.upcast_ref()).await;
            wait_for_frame(window.upcast_ref()).await;
            if window.width() == width {
                eprintln!("[probe] resize: real");
                return ProbeResize {
                    pane_width: self.messages.bubble_metrics().1,
                };
            }
        }

        self.probe_window_width.set(Some(width));
        self.apply_sidebar_layout(width);
        let pane_width = (width - self.paned.position()).max(1);
        self.messages.set_probe_pane_width(Some(pane_width));
        eprintln!("[probe] resize: injected");
        wait_for_frame(window.upcast_ref()).await;
        wait_for_frame(window.upcast_ref()).await;
        ProbeResize { pane_width }
    }

    fn toggle_sidebar(self: &Rc<Self>) {
        let collapsed = !self.ui_state.borrow().sidebar_collapsed;
        self.ui_state.borrow_mut().sidebar_collapsed = collapsed;
        let width = self.window().map(|window| window.width()).unwrap_or(1100);
        self.apply_sidebar_layout(width);
        self.schedule_ui_save();
    }

    fn focus_search(&self) {
        self.chatlist.focus_search();
    }

    fn open_contacts(self: &Rc<Self>) {
        self.close_forward();
        self.close_viewer();
        self.close_profile();
        self.close_new_group();
        self.close_stickers();
        self.close_caption_dialog();
        self.switcher.close();
        self.close_settings();
        let generation = self.contacts.begin();
        self.apply_info_layout(self.current_window_width());
        self.load_contacts(generation);
    }

    fn load_contacts(self: &Rc<Self>, generation: u64) {
        let tg = self.tg.clone();
        let session_epoch = self.session_epoch.get();
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let result = tg.get_contacts().await;
            let Some(this) = weak.upgrade().filter(|this| {
                this.is_session_current(session_epoch)
                    && this.contacts.generation() == generation
                    && this.contacts.is_open()
            }) else {
                return;
            };
            match result {
                Ok(contacts) => {
                    this.contacts.finish(generation, contacts);
                }
                Err(error) => {
                    this.contacts.fail(generation, &error);
                }
            }
        });
    }

    fn handle_contacts_action(self: Rc<Self>, action: ContactsAction) {
        match action {
            ContactsAction::Close => self.close_contacts(),
            ContactsAction::Retry => {
                let generation = self.contacts.begin();
                self.load_contacts(generation);
            }
            ContactsAction::Open(user_id) => {
                let session_epoch = self.session_epoch.get();
                // A slow `open_user` must not steal navigation: Esc, a chat
                // switch or New group all bump the contacts generation.
                let generation = self.contacts.generation();
                glib::MainContext::default().spawn_local(async move {
                    let result = self.tg.open_user(user_id).await;
                    if !self.is_session_current(session_epoch)
                        || !self.contacts.is_open()
                        || self.contacts.generation() != generation
                    {
                        return;
                    }
                    match result {
                        Ok(summary) => {
                            let chat_id = summary.id;
                            self.bump_dialogs_revision();
                            self.chatlist.set_summary(summary);
                            self.close_contacts();
                            self.open_chat(chat_id);
                        }
                        Err(error) => {
                            self.contacts.show_action_error(&error);
                        }
                    }
                });
            }
        }
    }

    fn close_contacts(&self) {
        if self.contacts.is_open() {
            self.contacts.close();
            self.messages.focus_composer();
            self.apply_info_layout(self.current_window_width());
        }
    }

    fn open_new_group(self: &Rc<Self>) {
        self.close_forward();
        self.close_viewer();
        self.close_profile();
        self.close_contacts();
        self.close_stickers();
        self.close_caption_dialog();
        self.switcher.close();
        self.close_settings();
        let generation = self.new_group.begin();
        self.apply_info_layout(self.current_window_width());
        self.load_new_group_contacts(generation);
    }

    fn load_new_group_contacts(self: &Rc<Self>, generation: u64) {
        let tg = self.tg.clone();
        let session_epoch = self.session_epoch.get();
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let result = tg.get_contacts().await;
            let Some(this) = weak.upgrade().filter(|this| {
                this.is_session_current(session_epoch)
                    && this.new_group.generation() == generation
                    && this.new_group.is_open()
            }) else {
                return;
            };
            match result {
                Ok(contacts) => {
                    this.new_group.finish_contacts(generation, contacts);
                }
                Err(error) => {
                    this.new_group.fail_contacts(generation, &error);
                }
            }
        });
    }

    fn handle_new_group_action(self: Rc<Self>, action: NewGroupAction) {
        match action {
            NewGroupAction::Close => self.close_new_group(),
            NewGroupAction::RetryContacts => {
                // A33: the section returns to its loading state before the
                // reload starts, without wiping the title or the selection.
                self.new_group.begin_retry();
                let generation = self.new_group.generation();
                self.load_new_group_contacts(generation);
            }
            NewGroupAction::Create { title, user_ids } => {
                let generation = self.new_group.generation();
                let Some(session_epoch) = self.begin_mutation() else {
                    return;
                };
                self.new_group.set_busy(true);
                glib::MainContext::default().spawn_local(async move {
                    let result = self.tg.create_group(&title, user_ids).await;
                    self.finish_mutation();
                    if !self.is_session_current(session_epoch)
                        || !self.new_group.is_open()
                        || self.new_group.generation() != generation
                    {
                        return;
                    }
                    match result {
                        Ok(summary) => {
                            let chat_id = summary.id;
                            self.bump_dialogs_revision();
                            self.chatlist.set_summary(summary);
                            self.close_new_group();
                            self.open_chat(chat_id);
                        }
                        Err(error) => self.new_group.show_create_error(&error),
                    }
                });
            }
        }
    }

    fn close_new_group(&self) {
        if self.new_group.is_open() {
            self.new_group.close();
            self.messages.focus_composer();
            self.apply_info_layout(self.current_window_width());
        }
    }

    fn open_saved_messages(self: Rc<Self>) {
        let saved = self.chatlist.ordered().into_iter().find_map(|(id, _)| {
            self.chatlist
                .summary(id)
                .filter(|chat| chat.kind == ChatKind::Saved)
                .map(|_| id)
        });
        if let Some(chat_id) = saved {
            self.open_chat(chat_id);
            return;
        }
        let session_epoch = self.session_epoch.get();
        // Same staleness rule as ContactsAction::Open: any navigation while
        // the lookup is in flight bumps the epoch and wins over it.
        let epoch = self.epoch.get();
        glib::MainContext::default().spawn_local(async move {
            // Hoist the clone: a `match` scrutinee temporary would keep the
            // RefCell borrow alive across the await below.
            let cached = self.me.borrow().clone();
            let me = match cached {
                Some(me) => Ok(me),
                None => self.tg.get_me().await,
            };
            let result = match me {
                Ok(me) => self.tg.open_user(me.id).await,
                Err(error) => Err(error),
            };
            if !self.is_session_current(session_epoch) || self.epoch.get() != epoch {
                return;
            }
            match result {
                Ok(summary) => {
                    let chat_id = summary.id;
                    self.bump_dialogs_revision();
                    self.chatlist.set_summary(summary);
                    self.open_chat(chat_id);
                }
                Err(error) => {
                    self.messages.show_error(&error);
                }
            }
        });
    }

    fn current_window_width(&self) -> i32 {
        self.probe_window_width
            .get()
            .or_else(|| self.window().map(|window| window.width()))
            .unwrap_or(1100)
    }

    fn schedule_ui_save(self: &Rc<Self>) {
        if let Some(source) = self.ui_save_timeout.borrow_mut().take() {
            source.remove();
        }
        let weak = Rc::downgrade(self);
        let source = glib::timeout_add_local_once(Duration::from_secs(1), move || {
            let Some(this) = weak.upgrade() else { return };
            this.ui_save_timeout.borrow_mut().take();
            let state = this.ui_state.borrow().clone();
            if let Err(error) = state.save() {
                eprintln!("ui-state: {error}");
            }
        });
        *self.ui_save_timeout.borrow_mut() = Some(source);
    }

    fn load_folders(self: &Rc<Self>) {
        let tg = self.tg.clone();
        let session_epoch = self.session_epoch.get();
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let result = tg.get_folders().await;
            let Some(this) = weak.upgrade().filter(|this| {
                this.session_ready.get() && this.session_epoch.get() == session_epoch
            }) else {
                return;
            };
            match result {
                Ok(folders) => {
                    let folder_id = this.ui_state.borrow().folder_id;
                    this.chatlist.set_folders(folders, folder_id);
                }
                Err(error) => eprintln!("get_folders: {error}"),
            }
        });
    }

    fn load_me(self: &Rc<Self>) {
        if self.me_loading.replace(true) {
            return;
        }
        let tg = self.tg.clone();
        let session_epoch = self.session_epoch.get();
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let result = tg.get_me().await;
            let Some(this) = weak.upgrade() else { return };
            this.me_loading.set(false);
            if !this.session_ready.get() || this.session_epoch.get() != session_epoch {
                return;
            }
            match result {
                Ok(me) => *this.me.borrow_mut() = Some(me),
                Err(error) => eprintln!("get_me: {error}"),
            }
        });
    }

    fn load_available_reactions(self: &Rc<Self>) {
        let generation = self.available_reactions_generation.get().wrapping_add(1);
        self.available_reactions_generation.set(generation);
        self.messages.begin_available_reactions();
        let session_epoch = self.session_epoch.get();
        let tg = self.tg.clone();
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let result = tg.get_available_reactions().await;
            let Some(this) = weak.upgrade().filter(|this| {
                this.is_session_current(session_epoch)
                    && this.available_reactions_generation.get() == generation
            }) else {
                return;
            };
            match result {
                Ok(reactions) => this.messages.set_available_reactions(reactions),
                Err(error) => {
                    shell_log!("get_available_reactions: {error}");
                    this.messages.fail_available_reactions(error);
                }
            }
        });
    }

    fn sidebar_search_changed(self: Rc<Self>, query: String, generation: u64) {
        if let Some(source) = self.search_timeout.borrow_mut().take() {
            source.remove();
        }
        self.chatlist.refresh_search();
        if query.chars().count() < 3 {
            return;
        }
        let weak = Rc::downgrade(&self);
        let source = glib::timeout_add_local_once(Duration::from_millis(300), move || {
            let Some(this) = weak.upgrade() else { return };
            this.search_timeout.borrow_mut().take();
            this.run_sidebar_search(query, generation);
        });
        *self.search_timeout.borrow_mut() = Some(source);
    }

    fn run_sidebar_search(self: &Rc<Self>, query: String, generation: u64) {
        let session_epoch = self.session_epoch.get();
        let tg = self.tg.clone();
        let weak = Rc::downgrade(self);
        let chats_query = query.clone();
        glib::MainContext::default().spawn_local(async move {
            let result = tg.search_chats(&chats_query).await;
            if let Some(this) = weak.upgrade().filter(|this| {
                this.session_ready.get() && this.session_epoch.get() == session_epoch
            }) {
                this.chatlist
                    .finish_chat_search(&chats_query, generation, result);
            }
        });
        let tg = self.tg.clone();
        let weak = Rc::downgrade(self);
        let session_epoch = self.session_epoch.get();
        glib::MainContext::default().spawn_local(async move {
            let result = tg.search_global(&query).await;
            if let Some(this) = weak.upgrade().filter(|this| {
                this.session_ready.get() && this.session_epoch.get() == session_epoch
            }) {
                this.chatlist
                    .finish_message_search(&query, generation, result);
            }
        });
    }

    fn retry_sidebar_search(self: Rc<Self>, kind: SearchRetry, query: String, generation: u64) {
        let tg = self.tg.clone();
        let session_epoch = self.session_epoch.get();
        let weak = Rc::downgrade(&self);
        glib::MainContext::default().spawn_local(async move {
            match kind {
                SearchRetry::Chats => {
                    let result = tg.search_chats(&query).await;
                    let Some(this) = weak.upgrade().filter(|this| {
                        this.session_ready.get() && this.session_epoch.get() == session_epoch
                    }) else {
                        return;
                    };
                    this.chatlist.finish_chat_search(&query, generation, result);
                }
                SearchRetry::Messages => {
                    let result = tg.search_global(&query).await;
                    let Some(this) = weak.upgrade().filter(|this| {
                        this.session_ready.get() && this.session_epoch.get() == session_epoch
                    }) else {
                        return;
                    };
                    this.chatlist
                        .finish_message_search(&query, generation, result);
                }
            }
        });
    }

    fn open_search_result(self: Rc<Self>, chat_id: i64, msg_id: Option<i32>) {
        if self.chatlist.summary(chat_id).is_none()
            && let Some(summary) = self.chatlist.search_summary(chat_id) {
                self.bump_dialogs_revision();
                self.chatlist.set_summary(summary);
            }
        self.chatlist.set_search_text("");
        self.clone().open_chat(chat_id);
        let Some(msg_id) = msg_id else { return };
        let weak = Rc::downgrade(&self);
        glib::MainContext::default().spawn_local(async move {
            if !poll_until(3000, || {
                weak.upgrade().is_some_and(|this| {
                    this.open_chat.get() == Some(chat_id) && !this.messages.is_loading()
                })
            })
            .await
            {
                return;
            }
            if let Some(this) = weak.upgrade()
                && !this.messages.scroll_to_message(msg_id) {
                    this.jump_to_message(msg_id);
                }
        });
    }

    fn open_main_menu(self: Rc<Self>) {
        self.open_main_menu_with_autohide(true);
    }

    fn open_main_menu_with_autohide(self: Rc<Self>, autohide: bool) {
        let (popover, contents) = menus::popover();
        popover.set_autohide(autohide);
        let header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        header.add_css_class("omg-menu-header");
        if let Some(me) = self.me.borrow().clone() {
            let avatar = Avatar::new(36);
            avatar.bind(&self.tg, me.id, &me.name, me.has_photo);
            header.append(&avatar.widget);
            let text = gtk::Box::new(gtk::Orientation::Vertical, 2);
            let name = gtk::Label::new(Some(&me.name));
            name.add_css_class("omg-chat-title");
            name.set_halign(gtk::Align::Start);
            text.append(&name);
            let phone = gtk::Label::new(Some(&me.phone));
            phone.add_css_class("omg-muted");
            phone.set_halign(gtk::Align::Start);
            text.append(&phone);
            header.append(&text);
        } else {
            header.append(&gtk::Label::new(Some("Loading account…")));
            self.load_me();
        }
        contents.append(&header);
        for (label, action, danger) in [
            ("Saved messages", MainMenuAction::Saved, false),
            ("Contacts", MainMenuAction::Contacts, false),
            ("New group", MainMenuAction::NewGroup, false),
            ("Archived chats", MainMenuAction::Archived, false),
            ("Settings", MainMenuAction::Settings, false),
            ("Keyboard shortcuts", MainMenuAction::Shortcuts, false),
            ("About", MainMenuAction::About, false),
            ("Hide window", MainMenuAction::Hide, false),
            ("Quit Omarchygram · Ctrl+Q", MainMenuAction::Quit, false),
            ("Log out", MainMenuAction::LogOut, true),
        ] {
            let button = menus::button(label, danger);
            let weak = Rc::downgrade(&self);
            let popover_weak = popover.downgrade();
            button.connect_clicked(move |_| {
                if let Some(popover) = popover_weak.upgrade() {
                    popover.popdown();
                }
                if let Some(this) = weak.upgrade() {
                    this.handle_main_menu(action);
                }
            });
            contents.append(&button);
        }
        let button = self.chatlist.main_menu_button();
        self.main_menu.show(&button, popover);
    }

    fn handle_main_menu(self: Rc<Self>, action: MainMenuAction) {
        match action {
            MainMenuAction::Hide => {
                if let Some(window) = self.window() { window.set_visible(false); }
            }
            MainMenuAction::Quit => {
                if let Some(app) = self.window().and_then(|w| w.application()) { app.quit(); }
            }
            MainMenuAction::Saved => self.open_saved_messages(),
            MainMenuAction::Contacts => self.open_contacts(),
            MainMenuAction::NewGroup => self.open_new_group(),
            MainMenuAction::Archived => self.chatlist.show_archived(),
            MainMenuAction::Settings | MainMenuAction::Shortcuts => self.toggle_settings(),
            MainMenuAction::About => {
                if let Some(window) = self.window() {
                    gtk::AlertDialog::builder()
                        .message("Omarchygram")
                        .detail("A Telegram client for Omarchy")
                        .buttons(["Close"])
                        .build()
                        .show(Some(&window));
                }
            }
            MainMenuAction::LogOut => self.confirm_log_out(),
        }
    }

    fn confirm_log_out(self: Rc<Self>) {
        let Some(window) = self.window() else { return };
        let dialog = gtk::AlertDialog::builder()
            .message("Log out of Omarchygram?")
            .buttons(["Cancel", "Log out"])
            .default_button(0)
            .cancel_button(0)
            .build();
        glib::MainContext::default().spawn_local(async move {
            let answer = dialog.choose_future(Some(&window)).await.ok();
            if answer != Some(1) {
                return;
            }
            self.log_out();
        });
    }

    fn handle_chat_action(self: Rc<Self>, chat_id: i64, action: ChatAction) {
        if !self.session_ready.get() {
            return;
        }
        if matches!(action, ChatAction::Open) {
            self.open_chat(chat_id);
            return;
        }
        if matches!(action, ChatAction::Search) {
            if self.open_chat.get() == Some(chat_id) {
                self.open_in_chat_search();
            }
            return;
        }
        if matches!(action, ChatAction::Info) {
            self.toggle_info_panel();
            return;
        }
        if matches!(action, ChatAction::Call) {
            if self.open_chat.get() == Some(chat_id)
                && self
                    .chatlist
                    .summary(chat_id)
                    .is_some_and(|summary| summary.kind == ChatKind::User)
            {
                self.start_call(chat_id);
            }
            return;
        }
        if matches!(action, ChatAction::JumpToDate) {
            return;
        }
        if matches!(action, ChatAction::ClearHistory | ChatAction::Delete) {
            self.confirm_destructive_chat_action(chat_id, action);
            return;
        }
        let tg = self.tg.clone();
        let session_epoch = self.session_epoch.get();
        let weak = Rc::downgrade(&self);
        if let ChatAction::Mute(mode) = action {
            let muted = !matches!(mode, crate::tg::MuteMode::Unmute);
            self.pending_mutes.borrow_mut().insert(chat_id, muted);
            if self.info.chat_id() == Some(chat_id) {
                self.info.set_notifications(!muted);
            }
        }
        if let ChatAction::MarkUnread(unread) = action {
            if unread {
                self.manual_unread_hold.borrow_mut().insert(chat_id);
            } else {
                self.manual_unread_hold.borrow_mut().remove(&chat_id);
            }
        }
        self.begin_chat_mutation();
        glib::MainContext::default().spawn_local(async move {
            let result = match action {
                ChatAction::MarkUnread(true) => tg.mark_unread(chat_id, true).await,
                ChatAction::MarkUnread(false) => {
                    let up_to = weak
                        .upgrade()
                        .and_then(|this| this.chatlist.summary(chat_id))
                        .map(|chat| chat.last_msg_id)
                        .unwrap_or(0);
                    tg.mark_read(chat_id, up_to).await
                }
                ChatAction::Pin(pinned) => tg.set_pinned(chat_id, pinned).await,
                ChatAction::Mute(mode) => tg.set_muted(chat_id, mode).await,
                ChatAction::Archive(archived) => tg.set_archived(chat_id, archived).await,
                _ => Ok(()),
            };
            let Some(this) = weak.upgrade() else { return };
            this.finish_chat_mutation();
            if !this.session_ready.get() || this.session_epoch.get() != session_epoch {
                return;
            }
            match result {
                Ok(()) if matches!(action, ChatAction::MarkUnread(true)) => {
                    this.bump_dialogs_revision();
                    this.chatlist.set_unread_mark(chat_id, true);
                }
                Ok(()) if matches!(action, ChatAction::MarkUnread(false)) => {
                    this.bump_dialogs_revision();
                    this.chatlist.clear_unread(chat_id);
                }
                Ok(()) => {}
                Err(error) => {
                    if let ChatAction::Mute(mode) = action {
                        let failed_intent = !matches!(mode, MuteMode::Unmute);
                        let still_current = this
                            .pending_mutes
                            .borrow()
                            .get(&chat_id)
                            .copied()
                            == Some(failed_intent);
                        if still_current {
                            this.pending_mutes.borrow_mut().remove(&chat_id);
                        }
                        let muted = this
                            .pending_mutes
                            .borrow()
                            .get(&chat_id)
                            .copied()
                            .or_else(|| this.chatlist.summary(chat_id).map(|summary| summary.muted))
                            .unwrap_or(false);
                        if this.info.chat_id() == Some(chat_id) {
                            this.info.set_notifications(!muted);
                        }
                    }
                    if matches!(action, ChatAction::MarkUnread(true)) {
                        this.manual_unread_hold.borrow_mut().remove(&chat_id);
                    }
                    this.messages.show_error(&error);
                }
            }
        });
    }

    fn start_call(self: Rc<Self>, peer_id: i64) {
        if !self.session_ready.get() {
            return;
        }
        let tg = self.tg.clone();
        let session_epoch = self.session_epoch.get();
        let weak = Rc::downgrade(&self);
        glib::MainContext::default().spawn_local(async move {
            let result = tg.call_start(peer_id).await;
            let Some(this) = weak.upgrade() else { return };
            if this.session_ready.get() && this.session_epoch.get() == session_epoch
                && let Err(error) = result {
                    this.messages.show_error(&error);
                }
        });
    }

    fn handle_call_action(self: Rc<Self>, action: CallAction) {
        if !self.session_ready.get() {
            return;
        }
        if self.probe && self.call.current().is_some_and(|info| info.id < 0) {
            self.drive_probe_incoming(action);
            return;
        }
        let tg = self.tg.clone();
        let session_epoch = self.session_epoch.get();
        let weak = Rc::downgrade(&self);
        glib::MainContext::default().spawn_local(async move {
            let result = match action {
                CallAction::Accept => tg.call_accept().await,
                CallAction::HangUp => tg.call_hang_up().await,
                CallAction::SetMuted(muted) => tg.call_set_muted(muted).await,
            };
            let Some(this) = weak.upgrade() else { return };
            if this.session_ready.get() && this.session_epoch.get() == session_epoch
                && let Err(error) = result {
                    this.messages.show_error(&error);
                    // A failed mute must not leave the toggle out of sync.
                    if matches!(action, CallAction::SetMuted(_)) {
                        this.call.resync_mute();
                    }
                }
        });
    }

    /// The production mock exposes incoming calls only through a process-start
    /// environment fixture. The default gate cannot restart its backend, so
    /// the probe drives the same event-shaped UI transition locally. Button
    /// actions and every rendered state are still exercised through CallView.
    fn drive_probe_incoming(self: Rc<Self>, action: CallAction) {
        let Some(mut info) = self.call.current().filter(|info| info.id < 0) else {
            return;
        };
        match action {
            CallAction::Accept if info.phase == CallPhase::Incoming => {
                info.phase = CallPhase::Exchanging;
                self.handle_call_changed(info.clone());
                glib::MainContext::default().spawn_local(async move {
                    glib::timeout_future(Duration::from_millis(40)).await;
                    if self.call.phase() != Some(CallPhase::Exchanging)
                        || self.call.current().as_ref().map(|call| call.id) != Some(info.id)
                    {
                        return;
                    }
                    info.phase = CallPhase::Connecting;
                    self.handle_call_changed(info.clone());
                    glib::timeout_future(Duration::from_millis(40)).await;
                    if self.call.phase() != Some(CallPhase::Connecting)
                        || self.call.current().as_ref().map(|call| call.id) != Some(info.id)
                    {
                        return;
                    }
                    info.phase = CallPhase::Active;
                    info.connected_at = Some(Local::now());
                    info.emojis = "🐴🍎🚗🌍".to_string();
                    self.handle_call_changed(info);
                });
            }
            CallAction::HangUp if info.phase != CallPhase::Ended => {
                info.end_reason = Some(if info.phase == CallPhase::Incoming {
                    CallEndReason::Declined
                } else {
                    CallEndReason::Hangup
                });
                info.phase = CallPhase::Ended;
                self.handle_call_changed(info);
            }
            CallAction::SetMuted(muted) if info.phase == CallPhase::Active => {
                info.muted = muted;
                self.handle_call_changed(info);
            }
            _ => {}
        }
    }

    fn handle_call_changed(self: &Rc<Self>, info: CallInfo) {
        let new_incoming = info.phase == CallPhase::Incoming
            && self.call.current().as_ref().is_none_or(|current| {
                current.phase != CallPhase::Incoming || current.id != info.id
            });
        if new_incoming && !self.window_is_active() {
            self.notify_incoming_call(&info.peer_name);
        } else if info.phase != CallPhase::Incoming {
            self.withdraw_call_notification();
        }
        self.call.update(info);
        // Mirror what the view accepted: its generation/late-Ended rules
        // decide identity, the status file never second-guesses them.
        crate::status::set_call(self.call.current().as_ref());
        self.apply_info_layout(self.current_window_width());
    }

    fn notify_incoming_call(&self, peer_name: &str) {
        let Some(window) = self.window() else { return };
        let Some(application) = window.application() else {
            return;
        };
        let title = if peer_name.trim().is_empty() {
            "Unknown caller"
        } else {
            peer_name
        };
        self.probe_notifications
            .set(self.probe_notifications.get().wrapping_add(1));
        if self.probe {
            return;
        }
        let title = glib::markup_escape_text(title);
        let notification = gio::Notification::new(title.as_str());
        notification.set_body(Some("Incoming voice call"));
        application.send_notification(Some("incoming-call"), &notification);
    }

    fn withdraw_call_notification(&self) {
        if let Some(application) = self.window().and_then(|window| window.application()) {
            application.withdraw_notification("incoming-call");
        }
    }

    fn confirm_destructive_chat_action(self: Rc<Self>, chat_id: i64, action: ChatAction) {
        if !self.session_ready.get() {
            return;
        }
        let Some(window) = self.window() else { return };
        let view_epoch = self.epoch.get();
        let session_epoch = self.session_epoch.get();
        let is_topic = split_topic_chat_id(chat_id).is_some();
        let title = self.title_for(chat_id);
        let (message, accept) = match action {
            ChatAction::ClearHistory if is_topic => (format!("Clear all messages in “{title}” for everyone? The topic stays available."), "Clear for everyone"),
            ChatAction::Delete if is_topic => (format!("Delete “{title}” and its messages for everyone?"), "Delete topic"),
            ChatAction::ClearHistory => (format!("Clear the history of “{title}”?"), "Clear"),
            ChatAction::Delete => (format!("Delete “{title}”?"), "Delete"),
            _ => return,
        };
        let dialog = gtk::AlertDialog::builder()
            .message(message)
            .buttons(["Cancel", accept])
            .default_button(0)
            .cancel_button(0)
            .build();
        glib::MainContext::default().spawn_local(async move {
            if dialog.choose_future(Some(&window)).await.ok() != Some(1) {
                return;
            }
            if !self.session_ready.get() {
                return;
            }
            self.begin_chat_mutation();
            let result = match action {
                ChatAction::ClearHistory => self.tg.clear_history(chat_id).await,
                ChatAction::Delete => self.tg.delete_chat(chat_id).await,
                _ => Ok(()),
            };
            self.finish_chat_mutation();
            if !self.session_ready.get() || self.session_epoch.get() != session_epoch {
                return;
            }
            match result {
                Ok(()) if matches!(action, ChatAction::ClearHistory) => {
                    self.bump_dialogs_revision();
                    if self.is_current(chat_id, view_epoch) {
                        self.clone().force_reload(chat_id);
                    }
                }
                Ok(()) if matches!(action, ChatAction::Delete) => {
                    self.bump_dialogs_revision();
                    if let Some((forum, _)) = split_topic_chat_id(chat_id) {
                        if self.is_current(chat_id, view_epoch) { self.clone().open_chat(forum); }
                        self.schedule_forum_topics_refresh(forum);
                        return;
                    }
                    self.chatlist.remove_chat(chat_id);
                    if self.is_current(chat_id, view_epoch) {
                        self.cancel_recording();
                        self.close_forward();
                        self.close_viewer();
                        self.close_profile();
                        self.close_in_chat_search();
                        self.open_chat.set(None);
                        let epoch = self.bump_epoch();
                        self.messages.clear_selection(epoch);
                    }
                }
                Ok(()) => {}
                Err(error) => self.messages.show_error(&error),
            }
        });
    }

    fn load_chat_info(self: &Rc<Self>, chat_id: i64, epoch: u64) {
        if let Some(info) = self.chat_info.borrow().get(&chat_id).cloned() {
            self.messages.set_chat_info(&info);
            return;
        }
        let tg = self.tg.clone();
        let session_epoch = self.session_epoch.get();
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let result = tg.get_chat_info(chat_id).await;
            let Some(this) = weak.upgrade().filter(|this| {
                this.session_ready.get() && this.session_epoch.get() == session_epoch
            }) else {
                return;
            };
            match result {
                Ok(info) => {
                    this.chat_info.borrow_mut().insert(chat_id, info.clone());
                    if this.is_current(chat_id, epoch) {
                        this.messages.set_chat_info(&info);
                    }
                }
                Err(error) => eprintln!("get_chat_info({chat_id}): {error}"),
            }
        });
    }

    fn toggle_info_panel(self: &Rc<Self>) {
        let desired = !self.ui_state.borrow().info_panel_open;
        self.ui_state.borrow_mut().info_panel_open = desired;
        self.schedule_ui_save();
        if desired {
            self.bind_info_panel();
        } else {
            self.info.unbind();
        }
        self.apply_info_layout(self.current_window_width());
    }

    fn close_info_panel(self: &Rc<Self>) {
        if !self.ui_state.borrow().info_panel_open {
            return;
        }
        self.ui_state.borrow_mut().info_panel_open = false;
        self.info.unbind();
        self.apply_info_layout(self.current_window_width());
        self.schedule_ui_save();
    }

    fn bind_info_panel(self: &Rc<Self>) {
        if !self.ui_state.borrow().info_panel_open {
            return;
        }
        // A forum topic has no chat info of its own: the panel shows the forum.
        let Some(chat_id) = self
            .open_chat
            .get()
            .map(dialog_id)
            .filter(|chat_id| !is_virtual(*chat_id))
        else {
            self.info.unbind();
            self.apply_info_layout(self.current_window_width());
            return;
        };
        let Some(mut summary) = self.chatlist.summary(chat_id) else {
            return;
        };
        if let Some(muted) = self.pending_mutes.borrow().get(&chat_id).copied() {
            summary.muted = muted;
        }
        let generation = self.info.bind(&summary);
        self.apply_info_layout(self.current_window_width());
        if summary.kind == ChatKind::Group {
            self.load_info_members(0);
        }
        self.load_info_shared_request();
        if let Some(mut info) = self.chat_info.borrow().get(&chat_id).cloned() {
            if let Some(muted) = self.pending_mutes.borrow().get(&chat_id).copied() {
                info.muted = muted;
            }
            self.info.finish_info(chat_id, generation, &info);
            return;
        }
        self.load_info_details(chat_id, generation);
    }

    fn load_info_details(self: &Rc<Self>, chat_id: i64, generation: u64) {
        let tg = self.tg.clone();
        let session_epoch = self.session_epoch.get();
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let result = tg.get_chat_info(chat_id).await;
            let Some(this) = weak.upgrade().filter(|this| this.is_session_current(session_epoch))
            else {
                return;
            };
            match result {
                Ok(info) => {
                    this.chat_info.borrow_mut().insert(chat_id, info.clone());
                    let mut visible = info;
                    if let Some(muted) = this.pending_mutes.borrow().get(&chat_id).copied() {
                        visible.muted = muted;
                    }
                    this.info.finish_info(chat_id, generation, &visible);
                }
                Err(error) => {
                    this.info.fail_info(chat_id, generation, &error);
                }
            }
        });
    }

    fn load_info_members(self: &Rc<Self>, offset: usize) {
        let Some((chat_id, generation, offset)) = self.info.begin_members(offset) else {
            return;
        };
        let tg = self.tg.clone();
        let session_epoch = self.session_epoch.get();
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let result = tg.get_members(chat_id, offset as i32, 50).await;
            let Some(this) = weak.upgrade().filter(|this| this.is_session_current(session_epoch))
            else {
                return;
            };
            match result {
                Ok(members) => {
                    this.info
                        .finish_members(chat_id, generation, offset, members);
                }
                Err(error) => {
                    this.info
                        .fail_members(chat_id, generation, offset, &error);
                }
            }
        });
    }

    fn load_info_shared_request(self: &Rc<Self>) {
        let Some(request) = self.info.begin_shared_page() else {
            return;
        };
        self.load_info_shared(request);
    }

    fn load_info_shared(
        self: &Rc<Self>,
        request: (i64, u64, u64, SharedKind, Option<i32>),
    ) {
        let (chat_id, bind_generation, shared_generation, kind, before_id) = request;
        let tg = self.tg.clone();
        let session_epoch = self.session_epoch.get();
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let result = tg.get_shared_media(chat_id, kind, before_id).await;
            let Some(this) = weak.upgrade().filter(|this| this.is_session_current(session_epoch))
            else {
                return;
            };
            match result {
                Ok(messages) => {
                    let photos = (kind == SharedKind::Photos).then(|| messages.clone());
                    if !this.info.finish_shared(
                        chat_id,
                        bind_generation,
                        shared_generation,
                        kind,
                        before_id,
                        messages,
                    ) {
                        return;
                    }
                    if let Some(photos) = photos {
                        for message in photos {
                            this.load_info_thumbnail(
                                chat_id,
                                bind_generation,
                                shared_generation,
                                message.id,
                            );
                        }
                    }
                }
                Err(error) => {
                    this.info.fail_shared(
                        chat_id,
                        bind_generation,
                        shared_generation,
                        kind,
                        &error,
                    );
                }
            }
        });
    }

    fn load_info_thumbnail(
        self: &Rc<Self>,
        chat_id: i64,
        bind_generation: u64,
        shared_generation: u64,
        msg_id: i32,
    ) {
        let tg = self.tg.clone();
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let Ok(Some(path)) = tg.download_media(chat_id, msg_id).await else {
                return;
            };
            let decode_path = path.clone();
            let Ok(Ok(texture)) = gio::spawn_blocking(move || {
                gdk::Texture::from_filename(&decode_path)
            })
            .await
            else {
                return;
            };
            if let Some(this) = weak.upgrade() {
                this.info.set_thumbnail(
                    chat_id,
                    bind_generation,
                    shared_generation,
                    msg_id,
                    path,
                    &texture,
                );
            }
        });
    }

    fn handle_info_action(self: Rc<Self>, action: InfoAction) {
        match action {
            InfoAction::Close => {
                self.close_info_panel();
            }
            InfoAction::OpenPhoto => {
                if let Some(id) = self.info.chat_id() {
                    let name = self.chatlist.summary(dialog_id(id)).map(|s| s.title).unwrap_or_else(|| "Profile photo".into());
                    self.open_profile_photo(dialog_id(id), &name);
                }
            }
            InfoAction::RetryInfo => {
                if let (Some(chat_id), generation) =
                    (self.info.chat_id(), self.info.bind_generation())
                {
                    self.load_info_details(chat_id, generation);
                }
            }
            InfoAction::SetNotifications(enabled) => {
                if let Some(chat_id) = self.info.chat_id() {
                    self.handle_chat_action(
                        chat_id,
                        ChatAction::Mute(if enabled {
                            MuteMode::Unmute
                        } else {
                            MuteMode::Forever
                        }),
                    );
                }
            }
            InfoAction::OpenMember(user_id) => self.open_profile(user_id, "Profile", None),
            InfoAction::MoreMembers | InfoAction::RetryMembers => {
                self.load_info_members(self.info.members_count());
            }
            InfoAction::SelectShared(kind) => {
                if let Some(request) = self.info.begin_shared(kind) {
                    self.load_info_shared(request);
                }
            }
            InfoAction::MoreShared | InfoAction::RetryShared => {
                self.load_info_shared_request();
            }
            InfoAction::OpenMedia(msg_id) => self.open_shared_media(msg_id),
        }
    }

    fn open_shared_media(self: &Rc<Self>, msg_id: i32) {
        let Some(chat_id) = self.info.chat_id() else {
            return;
        };
        let messages = self.info.shared_messages();
        let Some(message) = messages.iter().find(|message| message.id == msg_id).cloned() else {
            return;
        };
        match self.info.shared_kind() {
            SharedKind::Photos if message.media == Some(MediaKind::Photo) => {
                self.close_forward();
                self.close_viewer();
                self.close_profile();
                let generation = self.viewer_generation.get().wrapping_add(1);
                self.viewer_generation.set(generation);
                if self.viewer.present(
                    chat_id,
                    messages,
                    msg_id,
                    self.info.shared_path(msg_id),
                    generation,
                ) {
                    self.apply_info_layout(self.current_window_width());
                    self.clone().load_viewer_media(msg_id, generation);
                }
            }
            SharedKind::Links => {
                let target = message
                    .webpage
                    .as_ref()
                    .map(|preview| preview.url.as_str())
                    .unwrap_or(message.text.as_str());
                self.open_link(target);
            }
            SharedKind::Files | SharedKind::Voice | SharedKind::Music => {
                let bind_generation = self.info.bind_generation();
                let shared_generation = self.info.shared_generation();
                let kind = self.info.shared_kind();
                let session_epoch = self.session_epoch.get();
                let tg = self.tg.clone();
                let weak = Rc::downgrade(self);
                glib::MainContext::default().spawn_local(async move {
                    let result = tg.download_media(chat_id, msg_id).await;
                    let Some(this) = weak.upgrade().filter(|this| {
                        this.is_session_current(session_epoch)
                            && this.info.is_bound(chat_id)
                            && this.info.bind_generation() == bind_generation
                            && this.info.shared_generation() == shared_generation
                            && this.info.shared_kind() == kind
                    }) else {
                        return;
                    };
                    match result {
                        Ok(Some(path)) => this.launch_media(&path),
                        Ok(None) => this.messages.show_error("Media unavailable"),
                        Err(error) => this.messages.show_error(&error),
                    }
                });
            }
            SharedKind::Photos => {}
        }
    }

    fn open_stickers(self: &Rc<Self>) {
        if self.open_chat.get().is_none_or(is_virtual)
            || self.composer_operation.get()
            || self.messages.is_busy()
            || self.messages.selection_mode()
        {
            return;
        }
        self.close_forward();
        self.close_viewer();
        self.close_profile();
        self.close_contacts();
        self.close_new_group();
        let generation = self.stickers.begin();
        self.messages
            .show_sticker_popover(self.stickers.popover());
        self.apply_info_layout(self.current_window_width());
        self.load_sticker_packs(generation);
    }

    fn close_stickers(&self) {
        if self.stickers.is_open() {
            self.messages.dismiss_composer_popover();
        }
        self.stickers.close();
        self.apply_info_layout(self.current_window_width());
    }

    fn load_sticker_packs(self: &Rc<Self>, generation: u64) {
        let tg = self.tg.clone();
        let session_epoch = self.session_epoch.get();
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let result = tg.get_sticker_packs().await;
            let Some(this) = weak.upgrade().filter(|this| {
                this.is_session_current(session_epoch)
                    && this.stickers.generation() == generation
                    && this.stickers.is_open()
            }) else {
                return;
            };
            match result {
                Ok(packs) => {
                    this.stickers.finish_packs(generation, packs);
                }
                Err(error) => {
                    this.stickers.fail_packs(generation, &error);
                }
            }
        });
    }

    fn load_sticker_pack(self: &Rc<Self>, pack_id: String) {
        let (generation, content_generation, key) = self.stickers.begin_pack(&pack_id);
        let tg = self.tg.clone();
        let session_epoch = self.session_epoch.get();
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            if key == "gifs" {
                let result = tg.get_saved_gifs().await;
                let Some(this) = weak.upgrade().filter(|this| {
                    this.is_session_current(session_epoch)
                        && this.stickers.is_open()
                        && this.stickers.generation() == generation
                        && this.stickers.content_generation() == content_generation
                }) else {
                    return;
                };
                match result {
                    Ok(gifs) => {
                        let ids = gifs.iter().map(|gif| gif.id).collect::<Vec<_>>();
                        if this
                            .stickers
                            .finish_gifs(generation, content_generation, gifs)
                        {
                            for id in ids {
                                this.download_gif_card(generation, content_generation, id);
                            }
                        }
                    }
                    Err(error) => {
                        this.stickers
                            .fail_pack(generation, content_generation, "gifs", &error);
                    }
                }
                return;
            }
            let result = tg.get_stickers(&key).await;
            let Some(this) = weak.upgrade().filter(|this| {
                this.is_session_current(session_epoch)
                    && this.stickers.is_open()
                    && this.stickers.generation() == generation
                    && this.stickers.content_generation() == content_generation
            }) else {
                return;
            };
            match result {
                Ok(stickers) => {
                    // Wave 6D: animated .tgs cells load too (Lottie); only
                    // .webm video stickers still have no preview surface.
                    let cell_ids = stickers
                        .iter()
                        .filter_map(|sticker| (!sticker.video).then_some(sticker.id))
                        .collect::<Vec<_>>();
                    if this.stickers.finish_stickers(
                        generation,
                        content_generation,
                        &key,
                        stickers,
                    ) {
                        for sticker_id in cell_ids {
                            this.download_sticker_cell(
                                generation,
                                content_generation,
                                key.clone(),
                                sticker_id,
                            );
                        }
                    }
                }
                Err(error) => {
                    this.stickers
                        .fail_pack(generation, content_generation, &key, &error);
                }
            }
        });
    }

    fn download_sticker_cell(
        self: &Rc<Self>,
        generation: u64,
        content_generation: u64,
        pack_id: String,
        sticker_id: i64,
    ) {
        let tg = self.tg.clone();
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            // A32/A33: a missing file, a download error or an undecodable
            // image must be visible on the cell, not silently swallowed.
            let unavailable = |this: Option<Rc<Self>>| {
                if let Some(this) = this {
                    this.stickers.mark_unavailable(
                        generation,
                        content_generation,
                        &pack_id,
                        sticker_id,
                    );
                }
            };
            let path = match tg.download_sticker(sticker_id).await {
                Ok(Some(path)) => path,
                Ok(None) | Err(_) => return unavailable(weak.upgrade()),
            };
            // Wave 6D: .tgs cells render through the Lottie thread, not the
            // image decoder (which would fail and mark them unavailable).
            if is_lottie(&path) {
                if let Some(this) = weak.upgrade() {
                    this.stickers.set_lottie_cell(
                        generation,
                        content_generation,
                        &pack_id,
                        sticker_id,
                        path,
                    );
                }
                return;
            }
            let decode_path = path.clone();
            let decoded =
                gio::spawn_blocking(move || gdk::Texture::from_filename(&decode_path)).await;
            let Ok(Ok(texture)) = decoded else {
                return unavailable(weak.upgrade());
            };
            if let Some(this) = weak.upgrade() {
                this.stickers.set_sticker_texture(
                    generation,
                    content_generation,
                    &pack_id,
                    sticker_id,
                    path,
                    &texture,
                );
            }
        });
    }

    fn download_gif_card(
        self: &Rc<Self>,
        generation: u64,
        content_generation: u64,
        gif_id: i64,
    ) {
        let tg = self.tg.clone();
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let result = tg.download_gif(gif_id).await;
            let Some(this) = weak.upgrade() else { return };
            // GIF cells carry no preview surface yet; when the file is not
            // there the card says so instead of pretending to be loading.
            match result {
                Ok(Some(path)) => {
                    this.stickers
                        .mark_gif_ready(generation, content_generation, gif_id, path);
                }
                _ => {
                    this.stickers
                        .mark_unavailable(generation, content_generation, "gifs", gif_id);
                }
            }
        });
    }

    fn handle_sticker_action(self: Rc<Self>, action: StickerAction) {
        match action {
            StickerAction::RetryPacks => {
                let generation = self.stickers.begin();
                self.load_sticker_packs(generation);
            }
            StickerAction::SelectPack(pack_id) => self.load_sticker_pack(pack_id),
            StickerAction::RetryPack => {
                let pack_id = self.stickers.current_pack();
                if !pack_id.is_empty() {
                    self.load_sticker_pack(pack_id);
                }
            }
            StickerAction::Send(send) => self.send_sticker_or_gif(send),
            StickerAction::RetrySend => {
                if let Some(send) = self.stickers.retry_send() {
                    self.send_sticker_or_gif(send);
                }
            }
        }
    }

    fn send_sticker_or_gif(self: Rc<Self>, send: StickerSend) {
        if self.composer_operation.get() || self.messages.is_busy() {
            return;
        }
        let Some(chat_id) = self.open_chat.get().filter(|chat_id| !is_virtual(*chat_id)) else {
            return;
        };
        let epoch = self.epoch.get();
        let title = self.title_for(chat_id);
        let Some(session_epoch) = self.begin_mutation() else {
            return;
        };
        let token = self.acquire_composer();
        self.messages.set_busy(true);
        glib::MainContext::default().spawn_local(async move {
            let result = match send {
                StickerSend::Sticker(sticker_id) => {
                    self.tg.send_sticker(chat_id, sticker_id).await
                }
                StickerSend::Gif(gif_id) => self.tg.send_gif(chat_id, gif_id).await,
            };
            self.finish_mutation();
            if !self.is_session_current(session_epoch) {
                return;
            }
            // A7: only the operation that still owns the lock may clear it
            // (and the busy state it took with it).
            if self.release_composer(token) {
                self.messages.set_busy(false);
            }
            match result {
                Ok(message) => {
                    self.stickers.finish_send();
                    self.remember_last(&message);
                    self.dialog_upsert(
                        chat_id,
                        &title,
                        &message_preview(&message),
                        Some(message.ts),
                        UnreadUpdate::Delta(0),
                    );
                    if self.is_current(chat_id, epoch) {
                        let inserted = self.messages.merge_event(message);
                        self.post_render(inserted);
                    }
                }
                Err(error) => {
                    if self.is_current(chat_id, epoch) {
                        self.stickers.fail_send(send, &error);
                    }
                }
            }
        });
    }

    fn start_recording(self: &Rc<Self>) {
        if self.composer_operation.get()
            || self.messages.is_busy()
            || self.open_chat.get().is_none_or(is_virtual)
        {
            return;
        }
        let target = RecordTarget {
            chat_id: self.open_chat.get().unwrap_or_default(),
            epoch: self.epoch.get(),
        };
        if !self.recorder.borrow_mut().begin_start(target) {
            return;
        }
        self.close_stickers();
        self.voice_retry.borrow_mut().take();
        let token = self.acquire_composer();
        self.recorder_token.set(token);
        self.messages.clear_error();
        self.messages.show_recorder_starting();
        let local = self.local.clone();
        let session_epoch = self.session_epoch.get();
        let start_delay = self.probe_record_start_delay.get();
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            if start_delay > 0 {
                glib::timeout_future(Duration::from_millis(start_delay)).await;
            }
            let result = local.record_start().await;
            let Some(this) = weak.upgrade() else { return };
            let resolution = this
                .recorder
                .borrow_mut()
                .resolve_start(target, result.is_ok());
            match resolution {
                StartResolution::Recording(_) => {
                    if this.is_session_current(session_epoch)
                        && this.is_current(target.chat_id, target.epoch)
                    {
                        this.messages.show_recorder_recording();
                    } else {
                        this.cancel_recording();
                    }
                }
                StartResolution::CancelNow(_) => {
                    // The backend slot only frees once `record_cancel`
                    // returns; hold the lock until then so the next
                    // `record_start` cannot race it.
                    local.record_cancel().await;
                    if this.release_composer(token)
                        && this.is_current(target.chat_id, target.epoch)
                    {
                        this.messages.hide_recorder();
                    }
                }
                StartResolution::Failed(_) => {
                    if this.release_composer(token)
                        && this.is_session_current(session_epoch)
                        && this.is_current(target.chat_id, target.epoch)
                    {
                        this.messages
                            .show_recorder_error(&recording_error(result.err().as_deref()), false);
                    }
                }
                StartResolution::Stale => {
                    this.release_composer(token);
                }
            }
        });
    }

    fn cancel_recording(self: &Rc<Self>) {
        self.cancel_video_note();
        self.voice_retry.borrow_mut().take();
        let token = self.recorder_token.get();
        match self.recorder.borrow_mut().cancel() {
            CancelCommand::Now(target) => {
                // Hide the bar at once, but keep the composer locked until the
                // backend recording slot is actually released: `record_start`
                // fails while a cancel is still in flight.
                if self.is_current(target.chat_id, target.epoch) {
                    self.messages.hide_recorder();
                    // The composer is visible again but still locked until the
                    // backend slot is free; show that instead of dropping input.
                    self.messages.set_busy(true);
                }
                let local = self.local.clone();
                let weak = Rc::downgrade(self);
                glib::MainContext::default().spawn_local(async move {
                    local.record_cancel().await;
                    if let Some(this) = weak.upgrade()
                        && this.release_composer(token) && this.is_current(target.chat_id, target.epoch) {
                            this.messages.set_busy(false);
                        }
                });
            }
            CancelCommand::Deferred => {
                // A8: the outstanding start/stop completion owns both the
                // backend cancel and this lock. Releasing here would unlock a
                // composer that is still mid-operation, so keep the lock and
                // the bar until that completion resolves. A chat switch hides
                // the departed view's bar through `reset_chat`.
                self.messages.show_recorder_cancelling();
            }
            CancelCommand::None => {
                if self.messages.recorder_visible() {
                    self.release_composer(token);
                    self.messages.hide_recorder();
                }
            }
        }
    }

    fn stop_and_send_recording(self: &Rc<Self>) {
        let Some(target) = self.recorder.borrow_mut().begin_stop() else {
            return;
        };
        self.messages.show_recorder_stopping();
        let local = self.local.clone();
        let session_epoch = self.session_epoch.get();
        let token = self.recorder_token.get();
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let result = local.record_stop().await;
            let Some(this) = weak.upgrade() else { return };
            let resolution = this
                .recorder
                .borrow_mut()
                .resolve_stop(target, result.is_ok());
            match resolution {
                StopResolution::Sending(_) => {
                    if let Ok((path, duration)) = result {
                        this.send_voice_pending(
                            PendingVoice {
                                target,
                                path,
                                duration,
                            },
                            session_epoch,
                        );
                    }
                }
                StopResolution::Cancelled(_) => {
                    local.record_cancel().await;
                    if this.release_composer(token)
                        && this.is_current(target.chat_id, target.epoch)
                    {
                        this.messages.hide_recorder();
                    }
                }
                StopResolution::Failed(_) => {
                    if this.release_composer(token)
                        && this.is_session_current(session_epoch)
                        && this.is_current(target.chat_id, target.epoch)
                    {
                        this.messages
                            .show_recorder_error(&recording_error(result.err().as_deref()), false);
                    }
                }
                StopResolution::Stale => {
                    this.release_composer(token);
                }
            }
        });
    }

    fn retry_voice(self: &Rc<Self>) {
        // C5/A7: a retry is a composer operation like any other and must not
        // take the lock (nor the recorder token) from one already running.
        if self.composer_operation.get() || self.messages.is_busy() {
            return;
        }
        let Some(pending) = self.voice_retry.borrow().clone() else {
            return;
        };
        if !self.is_current(pending.target.chat_id, pending.target.epoch)
            || !self.recorder.borrow_mut().retry_send(pending.target)
        {
            return;
        }
        self.recorder_token.set(self.acquire_composer());
        self.send_voice_pending(pending, self.session_epoch.get());
    }

    fn send_voice_pending(
        self: &Rc<Self>,
        pending: PendingVoice,
        session_epoch: u64,
    ) {
        let token = self.recorder_token.get();
        let Some(mutation_epoch) = self.begin_mutation() else {
            self.recorder.borrow_mut().finish_send(pending.target);
            self.release_composer(token);
            return;
        };
        self.messages.show_recorder_sending();
        let title = self.title_for(pending.target.chat_id);
        let weak = Rc::downgrade(self);
        let tg = self.tg.clone();
        glib::MainContext::default().spawn_local(async move {
            let result = tg
                .send_voice(
                    pending.target.chat_id,
                    pending.path.clone(),
                    pending.duration,
                )
                .await;
            let Some(this) = weak.upgrade() else { return };
            this.finish_mutation();
            let finished = this.recorder.borrow_mut().finish_send(pending.target);
            if finished {
                this.release_composer(token);
            }
            if mutation_epoch != session_epoch || !this.is_session_current(session_epoch) {
                return;
            }
            // The machine already discarded this send (cancelled via Esc or
            // selection mode): never touch a bar that may belong to a newer
            // recording, and never re-arm a retry for it.
            if !finished {
                if let Ok(message) = result {
                    this.remember_last(&message);
                    this.dialog_upsert(
                        pending.target.chat_id,
                        &title,
                        &message_preview(&message),
                        Some(message.ts),
                        UnreadUpdate::Delta(0),
                    );
                    if this.is_current(pending.target.chat_id, pending.target.epoch) {
                        let inserted = this.messages.merge_event(message);
                        this.post_render(inserted);
                    }
                }
                return;
            }
            match result {
                Ok(message) => {
                    this.voice_retry.borrow_mut().take();
                    this.remember_last(&message);
                    this.dialog_upsert(
                        pending.target.chat_id,
                        &title,
                        &message_preview(&message),
                        Some(message.ts),
                        UnreadUpdate::Delta(0),
                    );
                    if this.is_current(pending.target.chat_id, pending.target.epoch) {
                        let inserted = this.messages.merge_event(message);
                        this.post_render(inserted);
                        this.messages.hide_recorder();
                    }
                }
                Err(error) => {
                    *this.voice_retry.borrow_mut() = Some(pending.clone());
                    if this.is_current(pending.target.chat_id, pending.target.epoch) {
                        this.messages
                            .show_recorder_error(&recording_error(Some(&error)), true);
                    }
                }
            }
        });
    }

    fn start_video_note(self: &Rc<Self>) {
        if self.open_chat.get().is_none_or(is_virtual) {
            return;
        }
        self.cancel_recording();
        let target = {
            let mut state = self.video_recorder.borrow_mut();
            state.token = state.token.wrapping_add(1);
            VideoRecorderTarget {
                chat_id: self.open_chat.get().unwrap_or_default(),
                chat_epoch: self.epoch.get(),
                session_epoch: self.session_epoch.get(),
                token: state.token,
            }
        };
        let start_now = {
            let mut state = self.video_recorder.borrow_mut();
            if state.phase == VideoRecorderPhase::Idle {
                state.phase = VideoRecorderPhase::Starting(target);
                true
            } else {
                // Cancel/start/stop is serialized. The current command owns the
                // local recorder until it resolves and performs any required
                // cleanup; only then may this newest request start.
                state.pending_start = Some(target);
                false
            }
        };
        self.messages.show_video_note_starting();
        if start_now {
            self.spawn_video_note_start(target);
        }
    }

    fn spawn_video_note_start(self: &Rc<Self>, target: VideoRecorderTarget) {
        let local = self.local.clone();
        let weak = Rc::downgrade(self);
        let attach_delay = self.probe_video_start_attach_delay.get();
        glib::MainContext::default().spawn_local(async move {
            let result = local.video_start(240).await;
            if attach_delay > 0 {
                glib::timeout_future(Duration::from_millis(attach_delay)).await;
            }
            match result {
                Ok(rx) => {
                    let Some(this) = weak.upgrade() else {
                        // The process exists even if the window disappeared
                        // while start was in flight.
                        local.video_cancel().await;
                        return;
                    };
                    let attach = {
                        let mut state = this.video_recorder.borrow_mut();
                        let current = state.phase == VideoRecorderPhase::Starting(target)
                            && state.token == target.token
                            && this.is_session_current(target.session_epoch)
                            && this.is_current(target.chat_id, target.chat_epoch);
                        if current {
                            state.phase = VideoRecorderPhase::Recording(target);
                            true
                        } else {
                            if state.phase == VideoRecorderPhase::Starting(target) {
                                state.phase = VideoRecorderPhase::Cancelling(target);
                            }
                            false
                        }
                    };
                    if attach {
                        this.messages.show_video_note_recording(rx);
                    } else {
                        // A cancel/restart/session switch made this start stale
                        // after it had created a process. Release that exact
                        // serialized recorder slot before launching a pending
                        // request, so the receiver can never attach to it.
                        local.video_cancel().await;
                        this.finish_video_note_cancel(target);
                    }
                }
                Err(error) => {
                    let Some(this) = weak.upgrade() else { return };
                    let show_error = {
                        let mut state = this.video_recorder.borrow_mut();
                        let current = state.phase == VideoRecorderPhase::Starting(target)
                            && state.token == target.token
                            && this.is_session_current(target.session_epoch)
                            && this.is_current(target.chat_id, target.chat_epoch);
                        if state.phase == VideoRecorderPhase::Starting(target) {
                            state.phase = VideoRecorderPhase::Idle;
                        }
                        current
                    };
                    if show_error {
                        this.messages.show_video_note_error(&error);
                    }
                    this.launch_pending_video_note_start();
                }
            }
        });
    }

    fn launch_pending_video_note_start(self: &Rc<Self>) {
        let pending = {
            let mut state = self.video_recorder.borrow_mut();
            if state.phase != VideoRecorderPhase::Idle {
                return;
            }
            state.pending_start.take()
        };
        let Some(target) = pending else { return };
        let current = {
            let state = self.video_recorder.borrow();
            state.token == target.token
                && self.is_session_current(target.session_epoch)
                && self.is_current(target.chat_id, target.chat_epoch)
        };
        if !current {
            return;
        }
        self.video_recorder.borrow_mut().phase = VideoRecorderPhase::Starting(target);
        self.messages.show_video_note_starting();
        self.spawn_video_note_start(target);
    }

    fn finish_video_note_cancel(self: &Rc<Self>, target: VideoRecorderTarget) {
        {
            let mut state = self.video_recorder.borrow_mut();
            if state.phase != VideoRecorderPhase::Cancelling(target) {
                return;
            }
            state.phase = VideoRecorderPhase::Idle;
        }
        self.launch_pending_video_note_start();
    }

    fn cancel_video_note(self: &Rc<Self>) {
        let cancel_now = {
            let mut state = self.video_recorder.borrow_mut();
            state.token = state.token.wrapping_add(1);
            state.pending_start = None;
            match state.phase {
                VideoRecorderPhase::Recording(target) => {
                    state.phase = VideoRecorderPhase::Cancelling(target);
                    Some(target)
                }
                // A Starting completion owns cleanup if it produced a process;
                // Cancelling/Stopping already own their serialized command.
                VideoRecorderPhase::Idle
                | VideoRecorderPhase::Starting(_)
                | VideoRecorderPhase::Cancelling(_)
                | VideoRecorderPhase::Stopping(_) => None,
            }
        };
        self.messages.hide_video_note();
        if let Some(target) = cancel_now {
            let local = self.local.clone();
            let weak = Rc::downgrade(self);
            glib::MainContext::default().spawn_local(async move {
                local.video_cancel().await;
                if let Some(this) = weak.upgrade() {
                    this.finish_video_note_cancel(target);
                }
            });
        }
    }

    fn send_video_note(self: &Rc<Self>) {
        let target = {
            let state = self.video_recorder.borrow();
            match state.phase {
                VideoRecorderPhase::Recording(target) if state.token == target.token => target,
                _ => return,
            }
        };
        if !self.is_session_current(target.session_epoch)
            || !self.is_current(target.chat_id, target.chat_epoch)
        {
            self.cancel_video_note();
            return;
        }
        let Some(session_epoch) = self.begin_mutation() else {
            self.cancel_video_note();
            return;
        };
        let chat_id = target.chat_id;
        let epoch = target.chat_epoch;
        let title = self.title_for(chat_id);
        self.video_recorder.borrow_mut().phase = VideoRecorderPhase::Stopping(target);
        self.messages.show_video_note_stopping();
        let local = self.local.clone();
        let tg = self.tg.clone();
        let this = self.clone();
        glib::MainContext::default().spawn_local(async move {
            let stop_res = local.video_stop().await;
            let owns_ui = {
                let mut state = this.video_recorder.borrow_mut();
                if state.phase == VideoRecorderPhase::Stopping(target) {
                    state.phase = VideoRecorderPhase::Idle;
                }
                state.token == target.token && state.pending_start.is_none()
            };
            let (path, duration) = match stop_res {
                Ok(res) => res,
                Err(error) => {
                    shell_log!("video note stop: {error}");
                    this.finish_mutation();
                    if owns_ui && this.is_session_current(session_epoch) {
                        this.messages.show_video_note_error(&error);
                    }
                    this.launch_pending_video_note_start();
                    return;
                }
            };
            if owns_ui {
                this.messages.hide_video_note();
            }
            this.launch_pending_video_note_start();
            let result = tg.send_video_note(chat_id, path, duration, 240).await;
            this.finish_mutation();
            if !this.is_session_current(session_epoch) {
                return;
            }
            match result {
                Ok(message) => {
                    let message = this.apply_tombstone(message);
                    if !message.deleted {
                        this.remember_last(&message);
                        this.dialog_upsert(
                            chat_id,
                            &title,
                            &message_preview(&message),
                            Some(message.ts),
                            UnreadUpdate::Delta(0),
                        );
                    }
                    if this.is_current(chat_id, epoch) {
                        let inserted = this.messages.merge_event(message);
                        this.post_render(inserted);
                    }
                }
                Err(error) => {
                    shell_log!("send_video_note({chat_id}): {error}");
                    if this.is_current(chat_id, epoch) {
                        this.messages.show_error(&error);
                    }
                }
            }
        });
    }

    fn track_own_live_location(self: &Rc<Self>, chat_id: i64, msg_id: i32, expires: DateTime<Local>) {
        let mut list = self.own_live_locations.borrow_mut();
        if !list.iter().any(|(c, m, _)| *c == chat_id && *m == msg_id) {
            list.push((chat_id, msg_id, expires));
        }
        drop(list);
        self.schedule_live_expiry_check();
    }

    fn schedule_live_expiry_check(self: &Rc<Self>) {
        if self.live_expiry_timer.borrow().is_some() {
            return;
        }
        let this_weak = Rc::downgrade(self);
        let source = glib::timeout_add_seconds_local(1, move || {
            let Some(this) = this_weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            let now = Local::now();
            let mut list = this.own_live_locations.borrow_mut();
            let mut expired = Vec::new();
            list.retain(|(chat_id, msg_id, expires)| {
                if now >= *expires {
                    expired.push((*chat_id, *msg_id));
                    false
                } else {
                    true
                }
            });
            let keep_going = !list.is_empty();
            drop(list);
            for (chat_id, msg_id) in expired {
                if this.open_chat.get() == Some(chat_id) {
                    this.messages.expire_live_location(msg_id);
                }
            }
            if !keep_going {
                *this.live_expiry_timer.borrow_mut() = None;
                glib::ControlFlow::Break
            } else {
                glib::ControlFlow::Continue
            }
        });
        *self.live_expiry_timer.borrow_mut() = Some(source);
    }

    fn check_own_live_message(self: &Rc<Self>, message: &Msg) {
        if message.outgoing
            && let Some(loc) = &message.location
                && let Some(live) = &loc.live
                    && !live.stopped && Local::now() <= live.expires {
                        self.track_own_live_location(message.chat_id, message.id, live.expires);
                    }
    }

    fn reload_stories(self: &Rc<Self>) {
        if !self.session_ready.get() {
            return;
        }
        let generation = self.stories_generation.get().wrapping_add(1);
        self.stories_generation.set(generation);
        let session_epoch = self.session_epoch.get();
        let tg = self.tg.clone();
        let this = self.clone();
        glib::MainContext::default().spawn_local(async move {
            if let Ok(peers) = tg.get_story_peers().await {
                if !this.is_session_current(session_epoch)
                    || this.stories_generation.get() != generation
                {
                    return;
                }
                *this.stories_peers.borrow_mut() = peers.clone();
                this.chatlist.set_story_peers(&peers);
                this.stories_strip.update_peers(peers);
            }
        });
    }

    fn open_stories_for_peer(self: &Rc<Self>, chat_id: i64) {
        if !self.session_ready.get() {
            return;
        }
        let peers = self.stories_peers.borrow().clone();
        self.stories_viewer.open(peers, chat_id);
    }

    fn capture_current_draft(&self, chat_id: i64) {
        let (text, reply_to, cursor) = self.messages.draft_snapshot_for_switch();
        let mut drafts = self.drafts.borrow_mut();
        let state = drafts.entry(chat_id).or_default();
        if state.current.text == text
            && state.current.reply_to == reply_to
            && state.current.cursor == cursor
        {
            return;
        }
        state.current = DraftSnapshot {
            text: text.clone(),
            reply_to,
            cursor,
            revision: state.current.revision.wrapping_add(1),
        };
        state.dirty = true;
        drop(drafts);
        self.bump_dialogs_revision();
        self.chatlist.set_draft(chat_id, &text);
    }

    fn composer_draft_changed(self: &Rc<Self>) {
        if !self.session_ready.get() {
            return;
        }
        let Some(chat_id) = self.open_chat.get().filter(|id| !is_virtual(*id)) else {
            return;
        };
        if self.messages.is_editing() {
            return;
        }
        self.capture_current_draft(chat_id);
        if let Some(source) = self.draft_timeouts.borrow_mut().remove(&chat_id) {
            source.remove();
        }
        let weak = Rc::downgrade(self);
        let source = glib::timeout_add_local_once(Duration::from_secs(1), move || {
            let Some(this) = weak.upgrade() else { return };
            this.draft_timeouts.borrow_mut().remove(&chat_id);
            this.flush_draft(chat_id);
        });
        self.draft_timeouts.borrow_mut().insert(chat_id, source);
    }

    fn restore_draft(&self, chat_id: i64) {
        let topic = self.forum_topics.borrow().iter().find(|t| t.chat_id == chat_id).cloned();
        let server_reply = topic.as_ref().and_then(|t| t.draft_reply_to);
        let server_draft = topic.map(|t| t.draft).or_else(|| self.chatlist.summary(chat_id).map(|s| s.draft)).unwrap_or_default();
        let snapshot = {
            let mut drafts = self.drafts.borrow_mut();
            let state = drafts.entry(chat_id).or_default();
            if state.current.text.is_empty() && !state.dirty && !server_draft.is_empty() {
                state.current.text = server_draft;
                state.current.reply_to = server_reply;
                state.current.cursor = state.current.text.chars().count() as i32;
            }
            state.current.clone()
        };
        self.messages
            .restore_draft(&snapshot.text, snapshot.reply_to, snapshot.cursor);
    }

    fn flush_draft(self: &Rc<Self>, chat_id: i64) {
        if let Some(source) = self.draft_timeouts.borrow_mut().remove(&chat_id) {
            source.remove();
        }
        let snapshot = self
            .drafts
            .borrow()
            .get(&chat_id)
            .filter(|state| state.dirty)
            .map(|state| state.current.clone());
        if let Some(snapshot) = snapshot {
            self.send_draft_snapshot(chat_id, snapshot);
        }
    }

    fn send_draft_snapshot(self: &Rc<Self>, chat_id: i64, snapshot: DraftSnapshot) {
        if !self.session_ready.get() {
            return;
        }
        {
            let mut drafts = self.drafts.borrow_mut();
            let state = drafts.entry(chat_id).or_default();
            if state.in_flight {
                state.queued = Some(snapshot);
                return;
            }
            state.in_flight = true;
        }
        let tg = self.tg.clone();
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let result = tg
                .save_draft(chat_id, &snapshot.text, snapshot.reply_to)
                .await;
            let Some(this) = weak.upgrade() else { return };
            let next = {
                let mut drafts = this.drafts.borrow_mut();
                let state = drafts.entry(chat_id).or_default();
                state.in_flight = false;
                if result.is_ok() && state.current.revision == snapshot.revision {
                    state.dirty = false;
                }
                match state.queued.take() {
                    Some(queued) if queued.revision >= state.current.revision => Some(queued),
                    Some(_) if state.dirty => Some(state.current.clone()),
                    _ => None,
                }
            };
            match result {
                Ok(()) => {
                    if this.open_chat.get() == Some(chat_id) {
                        this.messages.clear_draft_error();
                    }
                }
                Err(error) => {
                    if this.open_chat.get() == Some(chat_id) {
                        this.messages.show_draft_error(&error);
                    }
                }
            }
            if let Some(next) = next {
                this.send_draft_snapshot(chat_id, next);
            }
        });
    }

    fn clear_sent_draft(self: &Rc<Self>, chat_id: i64) {
        let snapshot = {
            let mut drafts = self.drafts.borrow_mut();
            let state = drafts.entry(chat_id).or_default();
            let revision = state.current.revision.wrapping_add(1);
            state.current = DraftSnapshot {
                revision,
                ..DraftSnapshot::default()
            };
            state.dirty = true;
            state.current.clone()
        };
        self.bump_dialogs_revision();
        self.chatlist.set_draft(chat_id, "");
        self.send_draft_snapshot(chat_id, snapshot);
    }

    fn spawn_event_loop(self: &Rc<Self>) {
        let this = self.clone();
        let events = self.tg.events.clone();
        glib::MainContext::default().spawn_local(async move {
            while let Ok(event) = events.recv().await {
                if this.session_ready.get() {
                    this.handle_event(event);
                }
            }
            shell_log!("event loop: backend event stream closed");
        });
    }

    fn load_dialogs(self: &Rc<Self>) {
        if !self.session_ready.get() {
            return;
        }
        if self.dialogs_in_flight.replace(true) {
            self.dialogs_refresh_again.set(true);
            return;
        }
        if !self.dialogs_loaded.get() {
            self.dialogs_error.set_label("Loading your chats…");
            self.dialogs_error.remove_css_class("omg-error");
            self.dialogs_error.add_css_class("omg-muted");
            self.dialogs_spinner.set_visible(true);
            self.dialogs_spinner.start();
            self.dialogs_retry.set_visible(false);
            self.dialogs_error_box.set_visible(true);
        }
        let requested_revision = self.dialogs_revision.get();
        let session_epoch = self.session_epoch.get();
        let this = self.clone();
        glib::MainContext::default().spawn_local(async move {
            let result = this.tg.get_dialogs().await;
            this.dialogs_in_flight.set(false);
            if !this.session_ready.get() || this.session_epoch.get() != session_epoch {
                return;
            }
            this.dialogs_spinner.stop();
            this.dialogs_spinner.set_visible(false);
            match result {
                Ok(mut dialogs) => {
                    if std::env::var_os("OMG_STARTUP_TRACE").is_some() {
                        eprintln!("[startup] {} chats returned; revision {} -> {}", dialogs.len(), requested_revision, this.dialogs_revision.get());
                    }
                    if requested_revision != this.dialogs_revision.get() {
                        this.dialogs_refresh_again.set(false);
                        this.load_dialogs();
                        return;
                    }
                    for dialog in &mut dialogs {
                        let mut read = this.read_outbox.borrow_mut();
                        let known = read.entry(dialog.id).or_insert(dialog.read_outbox_max_id);
                        *known = (*known).max(dialog.read_outbox_max_id);
                        dialog.read_outbox_max_id = *known;
                        drop(read);

                        if let Some(local) = this.chatlist.summary(dialog.id) {
                            let local_is_newer = match (local.last_time, dialog.last_time) {
                                (Some(local_time), Some(server_time)) => {
                                    local_time > server_time
                                        || (local_time == server_time
                                            && local.last_msg_id > dialog.last_msg_id)
                                }
                                (Some(_), None) => true,
                                _ => false,
                            };
                            if local_is_newer {
                                dialog.last_message = local.last_message;
                                dialog.last_sender = local.last_sender;
                                dialog.last_time = local.last_time;
                                dialog.last_msg_id = local.last_msg_id;
                                dialog.last_outgoing = local.last_outgoing;
                                dialog.unread = local.unread;
                                dialog.mentions = local.mentions;
                                dialog.unread_mark = local.unread_mark;
                                dialog.read_inbox_max_id = local.read_inbox_max_id;
                            } else if this.manual_unread_hold.borrow().contains(&dialog.id) {
                                dialog.unread = local.unread;
                                dialog.unread_mark = true;
                            }
                        }

                        let mut drafts = this.drafts.borrow_mut();
                        match drafts.get_mut(&dialog.id) {
                            Some(state) if state.dirty => {
                                dialog.draft = state.current.text.clone();
                            }
                            Some(_) => {}
                            None => {
                                drafts.insert(
                                    dialog.id,
                                    DraftState {
                                        current: DraftSnapshot {
                                            text: dialog.draft.clone(),
                                            cursor: dialog.draft.chars().count() as i32,
                                            ..DraftSnapshot::default()
                                        },
                                        ..DraftState::default()
                                    },
                                );
                            }
                        }
                    }
                    let dialog_times: HashMap<i64, Option<chrono::DateTime<chrono::Local>>> =
                        dialogs
                            .iter()
                            .map(|dialog| (dialog.id, dialog.last_time))
                            .collect();
                    this.chatlist.set_chats(dialogs);
                    this.chatlist.set_story_peers(&this.stories_peers.borrow());
                    this.pending_mutes.borrow_mut().retain(|chat_id, intent| {
                        this.chatlist
                            .summary(*chat_id)
                            .is_none_or(|summary| summary.muted != *intent)
                    });
                    if let Some(chat_id) = this.info.chat_id() {
                        let cached = this.chatlist.summary(chat_id).map(|summary| summary.muted);
                        let muted = effective_mute(
                            this.pending_mutes.borrow().get(&chat_id).copied(),
                            cached,
                        );
                        this.info.set_notifications(!muted);
                    }
                    let mut event_messages: Vec<Msg> =
                        this.last_by_chat.borrow().values().cloned().collect();
                    event_messages.sort_by_key(|message| message.ts);
                    for message in &event_messages {
                        match dialog_times.get(&message.chat_id).copied().flatten() {
                            None => this.chatlist.upsert(
                                message.chat_id,
                                &chat_title(message),
                                &message_preview(message),
                                Some(message.ts),
                                UnreadUpdate::Delta(0),
                            ),
                            Some(dialog_time) if message.ts > dialog_time => this.chatlist.upsert(
                                message.chat_id,
                                &chat_title(message),
                                &message_preview(message),
                                Some(message.ts),
                                UnreadUpdate::Delta(0),
                            ),
                            Some(dialog_time) if message.ts == dialog_time => this.chatlist.update(
                                message.chat_id,
                                &chat_title(message),
                                &message_preview(message),
                                Some(message.ts),
                                UnreadUpdate::Delta(0),
                            ),
                            Some(_) => {}
                        }
                    }
                    if let Some(chat_id) = this.open_chat.get()
                        && this.window_is_active() && this.chatlist.unread(chat_id) > 0 {
                            let latest = this
                                .messages
                                .last_message()
                                .map(|message| message.id)
                                .unwrap_or(0);
                            this.queue_mark_read(chat_id, latest, this.epoch.get());
                        }
                    this.dialogs_loaded.set(true);
                    this.dialogs_error_box.set_visible(false);
                    this.run_smoke_hooks();
                }
                Err(error) => {
                    shell_log!("get_dialogs: {error}");
                    this.dialogs_error.remove_css_class("omg-muted");
                    this.dialogs_error.add_css_class("omg-error");
                    this.dialogs_error.set_label(&format!("Could not load your chats. Check your connection and try again.\n{error}"));
                    this.dialogs_retry.set_visible(true);
                    this.dialogs_error_box.set_visible(true);
                }
            }
            if requested_revision != this.dialogs_revision.get() {
                this.dialogs_refresh_again.set(true);
            }
            if this.dialogs_refresh_again.replace(false) && this.session_ready.get() {
                this.load_dialogs();
            }
        });
    }

    fn schedule_dialogs_reload(self: &Rc<Self>) {
        if self.dialogs_reload_timeout.borrow().is_some() {
            return;
        }
        let weak = Rc::downgrade(self);
        let source = glib::timeout_add_local_once(Duration::from_millis(300), move || {
            let Some(this) = weak.upgrade() else { return };
            this.dialogs_reload_timeout.borrow_mut().take();
            this.load_dialogs();
        });
        *self.dialogs_reload_timeout.borrow_mut() = Some(source);
    }

    fn bump_dialogs_revision(&self) {
        self.dialogs_revision
            .set(self.dialogs_revision.get().wrapping_add(1));
    }

    fn begin_chat_mutation(&self) {
        self.mutations_in_flight
            .set(self.mutations_in_flight.get().saturating_add(1));
    }

    fn finish_chat_mutation(&self) {
        self.mutations_in_flight
            .set(self.mutations_in_flight.get().saturating_sub(1));
    }

    fn dialog_upsert(
        &self,
        chat_id: i64,
        title: &str,
        preview: &str,
        time: Option<DateTime<Local>>,
        unread: UnreadUpdate,
    ) {
        self.bump_dialogs_revision();
        self.chatlist
            .upsert(dialog_id(chat_id), title, preview, time, unread);
    }

    fn run_smoke_hooks(self: &Rc<Self>) {
        if self.probe || self.smoke_hook_done.replace(true) {
            return;
        }
        if let Ok(title) = std::env::var("OMG_SMOKE_OPEN")
            && let Some(chat_id) = self
                .chatlist
                .ordered()
                .into_iter()
                .find_map(|(id, candidate)| (candidate == title).then_some(id))
            {
                self.clone().open_chat(chat_id);
            }
        // OMG_SMOKE_OPEN_SEQUENCE="Title@ms,Title@ms": open chats at those
        // offsets (demo recordings: exercises the chat-switch animations).
        if let Ok(sequence) = std::env::var("OMG_SMOKE_OPEN_SEQUENCE") {
            for entry in sequence.split(',') {
                let Some((title, ms)) = entry.rsplit_once('@') else { continue };
                let Ok(ms) = ms.trim().parse::<u64>() else { continue };
                let title = title.trim().to_string();
                let this = self.clone();
                glib::timeout_add_local_once(Duration::from_millis(ms), move || {
                    if let Some(chat_id) = this
                        .chatlist
                        .ordered()
                        .into_iter()
                        .find_map(|(id, candidate)| (candidate == title).then_some(id))
                    {
                        this.clone().open_chat(chat_id);
                    }
                });
            }
        }
        if std::env::var_os("OMG_SMOKE_EXPAND_PIN").is_some() {
            let this = self.clone();
            glib::MainContext::default().spawn_local(async move {
                if poll_until(8_000, || this.messages.probe_pinned_metrics().0).await {
                    this.messages.trigger_pinned_expand();
                }
            });
        }
        if std::env::var_os("OMG_SMOKE_CHAT_SEARCH").is_some() { self.messages.open_search(); }
        if let Ok(query) = std::env::var("OMG_SMOKE_SEARCH") {
            self.chatlist.set_search_text(&query);
        }
        if let Some(call) = std::env::var("OMG_SMOKE_CALL")
            .ok()
            .filter(|value| !value.is_empty())
        {
            match call.as_str() {
                "out" => {
                    let weak = Rc::downgrade(self);
                    glib::MainContext::default().spawn_local(async move {
                        let Some(this) = weak.upgrade() else { return };
                        let marta = this
                            .chatlist
                            .ordered()
                            .into_iter()
                            .find_map(|(id, title)| (title == "Marta").then_some(id));
                        let Some(marta) = marta else { return };
                        if this.open_chat.get() != Some(marta) {
                            this.clone().open_chat(marta);
                        }
                        if poll_until(8_000, || {
                            this.open_chat.get() == Some(marta) && !this.messages.is_loading()
                        })
                        .await
                        {
                            this.start_call(marta);
                        }
                    });
                }
                // The mock's OMG_MOCK_INCOMING_CALL fixture emits the event;
                // the regular event loop opens this same surface.
                "in" => {}
                other => eprintln!("OMG_SMOKE_CALL: unknown value {other}"),
            }
        }
        if std::env::var("OMG_SMOKE_MENU").is_ok_and(|value| !value.is_empty()) {
            let weak = Rc::downgrade(self);
            let Some(window) = self.window() else { return };
            window.add_tick_callback(move |_, _| {
                let weak = weak.clone();
                glib::idle_add_local_once(move || {
                    if let Some(this) = weak.upgrade() {
                        this.open_main_menu_with_autohide(false);
                    }
                });
                glib::ControlFlow::Break
            });
        }
        // Screenshot hook (spec §4.5): open one of the 6C surfaces once the
        // chat named by OMG_SMOKE_OPEN is on screen.
        if let Some(which) = std::env::var("OMG_SMOKE_ATTACH")
            .ok()
            .filter(|value| !value.is_empty())
        {
            let weak = Rc::downgrade(self);
            glib::MainContext::default().spawn_local(async move {
                let ready = poll_until(8_000, || {
                    weak.upgrade().is_some_and(|this| {
                        this.open_chat.get().is_some_and(|id| !is_virtual(id))
                            && !this.messages.is_loading()
                    })
                })
                .await;
                let Some(this) = weak.upgrade().filter(|_| ready) else {
                    return;
                };
                match which.as_str() {
                    "poll" => this.open_poll_dialog(),
                    "location" => this.open_location_dialog(),
                    "later" => {
                        // The normal empty composer shows MIC, so seed benign
                        // text before exercising the actual send-later path.
                        if this.messages.composer_text().trim().is_empty() {
                            this.messages.set_composer_text("Scheduled message");
                        }
                        this.messages.show_send_later();
                    }
                    "menu" => this.messages.show_attach_menu(),
                    other => eprintln!("OMG_SMOKE_ATTACH: unknown value {other}"),
                }
            });
        }
        // Screenshot hook: OMG_SMOKE_SCROLL=<message id> scrolls the opened
        // chat to that row (bin/shot cannot scroll; the UI review needed the
        // cards above the fold).
        if let Some(target) = std::env::var("OMG_SMOKE_SCROLL")
            .ok()
            .and_then(|value| value.trim().parse::<i32>().ok())
        {
            let weak = Rc::downgrade(self);
            glib::MainContext::default().spawn_local(async move {
                let ready = poll_until(8_000, || {
                    weak.upgrade().is_some_and(|this| {
                        this.open_chat.get().is_some_and(|id| !is_virtual(id))
                            && !this.messages.is_loading()
                            && this.messages.contains(target)
                    })
                })
                .await;
                if let Some(this) = weak.upgrade().filter(|_| ready) {
                    glib::timeout_future(Duration::from_millis(300)).await;
                    this.messages.scroll_to_message(target);
                }
            });
        }
        if let Some(msg_id) = std::env::var("OMG_SMOKE_FORWARD")
            .ok()
            .and_then(|value| value.parse::<i32>().ok())
        {
            let weak = Rc::downgrade(self);
            glib::MainContext::default().spawn_local(async move {
                let ready = poll_until(8_000, || {
                    weak.upgrade()
                        .is_some_and(|this| this.messages.contains(msg_id))
                })
                .await;
                if ready
                    && let Some(this) = weak.upgrade() {
                        this.open_forward(vec![msg_id]);
                    }
            });
        }
        if let Some(msg_id) = std::env::var("OMG_SMOKE_VIEWER")
            .ok()
            .and_then(|value| value.parse::<i32>().ok())
        {
            let weak = Rc::downgrade(self);
            glib::MainContext::default().spawn_local(async move {
                let ready = poll_until(8_000, || {
                    weak.upgrade().is_some_and(|this| {
                        matches!(this.messages.media_state(msg_id), Some(MediaState::Done(_)))
                    })
                })
                .await;
                if ready
                    && let Some(this) = weak.upgrade() {
                        this.open_viewer(msg_id);
                    }
            });
        }
        if std::env::var("OMG_SMOKE_INFO").is_ok_and(|value| !value.is_empty()) {
            let weak = Rc::downgrade(self);
            glib::MainContext::default().spawn_local(async move {
                let Some(this) = weak.upgrade() else { return };
                let chat_id = this
                    .chatlist
                    .ordered()
                    .into_iter()
                    .find_map(|(id, title)| (title == "Arch Linux ARM").then_some(id));
                let Some(chat_id) = chat_id else { return };
                this.clone().open_chat(chat_id);
                if !this.ui_state.borrow().info_panel_open {
                    this.toggle_info_panel();
                }
                let _ = poll_until(8_000, || {
                    this.info.is_bound(chat_id)
                        && this.info.members_count() > 0
                        && this.info.shared_state_text() != "Loading shared media…"
                })
                .await;
            });
        }
        if std::env::var("OMG_SMOKE_STICKERS").is_ok_and(|value| !value.is_empty()) {
            let weak = Rc::downgrade(self);
            glib::MainContext::default().spawn_local(async move {
                let Some(this) = weak.upgrade() else { return };
                if this.open_chat.get().is_none()
                    && let Some(chat_id) = this
                        .chatlist
                        .ordered()
                        .into_iter()
                        .find_map(|(id, title)| (title == "Marta").then_some(id))
                    {
                        this.clone().open_chat(chat_id);
                    }
                if poll_until(8_000, || !this.messages.is_loading()).await {
                    this.open_stickers();
                }
            });
        }
        if std::env::var("OMG_SMOKE_RECORDING").is_ok_and(|value| !value.is_empty() && value != "video") {
            let weak = Rc::downgrade(self);
            glib::MainContext::default().spawn_local(async move {
                let Some(this) = weak.upgrade() else { return };
                if this.open_chat.get().is_none()
                    && let Some(chat_id) = this
                        .chatlist
                        .ordered()
                        .into_iter()
                        .find_map(|(id, title)| (title == "Marta").then_some(id))
                    {
                        this.clone().open_chat(chat_id);
                    }
                if poll_until(8_000, || !this.messages.is_loading()).await {
                    this.start_recording();
                }
            });
        }
        if std::env::var("OMG_SMOKE_RECORD").is_ok_and(|value| value == "video")
            || std::env::var("OMG_SMOKE_RECORDING").is_ok_and(|value| value == "video")
        {
            let weak = Rc::downgrade(self);
            glib::MainContext::default().spawn_local(async move {
                let Some(this) = weak.upgrade() else { return };
                if this.open_chat.get().is_none()
                    && let Some(chat_id) = this
                        .chatlist
                        .ordered()
                        .into_iter()
                        .find_map(|(id, title)| (title == "Marta").then_some(id))
                    {
                        this.clone().open_chat(chat_id);
                    }
                if poll_until(8_000, || !this.messages.is_loading()).await {
                    this.start_video_note();
                }
            });
        }
        if let Ok(target) = std::env::var("OMG_SMOKE_STORIES") {
            let weak = Rc::downgrade(self);
            glib::MainContext::default().spawn_local(async move {
                let Some(this) = weak.upgrade() else { return };
                if poll_until(8_000, || this.stories_strip.peer_count() > 0).await {
                    let chat_id = target.parse::<i64>().unwrap_or(1);
                    this.open_stories_for_peer(chat_id);
                }
            });
        }
        if std::env::var("OMG_SMOKE_CONTACTS").is_ok_and(|value| !value.is_empty()) {
            self.open_contacts();
        }
    }

    fn install_window_hook(self: &Rc<Self>) {
        if self.window_hooked.replace(true) {
            return;
        }
        let Some(window) = self.window() else {
            self.window_hooked.set(false);
            let weak = Rc::downgrade(self);
            glib::idle_add_local_once(move || {
                if let Some(this) = weak.upgrade() {
                    this.install_window_hook();
                }
            });
            return;
        };
        let weak = Rc::downgrade(self);
        window.connect_is_active_notify(move |window| {
            let Some(this) = weak.upgrade() else { return };
            if window.is_active() { this.presence_last_activity.set(std::time::Instant::now()); }
            this.update_online_status();
            this.effects
                .window_focus(window.upcast_ref(), window.is_active());
            if !window.is_active() {
                return;
            }
            let Some(chat_id) = this.open_chat.get() else {
                return;
            };
            if is_virtual(chat_id) {
                return;
            }
            if this.chatlist.unread(chat_id) > 0 {
                let latest = this.messages.last_message().map(|msg| msg.id).unwrap_or(0);
                let epoch = this.epoch.get();
                this.queue_mark_read(chat_id, latest, epoch);
            }
        });
        let weak = Rc::downgrade(self);
        window.connect_visible_notify(move |_| {
            if let Some(this) = weak.upgrade() { this.update_online_status(); }
        });
        let activity = gtk::EventControllerLegacy::new();
        activity.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(self);
        activity.connect_event(move |_, event| {
            if matches!(event.event_type(), gtk::gdk::EventType::KeyPress | gtk::gdk::EventType::ButtonPress
                | gtk::gdk::EventType::TouchBegin | gtk::gdk::EventType::Scroll | gtk::gdk::EventType::MotionNotify)
                && let Some(this) = weak.upgrade() {
                this.presence_last_activity.set(std::time::Instant::now());
                this.update_online_status();
            }
            glib::Propagation::Proceed
        });
        window.add_controller(activity);
        let weak = Rc::downgrade(self);
        glib::timeout_add_seconds_local(5, move || {
            let Some(this) = weak.upgrade() else { return glib::ControlFlow::Break };
            this.update_online_status();
            glib::ControlFlow::Continue
        });
    }

    fn update_online_status(self: &Rc<Self>) {
        if !self.session_ready.get() { return; }
        let Some(window) = self.window() else { return };
        let online = should_report_online(window.is_visible(), window.is_active(), self.presence_last_activity.get().elapsed());
        if self.presence_online.replace(Some(online)) == Some(online) { return; }
        let weak = Rc::downgrade(self);
        let tg = self.tg.clone();
        let session_epoch = self.session_epoch.get();
        glib::MainContext::default().spawn_local(async move {
            if tg.set_online(online).await.is_err()
                && let Some(this) = weak.upgrade().filter(|this| this.is_session_current(session_epoch))
                && this.presence_online.get() == Some(online) {
                // Retry on the next activity/timer tick, without a tight loop.
                this.presence_online.set(None);
            }
        });
    }

    fn install_theme_switch_hook(self: &Rc<Self>) {
        if self.theme_monitor.borrow().is_some() {
            return;
        }
        let state_dir = dirs::state_dir()
            .or_else(|| dirs::home_dir().map(|home| home.join(".local").join("state")));
        let Some(path) =
            state_dir.map(|state| state.join("omarchy").join("current").join("theme.name"))
        else {
            return;
        };
        if path.parent().is_none_or(|parent| !parent.exists()) {
            return;
        }
        let Ok(monitor) = gio::File::for_path(path)
            .monitor_file(gio::FileMonitorFlags::NONE, gio::Cancellable::NONE)
        else {
            return;
        };
        let weak = Rc::downgrade(self);
        monitor.connect_changed(move |_, _, _, _| {
            let Some(this) = weak.upgrade() else { return };
            if let Some(source) = this.theme_switch_timeout.borrow_mut().take() {
                source.remove();
            }
            let weak = Rc::downgrade(&this);
            let source = glib::timeout_add_local_once(Duration::from_millis(220), move || {
                let Some(this) = weak.upgrade() else { return };
                this.theme_switch_timeout.borrow_mut().take();
                this.messages.refresh_theme();
                this.effects.theme_switched(&this.overlay);
            });
            *this.theme_switch_timeout.borrow_mut() = Some(source);
        });
        *self.theme_monitor.borrow_mut() = Some(monitor);
    }

    fn handle_event(self: &Rc<Self>, event: Event) {
        if !self.session_ready.get() {
            return;
        }
        match event {
            Event::CallChanged(info) => self.handle_call_changed(info),
            Event::ReadOutbox { chat_id, max_id } => {
                self.bump_dialogs_revision();
                let mut read = self.read_outbox.borrow_mut();
                let known = read.entry(chat_id).or_insert(max_id);
                *known = (*known).max(max_id);
                let known = *known;
                drop(read);
                self.chatlist.set_read_outbox(chat_id, known);
                if self.open_chat_is(chat_id) {
                    self.messages.set_read_outbox(known);
                }
            }
            Event::ReadInbox { chat_id, max_id: _ } => {
                self.bump_dialogs_revision();
                if !self.manual_unread_hold.borrow().contains(&chat_id) {
                    self.chatlist.clear_unread(chat_id);
                }
            }
            Event::Presence { user_id, presence } => {
                self.chatlist.set_presence(user_id, presence);
                if self.open_chat.get() == Some(user_id) {
                    self.messages.set_presence(presence);
                }
            }
            Event::DialogsChanged => {
                self.bump_dialogs_revision();
                self.schedule_dialogs_reload();
            }
            Event::PinnedChanged { chat_id } => {
                if let Some(open) = self.open_chat.get().filter(|_| self.open_chat_is(chat_id)) {
                    self.load_pinned(open);
                }
            }
            // Wave 6 events: handled by the 6B/6C/6E/6F packages (specs/spec-wave6.md).
            Event::PollChanged { poll_id, poll } => {
                self.messages.update_poll(poll_id, poll);
            }
            Event::StoriesChanged => {
                self.reload_stories();
                // Rings update directly when this request completes. Story
                // notifications must not invalidate an in-flight chat load.
            }
            Event::ScheduledChanged { chat_id } => {
                if self.open_chat_is(chat_id)
                    && let Some(open_chat_id) = self.open_chat.get() {
                        self.clone().refresh_scheduled(open_chat_id);
                    }
            }
            Event::TopicsChanged { forum_id } => {
                if self.forum_list_open.get() && self.open_chat.get() == Some(forum_id) {
                    self.schedule_forum_topics_refresh(forum_id);
                }
            }
            Event::NewMessage(message) => self.handle_new_message(message),
            Event::MessageChanged(message) => {
                let message_key = (message.chat_id, message.id);
                let change = self
                    .message_change_generations
                    .borrow()
                    .get(&message_key)
                    .copied()
                    .unwrap_or(0)
                    .wrapping_add(1);
                self.message_change_generations
                    .borrow_mut()
                    .insert(message_key, change);
                let message = self.apply_tombstone(message);
                self.check_own_live_message(&message);
                let is_open = self
                    .open_chat
                    .get()
                    .is_some_and(|id| msg_in_chat(&message, id));
                if is_open && !self.forum_list_open.get() {
                    let was_last = self.messages.is_last(message.id);
                    let inserted = self.messages.merge_event(message.clone());
                    self.post_render(inserted);
                    if was_last && !message.deleted {
                        self.remember_last(&message);
                        self.dialog_upsert(
                            message.chat_id,
                            &chat_title(&message),
                            &message_preview(&message),
                            Some(message.ts),
                            UnreadUpdate::Delta(0),
                        );
                    }
                }
            }
            Event::MessageDeleted { chat_id, msg_ids } => {
                // A20: bookkeeping changes before viewer/row animation or
                // tombstone rendering touches any selected row.
                self.messages.drop_selection_ids(&msg_ids);
                let viewer_closed = self.viewer.remove_deleted(chat_id, &msg_ids);
                if viewer_closed {
                    self.close_viewer();
                    self.close_profile();
                }
                let is_open = self.open_chat_is(chat_id);
                let anti_delete = self.settings.get().anti_delete;
                let tracked_last = self
                    .last_by_chat
                    .borrow()
                    .get(&chat_id)
                    .map(|message| message.id);
                let tracked_was_deleted = tracked_last
                    .is_some_and(|tracked| msg_ids.contains(&tracked));

                if anti_delete {
                    self.tombstones
                        .borrow_mut()
                        .entry(chat_id)
                        .or_default()
                        .extend(msg_ids.iter().copied());
                    if is_open {
                        for msg_id in &msg_ids {
                            self.messages.mark_deleted(*msg_id);
                        }
                    }
                } else if is_open {
                    // Anti-delete off: remove rows and retain the established
                    // last-message/sidebar reconciliation behavior.
                    let title = self.title_for(chat_id);
                    let store_last_was_deleted = self
                        .messages
                        .last_message()
                        .is_some_and(|message| msg_ids.contains(&message.id));
                    let next_last = self.messages.last_excluding(&msg_ids);
                    let epoch = self.epoch.get();
                    for msg_id in &msg_ids {
                        if self.messages.animate_deleted(*msg_id) {
                            let this = self.clone();
                            let msg_id = *msg_id;
                            glib::timeout_add_local_once(Duration::from_millis(500), move || {
                                if this.is_current(chat_id, epoch) {
                                    this.messages.remove(msg_id);
                                }
                            });
                        } else {
                            self.messages.remove(*msg_id);
                        }
                    }
                    if tracked_was_deleted || store_last_was_deleted {
                        let reconciled = if let Some(deleted_id) =
                            tracked_last.filter(|tracked| msg_ids.contains(tracked))
                        {
                            self.reconcile_deleted_last(chat_id, deleted_id, next_last.clone())
                        } else {
                            let mut last_by_chat = self.last_by_chat.borrow_mut();
                            if let Some(message) = next_last {
                                last_by_chat.insert(chat_id, message.clone());
                                Some(message)
                            } else {
                                last_by_chat.remove(&chat_id);
                                None
                            }
                        };
                        let (preview, time) = reconciled
                            .as_ref()
                            .map(|message| (message_preview(message), Some(message.ts)))
                            .unwrap_or_else(|| (String::new(), None));
                        self.dialog_upsert(chat_id, &title, &preview, time, UnreadUpdate::Delta(0));
                    }
                }

                // A closed chat has no complete message store to reconcile.
                // Drop the stale tracked preview and let dialogs provide the
                // authoritative new last message, with refreshes coalesced.
                if !is_open && tracked_was_deleted {
                    self.last_by_chat.borrow_mut().remove(&chat_id);
                    self.load_dialogs();
                }
            }
            Event::Typing { chat_id, name } => {
                if self.open_chat.get() != Some(chat_id) {
                    return;
                }
                if let Some(source) = self.typing_timeout.borrow_mut().take()
                    && let Some(live) = glib::MainContext::default().find_source_by_id(&source) {
                        live.destroy();
                    }
                let generation = self.messages.set_typing(&name);
                let epoch = self.epoch.get();
                let weak = Rc::downgrade(self);
                let source = glib::timeout_add_local_once(Duration::from_secs(5), move || {
                    if let Some(this) = weak.upgrade()
                        && this.is_current(chat_id, epoch) {
                            this.typing_timeout.borrow_mut().take();
                            this.messages.clear_typing_if(generation);
                        }
                });
                *self.typing_timeout.borrow_mut() = Some(source);
            }
        }
    }

    fn handle_new_message(self: &Rc<Self>, message: Msg) {
        let message = self.apply_tombstone(message);
        self.check_own_live_message(&message);
        let active = self.window_is_active();
        let is_open = self
            .open_chat
            .get()
            .is_some_and(|id| msg_in_chat(&message, id));
        let forum_list = self.forum_list_open.get();
        let show_in_messages = is_open && !forum_list;
        let manual_unread = self.manual_unread_hold.borrow().contains(&message.chat_id);
        let read_triggered = show_in_messages && active && !manual_unread;
        // Outgoing = sent from the user's own other device: show it (the store
        // dedupes against local sends by id), but never notify or count unread.
        let own = message.outgoing;
        if !own && !message.deleted {
            self.recent_incoming
                .borrow_mut()
                .insert(message.chat_id, message.ts);
        }
        if !message.deleted {
            self.remember_last(&message);
            self.dialog_upsert(
                message.chat_id,
                &chat_title(&message),
                &message_preview(&message),
                Some(message.ts),
                if own || read_triggered {
                    UnreadUpdate::Delta(0)
                } else {
                    UnreadUpdate::Delta(1)
                },
            );
        }
        if forum_list && is_open {
            // A topic got a new message while its list is open: bump the unread
            // in the list (the mock also emits TopicsChanged, which refetches).
            self.schedule_forum_topics_refresh(message.chat_id);
        }
        if show_in_messages {
            let inserted = self.messages.merge_event(message.clone());
            self.post_render(inserted);
            if !own && !message.deleted {
                self.messages.mark_recent_incoming();
            }
            if read_triggered && !own && !message.deleted {
                let read_chat = self.open_chat.get().unwrap_or(message.chat_id);
                self.queue_mark_read(read_chat, message.id, self.epoch.get());
            }
        }
        if !own && !message.deleted && !is_open {
            self.chatlist.mention(message.chat_id);
        }
        if !own && !message.deleted && (!active || !is_open) {
            self.notify(&message);
        }
    }

    fn notify(self: &Rc<Self>, message: &Msg) {
        let muted = effective_mute(
            self.pending_mutes.borrow().get(&message.chat_id).copied(),
            self.chatlist
                .summary(message.chat_id)
                .map(|chat| chat.muted),
        );
        if muted {
            return;
        }
        let Some(window) = self.window() else { return };
        let Some(application) = window.application() else {
            return;
        };
        let title = if message.chat_title.trim().is_empty() {
            if message.sender.trim().is_empty() {
                "Unknown"
            } else {
                &message.sender
            }
        } else {
            &message.chat_title
        };
        // Escape: mako/dunst render Pango markup in notification bodies, so
        // raw remote text could spoof or break the notification.
        let title = glib::markup_escape_text(title);
        let mut body = message_preview(message);
        if body.chars().count() > 200 {
            body = body.chars().take(200).collect::<String>() + "…";
        }
        let body = glib::markup_escape_text(&body);
        let notification = gio::Notification::new(title.as_str());
        notification.set_body(Some(body.as_str()));
        let chat_id = message.chat_id;
        let avatar_peer = dialog_id(chat_id);
        let sequence = self.notification_sequence.get().wrapping_add(1);
        self.notification_sequence.set(sequence);
        {
            let mut pending = self.pending_notifications.borrow_mut();
            // Only pending avatar lookups live here, never a message archive.
            if pending.len() >= 256
                && let Some(oldest) = pending.iter().min_by_key(|(_, seq)| **seq).map(|(id, _)| *id) {
                pending.remove(&oldest);
            }
            pending.insert(chat_id, sequence);
        }
        let tg = self.tg.clone();
        let session = self.session_epoch.get();
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            // Cached photos complete quickly. A slow network must not hold
            // the notification indefinitely or cause a second popup later.
            let photo = glib::future_with_timeout(Duration::from_millis(800), tg.download_avatar(avatar_peer))
                .await.ok().and_then(Result::ok).flatten();
            let Some(this) = weak.upgrade() else { return };
            if this.pending_notifications.borrow().get(&chat_id) != Some(&sequence) { return; }
            this.pending_notifications.borrow_mut().remove(&chat_id);
            if !this.is_session_current(session)
                || (this.window_is_active() && this.open_chat.get().is_some_and(|id| dialog_id(id) == avatar_peer))
                || effective_mute(this.pending_mutes.borrow().get(&chat_id).copied(),
                    this.chatlist.summary(avatar_peer).map(|chat| chat.muted)) { return; }
            if let Some(path) = photo {
                notification.set_icon(&gio::FileIcon::new(&gio::File::for_path(&path)));
                if this.probe { *this.probe_notification_avatar.borrow_mut() = Some((avatar_peer, path)); }
            } else {
                notification.set_icon(&gio::ThemedIcon::new("omarchygram"));
                if this.probe { this.probe_notification_avatar.borrow_mut().take(); }
            }
            if !this.probe {
                application.send_notification(Some(&format!("chat-{chat_id}")), &notification);
            }
            this.probe_notifications.set(this.probe_notifications.get().wrapping_add(1));
        });
    }

    fn open_chat(self: Rc<Self>, chat_id: i64) {
        if !self.session_ready.get() {
            return;
        }
        self.pending_notifications.borrow_mut().retain(|id, _| dialog_id(*id) != dialog_id(chat_id));
        // A chat activation owns the main view even when it re-opens the
        // already-selected chat; dismiss settings capture and modal windows.
        self.close_settings();
        self.switcher.close();
        self.close_forward();
        self.close_viewer();
        self.close_profile();
        self.close_contacts();
        self.close_new_group();
        self.close_stickers();
        self.close_caption_dialog();
        self.close_in_chat_search();
        self.reaction_retry.borrow_mut().take();
        if self.open_chat.get() == Some(chat_id) {
            return;
        }
        self.cancel_recording();
        self.main_menu.dismiss();
        self.chatlist.dismiss_popovers();
        self.messages.dismiss_owned_popovers();
        // §2.3: nothing keeps playing once the window is gone.
        self.messages.reset_players();
        if let Some(previous) = self.open_chat.get().filter(|id| !is_virtual(*id)) {
            self.manual_unread_hold.borrow_mut().remove(&previous);
            self.capture_current_draft(previous);
            self.flush_draft(previous);
        }
        if is_virtual(chat_id) {
            self.open_virtual_chat(chat_id);
            return;
        }
        let epoch = self.bump_epoch();
        self.open_chat.set(Some(chat_id));

        // A forum supergroup row shows its topic list instead of a history
        // (§6.3); a synthetic topic id opens like a normal chat, but the
        // chat-list selection stays on the forum row.
        let topic = split_topic_chat_id(chat_id);
        if topic.is_none()
            && let Some(summary) = self.chatlist.summary(chat_id).filter(|s| s.forum) {
                self.open_forum(chat_id, &summary, epoch);
                return;
            }
        self.forum_list_open.set(false);
        self.content_area.set_visible_child_name("messages");
        self.chatlist.select_chat(dialog_id(chat_id));
        {
            let mut recent = self.recent_real_chats.borrow_mut();
            recent.retain(|id| *id != chat_id);
            recent.insert(0, chat_id);
        }
        let title = self.title_for(chat_id);
        self.messages.reset_chat(chat_id, &title, epoch);
        if let Some(mut summary) = self.chatlist.summary(dialog_id(chat_id)) {
            if let Some((forum_id, topic_id)) = topic {
                summary.id = chat_id;
                if let Some(topic) = self.forum_topics.borrow().iter().find(|t| t.chat_id == chat_id) {
                    summary.title = topic.title.clone(); summary.muted = topic.muted; summary.pinned = topic.pinned;
                    summary.read_outbox_max_id = topic.read_outbox_max_id; summary.draft = topic.draft.clone();
                }
                self.set_topic_breadcrumb(forum_id, topic_id, epoch);
            }
            self.messages.set_chat_summary(&summary, &self.tg);
            let known = self
                .read_outbox
                .borrow()
                .get(&chat_id)
                .copied()
                .unwrap_or(summary.read_outbox_max_id);
            self.messages.set_read_outbox(known);
        } else if let Some((forum_id, topic_id)) = topic {
            self.set_topic_breadcrumb(forum_id, topic_id, epoch);
        }
        self.restore_draft(chat_id);
        if let Some(text) = self.pending_inline_query.borrow_mut().take() {
            self.messages.set_composer_text(&text);
            self.messages.focus_composer();
        }
        self.bind_info_panel();
        // Start history before optional info, pins and scheduled-message RPCs.
        self.clone().start_initial_load(chat_id, epoch);
        // A topic has no chat info of its own; its bot commands, members and
        // presence are the forum's, which the topic list already showed.
        if topic.is_none() {
            self.load_chat_info(chat_id, epoch);
        }
        self.load_pinned(chat_id);
        let recent = self
            .recent_incoming
            .borrow()
            .get(&chat_id)
            .is_some_and(|time| Local::now().signed_duration_since(*time).num_minutes() < 5);
        if recent {
            self.messages.mark_recent_incoming();
        }
        // Spec §4.4: the strip belongs to the chat, so every open refetches.
        self.refresh_scheduled(chat_id);
    }

    /// Show a forum's topic list in place of the message pane.
    fn open_forum(self: &Rc<Self>, forum_id: i64, summary: &ChatSummary, epoch: u64) {
        self.forum_list_open.set(true);
        self.chatlist.select_chat(forum_id);
        {
            let mut recent = self.recent_real_chats.borrow_mut();
            recent.retain(|id| *id != forum_id);
            recent.insert(0, forum_id);
        }
        self.messages.reset_chat(forum_id, &summary.title, epoch);
        self.topics.set_forum(&summary.title);
        self.topics.clear();
        self.content_area.set_visible_child_name("topics");
        self.bind_info_panel();
        self.load_forum_topics(forum_id, epoch);
    }

    fn load_forum_topics(self: &Rc<Self>, forum_id: i64, epoch: u64) {
        let generation = self.forum_topics_generation.get().wrapping_add(1);
        self.forum_topics_generation.set(generation);
        let tg = self.tg.clone();
        let session_epoch = self.session_epoch.get();
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let result = tg.get_topics(forum_id).await;
            let Some(this) = weak.upgrade().filter(|this| {
                this.session_ready.get() && this.session_epoch.get() == session_epoch
            }) else {
                return;
            };
            if this.forum_topics_generation.get() != generation {
                return;
            }
            match result {
                Ok(topics) => {
                    this.apply_forum_topics(forum_id, epoch, generation, topics);
                }
                Err(error) => {
                    shell_log!("get_topics({forum_id}): {error}");
                    if this.is_current(forum_id, epoch) && this.forum_list_open.get() {
                        this.messages.show_error(&error);
                    }
                }
            }
        });
    }

    /// Apply only the newest forum load. Keeping this check next to the UI
    /// mutation also makes refresh ordering directly assertable by the probe.
    fn apply_forum_topics(
        &self,
        forum_id: i64,
        epoch: u64,
        generation: u64,
        topics: Vec<Topic>,
    ) -> bool {
        if self.forum_topics_generation.get() != generation {
            return false;
        }
        *self.forum_topics.borrow_mut() = topics.clone();
        if self.is_current(forum_id, epoch) && self.forum_list_open.get() {
            self.topics.set_topics(topics);
        }
        true
    }

    /// `TopicsChanged`, or a new message in a topic of the open forum.
    fn schedule_forum_topics_refresh(self: &Rc<Self>, forum_id: i64) {
        if !self.forum_list_open.get() || self.open_chat.get() != Some(forum_id) {
            return;
        }
        self.load_forum_topics(forum_id, self.epoch.get());
    }

    /// Create a topic from the new-topic dialog, then open it (§6.3).
    fn create_topic_from_dialog(self: Rc<Self>, title: String) {
        let Some(forum_id) = self.open_chat.get().filter(|_| self.forum_list_open.get()) else {
            return;
        };
        let epoch = self.epoch.get();
        let session_epoch = self.session_epoch.get();
        glib::MainContext::default().spawn_local(async move {
            match self.tg.create_topic(forum_id, &title).await {
                Ok(topic) => {
                    self.topics.finish_create();
                    if !self.is_session_current(session_epoch) {
                        return;
                    }
                    // The backend also emits TopicsChanged; refetch so the new
                    // row carries the backend's ordering and counts.
                    self.load_forum_topics(forum_id, epoch);
                    if self.is_current(forum_id, epoch) && self.forum_list_open.get() {
                        self.clone().open_chat(topic.chat_id);
                    }
                }
                Err(error) => {
                    shell_log!("create_topic({forum_id}): {error}");
                    if self.is_session_current(session_epoch)
                        && self.is_current(forum_id, epoch)
                        && self.forum_list_open.get()
                    {
                        self.topics.show_create_error(&error);
                    } else {
                        // Completion always releases the pending UI state,
                        // even after a chat/session generation made the error
                        // stale and therefore ineligible for display.
                        self.topics.finish_create_pending();
                    }
                }
            }
        });
    }

    /// Header breadcrumb "Forum › Topic" plus the back button (§6.3). The
    /// topic list is usually already loaded; when the topic was opened
    /// directly (a restored session) its title is fetched.
    fn set_topic_breadcrumb(self: &Rc<Self>, forum_id: i64, topic_id: i32, epoch: u64) {
        let forum_title = self.title_for(forum_id);
        let topic_title = self
            .forum_topics
            .borrow()
            .iter()
            .find(|topic| topic.forum_id == forum_id && topic.id == topic_id)
            .map(|topic| topic.title.clone());
        match topic_title {
            Some(topic_title) => self
                .messages
                .set_topic_header(&format!("{forum_title} › {topic_title}")),
            None => {
                self.messages.set_topic_header(&forum_title);
                let chat_id = topic_chat_id(forum_id, topic_id);
                let tg = self.tg.clone();
                let weak = Rc::downgrade(self);
                glib::MainContext::default().spawn_local(async move {
                    let Ok(topics) = tg.get_topics(forum_id).await else {
                        return;
                    };
                    let Some(this) = weak.upgrade() else {
                        return;
                    };
                    if !this.is_current(chat_id, epoch) {
                        return;
                    }
                    if let Some(topic) = topics.iter().find(|topic| topic.id == topic_id) {
                        this.messages
                            .set_topic_header(&format!("{forum_title} › {}", topic.title));
                    }
                    *this.forum_topics.borrow_mut() = topics;
                    this.restore_draft(chat_id);
                });
            }
        }
    }

    fn open_virtual_chat(self: Rc<Self>, chat_id: i64) {
        if !self.session_ready.get() {
            return;
        }
        let settings = self.settings.get();
        if (chat_id == ASSISTANT_CHAT && !settings.ai.enabled)
            || (chat_id == OMARCHY_CHAT && !settings.os.enabled)
        {
            return;
        }
        if chat_id == OMARCHY_CHAT {
            let should_seed = self
                .virtual_stores
                .borrow()
                .get(&OMARCHY_CHAT)
                .is_some_and(|store| store.msgs.is_empty());
            if should_seed {
                let actions = os::catalog(&settings.os.actions);
                let help = os::help_text(&actions);
                if let Some(store) = self.virtual_stores.borrow_mut().get_mut(&OMARCHY_CHAT) {
                    store.append(OMARCHY_CHAT, help, false, false);
                }
            }
        }
        let epoch = self.bump_epoch();
        self.open_chat.set(Some(chat_id));
        self.info.unbind();
        self.apply_info_layout(self.current_window_width());
        self.chatlist.select_chat(chat_id);
        self.messages
            .reset_chat(chat_id, virtual_title(chat_id), epoch);
        self.messages.set_virtual_header(chat_id, &self.tg);
        self.messages.set_pinned_message(None);
        let (messages, mono_ids) = {
            let stores = self.virtual_stores.borrow();
            let Some(store) = stores.get(&chat_id) else {
                return;
            };
            (store.msgs.clone(), store.mono_ids.clone())
        };
        let inserted = self.messages.finish_initial(messages);
        for msg_id in inserted {
            self.messages
                .set_monospace(msg_id, mono_ids.contains(&msg_id));
        }
        self.messages.animate_chat_switched();
        self.update_virtual_status(chat_id);
        self.refresh_virtual_rows(&settings);
    }

    /// Reload the current chat even when it is already open. This is the data
    /// half of the flags-before-data anti-delete transition (D2).
    fn force_reload(self: Rc<Self>, chat_id: i64) {
        if self.open_chat.get() != Some(chat_id) {
            return;
        }
        // `reset_chat` hides the recorder bar; the machine has to be
        // transitioned with it or the composer stays locked with no controls.
        self.cancel_recording();
        self.close_forward();
        self.close_viewer();
        self.close_profile();
        self.close_stickers();
        if self.messages.search_is_open() {
            self.close_in_chat_search();
        }
        if is_virtual(chat_id) {
            self.open_chat.set(None);
            self.open_virtual_chat(chat_id);
            return;
        }
        let epoch = self.bump_epoch();
        let title = self.title_for(chat_id);
        self.messages.reset_chat(chat_id, &title, epoch);
        if let Some(summary) = self.chatlist.summary(chat_id) {
            self.messages.set_chat_summary(&summary, &self.tg);
            let known = self
                .read_outbox
                .borrow()
                .get(&chat_id)
                .copied()
                .unwrap_or(summary.read_outbox_max_id);
            self.messages.set_read_outbox(known);
        }
        self.restore_draft(chat_id);
        self.load_pinned(chat_id);
        self.start_initial_load(chat_id, epoch);
    }

    // History completions are guarded by EPOCH only (C1). They are not
    // settings_gen-guarded on purpose: the merge already applies the CURRENT
    // anti-delete flag (apply_tombstones), an anti-delete flip force_reloads
    // (new epoch), and a gen-discard would strand Loading…/paging (C3).
    fn start_initial_load(self: Rc<Self>, chat_id: i64, epoch: u64) {
        self.messages.begin_history_load();
        let cached = Rc::new(RefCell::new(Vec::<Msg>::new()));
        let fresh_succeeded = Rc::new(Cell::new(false));
        let fresh_error = Rc::new(RefCell::new(None::<String>));
        let started = std::time::Instant::now();
        {
            let this = self.clone();
            let cached = cached.clone();
            let fresh_succeeded = fresh_succeeded.clone();
            let fresh_error = fresh_error.clone();
            glib::MainContext::default().spawn_local(async move {
                let Ok(mut messages) = this.tg.get_cached_history(chat_id).await else { return };
                if !this.is_current(chat_id, epoch) || fresh_succeeded.get() || messages.is_empty() { return; }
                this.apply_tombstones(chat_id, &mut messages);
                if messages.is_empty() { return; }
                *cached.borrow_mut() = messages.clone();
                let inserted = this.messages.finish_cached(messages);
                this.post_render(inserted);
                this.messages.animate_chat_switched();
                if let Some(error) = fresh_error.borrow().as_ref() { this.messages.fail_initial(error); }
                if std::env::var_os("OMG_HISTORY_TRACE").is_some() {
                    eprintln!("[history] cache displayed: {} messages in {} ms", cached.borrow().len(), started.elapsed().as_millis());
                }
            });
        }
        glib::MainContext::default().spawn_local(async move {
            match self.tg.get_history(chat_id, None).await {
                Ok(mut messages) => {
                    if !self.is_current(chat_id, epoch) {
                        return;
                    }
                    fresh_succeeded.set(true);
                    self.apply_tombstones(chat_id, &mut messages);
                    if let Some(last) = messages.iter().rev().find(|message| !message.deleted) {
                        self.remember_last(last);
                    }
                    for msg in &messages {
                        self.check_own_live_message(msg);
                    }
                    let ids = messages.iter().map(|m| m.id).collect::<Vec<_>>();
                    let inserted = self.messages.finish_refreshed(messages, &cached.borrow());
                    // Media references become available with the server page;
                    // retry cached placeholders as well as newly inserted rows.
                    self.post_render(if cached.borrow().is_empty() { inserted } else { ids });
                    if cached.borrow().is_empty() { self.messages.animate_chat_switched(); }
                    if std::env::var_os("OMG_HISTORY_TRACE").is_some() {
                        eprintln!("[history] server displayed in {} ms", started.elapsed().as_millis());
                    }
                    if self.window_is_active() {
                        let latest = self
                            .messages
                            .last_message()
                            .map(|message| message.id)
                            .unwrap_or(0);
                        self.queue_mark_read(chat_id, latest, epoch);
                    }
                }
                Err(error) => {
                    *fresh_error.borrow_mut() = Some(error.clone());
                    shell_log!("get_history({chat_id}): {error}");
                    if self.is_current(chat_id, epoch) {
                        self.messages.fail_initial(&error);
                    }
                }
            }
        });
    }

    fn paginate(self: Rc<Self>) {
        let Some(chat_id) = self.open_chat.get() else {
            return;
        };
        if is_virtual(chat_id) {
            return;
        }
        let Some(before_id) = self.messages.begin_page() else {
            return;
        };
        let epoch = self.epoch.get();
        glib::MainContext::default().spawn_local(async move {
            match self.tg.get_history(chat_id, Some(before_id)).await {
                Ok(mut messages) => {
                    if !self.is_current(chat_id, epoch) {
                        return;
                    }
                    self.apply_tombstones(chat_id, &mut messages);
                    let inserted = self.messages.finish_page(messages);
                    self.post_render(inserted);
                }
                Err(error) => {
                    shell_log!("get_history page ({chat_id}): {error}");
                    if self.is_current(chat_id, epoch) {
                        self.messages.fail_page(&error);
                    }
                }
            }
        });
    }

    fn handle_message_action(self: Rc<Self>, action: MessageAction) {
        if !self.session_ready.get() {
            return;
        }
        if self.open_chat.get().is_some_and(is_virtual)
            && matches!(
                &action,
                MessageAction::Reply(_) | MessageAction::Edit(_) | MessageAction::Delete(_)
            )
        {
            return;
        }
        match action {
            MessageAction::RetryHistory => {
                if let Some(chat_id) = self.open_chat.get() { self.clone().force_reload(chat_id); }
            }
            MessageAction::Switcher => self.open_switcher(),
            MessageAction::Submit => self.submit_composer(),
            MessageAction::Mic => self.start_recording(),
            MessageAction::Stickers => self.open_stickers(),
            MessageAction::RecorderCancel => self.cancel_recording(),
            MessageAction::RecorderSend => self.stop_and_send_recording(),
            MessageAction::RecorderRetry => self.retry_voice(),
            MessageAction::VideoNoteStart => self.start_video_note(),
            MessageAction::VideoNoteCancel => self.cancel_video_note(),
            MessageAction::VideoNoteSend => self.send_video_note(),
            MessageAction::VideoNoteClose => self.cancel_video_note(),
            MessageAction::DraftChanged => self.composer_draft_changed(),
            MessageAction::DraftRetry => {
                if let Some(chat_id) = self.open_chat.get().filter(|id| !is_virtual(*id)) {
                    self.flush_draft(chat_id);
                }
            }
            MessageAction::RetryReaction => {
                let retry = self.reaction_retry.borrow_mut().take();
                if let Some((chat_id, msg_id, emoji)) = retry
                    && self.open_chat.get() == Some(chat_id) {
                        self.send_reaction(msg_id, emoji);
                    }
            }
            MessageAction::RetryAvailableReactions => self.load_available_reactions(),
            MessageAction::SearchChanged(query) => self.search_in_chat(query, None),
            MessageAction::SearchPrevious => self.move_search_result(-1),
            MessageAction::SearchNext => self.move_search_result(1),
            MessageAction::SearchOlder => self.load_older_search_results(),
            MessageAction::SearchRetry => self.retry_in_chat_search(),
            MessageAction::SearchClose => self.close_in_chat_search(),
            MessageAction::Header(action) => {
                if let Some(chat_id) = self.open_chat.get().filter(|id| !is_virtual(*id)) {
                    self.handle_chat_action(chat_id, action);
                }
            }
            MessageAction::AttachFile => {
                if self.open_chat.get().is_some_and(is_virtual) {
                    return;
                }
                self.messages.prepare_attachment();
                self.open_file_dialog();
            }
            MessageAction::AttachPoll => self.open_poll_dialog(),
            MessageAction::AttachLocation => self.open_location_dialog(),
            MessageAction::SendLater(at) => {
                if let Some(chat_id) = self.open_chat.get().filter(|id| !is_virtual(*id)) {
                    self.send_text_later(chat_id, at);
                }
            }
            MessageAction::SendScheduledNow(ids) => {
                if let Some(chat_id) = self.open_chat.get().filter(|id| !is_virtual(*id)) {
                    self.send_scheduled_now(chat_id, ids);
                }
            }
            MessageAction::DeleteScheduled(ids) => {
                if let Some(chat_id) = self.open_chat.get().filter(|id| !is_virtual(*id)) {
                    self.delete_scheduled(chat_id, ids);
                }
            }
            MessageAction::DropFile(file) => {
                if self.open_chat.get().is_some_and(is_virtual) {
                    return;
                }
                self.messages.prepare_attachment();
                self.send_file(file);
            }
            MessageAction::Reply(msg_id) => {
                self.messages.begin_reply(msg_id);
                self.composer_draft_changed();
            }
            MessageAction::Edit(msg_id) => self.messages.begin_edit(msg_id),
            MessageAction::EditHistory(msg_id) => self.open_edit_history(msg_id),
            MessageAction::Forward(msg_id) => self.open_forward(vec![msg_id]),
            MessageAction::Select(msg_id) => {
                self.close_stickers();
                self.messages.begin_selection(msg_id);
            }
            MessageAction::SelectionForward => {
                let ids = self.messages.selection_ids();
                self.open_forward(ids);
            }
            MessageAction::SelectionDelete => self.confirm_delete_selected(),
            MessageAction::SelectionCopy => {
                let text = self
                    .messages
                    .selection_ids()
                    .into_iter()
                    .filter_map(|msg_id| self.messages.message(msg_id))
                    .map(|message| message.text)
                    .collect::<Vec<_>>()
                    .join("\n");
                if !text.is_empty() {
                    self.widget.clipboard().set_text(&text);
                    *self.probe_copied.borrow_mut() = text;
                }
            }
            MessageAction::SelectionCancel => self.messages.exit_selection_mode(),
            MessageAction::Reaction { msg_id, emoji } => {
                if let Some(emoji) = emoji {
                    self.send_reaction(msg_id, emoji);
                }
            }
            MessageAction::RevealSpoiler { msg_id, start, end } => {
                self.messages.reveal_spoiler(msg_id, start, end);
            }
            MessageAction::OpenLink(url) => self.open_link(&url),
            MessageAction::OpenMention(user_id) => self.open_mention(user_id),
            MessageAction::OpenSender(msg_id) => {
                if let Some(message) = self.messages.message(msg_id)
                    && let Some(user_id) = message.sender_id.filter(|id| *id > 0) {
                    self.open_profile(user_id, &message.sender, Some((message.chat_id, msg_id)));
                }
            }
            MessageAction::Vote { msg_id, options } => {
                if let Some(chat_id) = self.open_chat.get().filter(|id| !is_virtual(*id)) {
                    self.vote(chat_id, msg_id, options);
                }
            }
            MessageAction::RetractVote(msg_id) => {
                if let Some(chat_id) = self.open_chat.get().filter(|id| !is_virtual(*id)) {
                    self.vote(chat_id, msg_id, Vec::new());
                }
            }
            MessageAction::AddContact(msg_id) => self.add_contact(msg_id),
            MessageAction::OpenInBrowser(url) => self.open_in_browser(&url),
            MessageAction::UpdateLive(msg_id) => self.open_update_live_dialog(msg_id),
            MessageAction::StopLive(msg_id) => {
                if let Some(chat_id) = self.open_chat.get().filter(|id| !is_virtual(*id)) {
                    self.stop_live(chat_id, msg_id);
                }
            }
            MessageAction::RedownloadMedia(msg_id) => {
                self.start_media_download(msg_id, false);
            }
            MessageAction::PressButton { msg_id, data } => self.press_callback_button(msg_id, data),
            MessageAction::Start => self.send_start(),
            MessageAction::Back => self.go_back_from_topic(),
            MessageAction::SwitchInline {
                query,
                same_chat,
                bot_username,
            } => self.switch_inline(query, same_chat, bot_username),
            MessageAction::UnpinMessage(msg_id) => self.unpin_message(msg_id),
            MessageAction::RetryPinned => {
                if let Some(chat_id) = self.open_chat.get() {
                    self.load_pinned(chat_id);
                }
            }
            MessageAction::Delete(msg_id) => self.delete_message(msg_id),
            MessageAction::Media(msg_id) => self.media_action(msg_id, player::OpenIntent::Manual),
            MessageAction::MediaAutoplay(msg_id) => {
                self.media_action(msg_id, player::OpenIntent::AutoplayMuted)
            }
            MessageAction::MediaSeek(msg_id, fraction) => {
                self.messages.player_seek(msg_id, fraction);
            }
            // The pill cycles 1x/1.5x/2x and persists through SettingsStore.
            MessageAction::MediaSpeed(msg_id) => {
                self.messages.player_cycle_speed(msg_id);
            }
            MessageAction::MediaMute(msg_id) => self.messages.player_toggle_mute(msg_id),
            MessageAction::MediaFullscreen(msg_id) => self.messages.player_fullscreen(msg_id),
            MessageAction::Paginate => self.paginate(),
            MessageAction::CancelMode => {
                self.messages.cancel_mode();
                self.composer_draft_changed();
            }
            MessageAction::CopyMessageId(msg_id) => {
                if let Some(message) = self.messages.message(msg_id) {
                    self.widget
                        .clipboard()
                        .set_text(&format!("chat {} msg {}", message.chat_id, message.id));
                }
            }
            MessageAction::CopyUserId(msg_id) => {
                let sender_id = self
                    .messages
                    .message(msg_id)
                    .and_then(|message| message.sender_id);
                if let Some(sender_id) = sender_id {
                    self.widget.clipboard().set_text(&sender_id.to_string());
                }
            }
            MessageAction::JumpToMessage(msg_id) => self.jump_to_message(msg_id),
            MessageAction::JumpToDate(date) => self.jump_to_date(date),
            MessageAction::JumpToLatest => self.jump_to_latest(),
            MessageAction::DraftReply(msg_id) => self.draft_reply(msg_id),
            MessageAction::Translate(msg_id) => self.translate_message(msg_id),
            MessageAction::Summarize(msg_id) => self.summarize_message(msg_id),
            MessageAction::Transcribe(msg_id) => self.request_transcription(msg_id),
        }
    }

    /// 6B: send a vote (empty `options` retracts) and reflect the result on the
    /// poll card. `Event::PollChanged` re-renders the card on success.
    fn vote(self: Rc<Self>, chat_id: i64, msg_id: i32, options: Vec<usize>) {
        let Some(widget) = self.messages.card_widget(msg_id) else {
            return;
        };
        let epoch = self.epoch.get();
        crate::ui::poll::set_voting(&widget, true);
        glib::MainContext::default().spawn_local(async move {
            let result = self.tg.send_vote(chat_id, msg_id, options).await;
            if !self.is_current(chat_id, epoch) {
                return;
            }
            match result {
                Ok(()) => {
                    // The card is re-enabled by the incoming `Event::PollChanged`.
                    if let Some(widget) = self.messages.card_widget(msg_id) {
                        widget.set_sensitive(true);
                    }
                }
                Err(error) => {
                    if let Some(widget) = self.messages.card_widget(msg_id) {
                        crate::ui::poll::set_error(&widget, &error);
                        widget.set_sensitive(true);
                    }
                    self.messages.show_error(&error);
                }
            }
        });
    }

    /// 6B: add a contact card's user to the address book.
    fn add_contact(self: Rc<Self>, msg_id: i32) {
        let Some(chat_id) = self.open_chat.get().filter(|id| !is_virtual(*id)) else {
            return;
        };
        let epoch = self.epoch.get();
        let Some(message) = self.messages.message(msg_id) else {
            return;
        };
        let Some(contact) = message.contact.clone() else {
            return;
        };
        let Some(user_id) = contact.user_id else {
            return;
        };
        let Some(widget) = self.messages.card_widget(msg_id) else {
            return;
        };
        crate::ui::cards::set_contact_pending(&widget, true);
        glib::MainContext::default().spawn_local(async move {
            let result = self
                .tg
                .add_contact(
                    user_id,
                    &contact.first_name,
                    &contact.last_name,
                    &contact.phone,
                )
                .await;
            if !self.is_current(chat_id, epoch)
                || self
                    .messages
                    .message(msg_id)
                    .and_then(|message| message.contact)
                    .and_then(|contact| contact.user_id)
                    != Some(user_id)
            {
                return;
            }
            match result {
                Ok(()) => {
                    self.messages.mark_contact_added(msg_id);
                }
                Err(error) => {
                    if let Some(widget) = self.messages.card_widget(msg_id) {
                        crate::ui::cards::set_contact_error(&widget, &error);
                        crate::ui::cards::set_contact_pending(&widget, false);
                    }
                }
            }
        });
    }

    /// 6B: open an OpenStreetMap link in the default browser.
    fn open_in_browser(self: &Rc<Self>, url: &str) {
        let url = url.to_string();
        if self.probe {
            self.probe_uri_launches
                .set(self.probe_uri_launches.get().wrapping_add(1));
            return;
        }
        if let Err(error) =
            gio::AppInfo::launch_default_for_uri(&url, None::<&gio::AppLaunchContext>)
        {
            shell_log!("open in browser: {error}");
            self.messages.show_error(error.message());
        }
    }

    /// 6B: stop sharing our own live location.
    fn stop_live(self: Rc<Self>, chat_id: i64, msg_id: i32) {
        glib::MainContext::default().spawn_local(async move {
            if let Err(error) = self.tg.stop_live_location(chat_id, msg_id).await {
                self.messages.show_error(&error);
            }
        });
    }

    fn open_in_chat_search(self: &Rc<Self>) {
        if self.open_chat.get().is_none_or(is_virtual) {
            return;
        }
        self.close_forward();
        self.close_viewer();
        self.close_profile();
        self.switcher.close();
        self.close_settings();
        self.messages.open_search();
    }

    fn close_in_chat_search(&self) {
        let generation = self.in_chat_search.borrow().generation.wrapping_add(1);
        *self.in_chat_search.borrow_mut() = InChatSearchState {
            generation,
            ..InChatSearchState::default()
        };
        self.messages.close_search();
    }

    fn search_in_chat(self: Rc<Self>, query: String, before_id: Option<i32>) {
        let Some(chat_id) = self.open_chat.get().filter(|chat_id| !is_virtual(*chat_id)) else {
            return;
        };
        let query = query.trim().to_string();
        let (generation, prior_hits) = {
            let mut state = self.in_chat_search.borrow_mut();
            state.generation = state.generation.wrapping_add(1);
            state.query = query.clone();
            state.in_flight = query.chars().count() >= 3;
            state.retry_before = before_id;
            if before_id.is_none() {
                if query.chars().count() < 3 {
                    state.hits.clear();
                }
                state.index = None;
                state.exhausted = false;
            }
            (state.generation, state.hits.clone())
        };
        if query.chars().count() < 3 {
            self.messages.set_search_results(&[], None, false);
            return;
        }
        self.messages.set_search_loading(true);
        let session_epoch = self.session_epoch.get();
        glib::MainContext::default().spawn_local(async move {
            let result = self.tg.search_messages(chat_id, &query, before_id).await;
            if !self.is_session_current(session_epoch)
                || self.open_chat.get() != Some(chat_id)
                || !self.messages.search_is_open()
                || self.in_chat_search.borrow().generation != generation
            {
                return;
            }
            match result {
                Ok(page) => {
                    let mut state = self.in_chat_search.borrow_mut();
                    state.in_flight = false;
                    state.exhausted = page.len() < 50;
                    if before_id.is_none() {
                        state.hits = page;
                    } else {
                        let mut known = state
                            .hits
                            .iter()
                            .map(|message| message.id)
                            .collect::<HashSet<_>>();
                        state
                            .hits
                            .extend(page.into_iter().filter(|message| known.insert(message.id)));
                    }
                    if state.index.is_none() && !state.hits.is_empty() {
                        state.index = Some(0);
                    }
                    state.retry_before = None;
                    drop(state);
                    self.refresh_in_chat_search();
                    self.jump_to_active_search_result();
                }
                Err(error) => {
                    self.in_chat_search.borrow_mut().in_flight = false;
                    shell_log!("search_messages({chat_id}): {error}");
                    self.messages.fail_search(&error, !prior_hits.is_empty());
                }
            }
        });
    }

    fn refresh_in_chat_search(&self) {
        let state = self.in_chat_search.borrow();
        let ids = state
            .hits
            .iter()
            .map(|message| message.id)
            .collect::<Vec<_>>();
        self.messages
            .set_search_results(&ids, state.index, !state.exhausted && !state.in_flight);
    }

    fn move_search_result(self: Rc<Self>, delta: isize) {
        {
            let mut state = self.in_chat_search.borrow_mut();
            let Some(index) = state.index else { return };
            let next = index as isize + delta;
            if next < 0 || next >= state.hits.len() as isize {
                return;
            }
            state.index = Some(next as usize);
        }
        self.refresh_in_chat_search();
        self.jump_to_active_search_result();
    }

    fn load_older_search_results(self: Rc<Self>) {
        let (query, before) = {
            let state = self.in_chat_search.borrow();
            if state.in_flight || state.exhausted {
                return;
            }
            (
                state.query.clone(),
                state.hits.iter().map(|message| message.id).min(),
            )
        };
        if let Some(before) = before {
            self.search_in_chat(query, Some(before));
        }
    }

    fn retry_in_chat_search(self: Rc<Self>) {
        let (query, before) = {
            let state = self.in_chat_search.borrow();
            (state.query.clone(), state.retry_before)
        };
        self.search_in_chat(query, before);
    }

    fn jump_to_active_search_result(self: &Rc<Self>) {
        let (hit, generation) = {
            let state = self.in_chat_search.borrow();
            (
                state.index.and_then(|index| state.hits.get(index)).cloned(),
                state.generation,
            )
        };
        let Some(hit) = hit else { return };
        if self.messages.scroll_to_search_result(hit.id) {
            return;
        }
        if self.messages.contains(hit.id) {
            self.scroll_search_result_next_tick(hit.id, generation);
            return;
        }
        let chat_id = hit.chat_id;
        let session_epoch = self.session_epoch.get();
        self.close_forward();
        self.close_viewer();
        self.close_profile();
        let this = self.clone();
        glib::MainContext::default().spawn_local(async move {
            // A17 order matters: anchor around the search hit's timestamp
            // first, then fetch the exact id so a sparse date page cannot omit
            // the active result.
            let surrounding = this.tg.get_history_at_date(chat_id, hit.ts).await;
            if !this.is_session_current(session_epoch)
                || this.open_chat.get() != Some(chat_id)
                || this.in_chat_search.borrow().generation != generation
                || !this.messages.search_is_open()
            {
                return;
            }
            let mut messages = match surrounding {
                Ok(messages) => messages,
                Err(error) => {
                    this.messages.fail_search(&error, true);
                    return;
                }
            };
            let ensured = this.tg.get_messages(chat_id, vec![hit.id]).await;
            if !this.is_session_current(session_epoch)
                || this.open_chat.get() != Some(chat_id)
                || this.in_chat_search.borrow().generation != generation
                || !this.messages.search_is_open()
            {
                return;
            }
            let ensure = match ensured {
                Ok(messages) => messages.into_iter().find(|message| message.id == hit.id),
                Err(error) => {
                    this.messages.fail_search(&error, true);
                    return;
                }
            };
            let Some(ensure) = ensure else {
                this.messages
                    .fail_search("Message is no longer available", true);
                return;
            };
            if !messages.iter().any(|message| message.id == ensure.id) {
                messages.push(ensure);
            }
            this.apply_tombstones(chat_id, &mut messages);
            let epoch = this.bump_epoch();
            this.messages.reset_history(chat_id, epoch);
            let inserted = this.messages.finish_initial(messages);
            this.post_render(inserted);
            this.messages.set_detached(true);
            this.refresh_in_chat_search();
            if !this.messages.scroll_to_search_result(hit.id) {
                this.scroll_search_result_next_tick(hit.id, generation);
            }
        });
    }

    fn scroll_search_result_next_tick(self: &Rc<Self>, msg_id: i32, generation: u64) {
        let weak = Rc::downgrade(self);
        self.messages.widget.add_tick_callback(move |_, _| {
            if let Some(this) = weak.upgrade().filter(|this| {
                this.messages.search_is_open()
                    && this.in_chat_search.borrow().generation == generation
            }) {
                this.messages.scroll_to_search_result(msg_id);
            }
            glib::ControlFlow::Break
        });
    }

    fn load_pinned(self: &Rc<Self>, chat_id: i64) {
        let generation = self.pinned_generation.get().wrapping_add(1);
        self.pinned_generation.set(generation);
        self.messages.begin_pinned();
        let session_epoch = self.session_epoch.get();
        let tg = self.tg.clone();
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let result = tg.get_pinned_message(chat_id).await;
            let Some(this) = weak.upgrade().filter(|this| {
                this.is_session_current(session_epoch)
                    && this.open_chat.get() == Some(chat_id)
                    && this.pinned_generation.get() == generation
            }) else {
                return;
            };
            match result {
                Ok(message) => this.messages.set_pinned_message(message),
                Err(error) => this.messages.fail_pinned(&error),
            }
        });
    }

    fn unpin_message(self: Rc<Self>, msg_id: i32) {
        let Some(chat_id) = self.open_chat.get() else {
            return;
        };
        let epoch = self.epoch.get();
        let Some(session_epoch) = self.begin_mutation() else {
            return;
        };
        glib::MainContext::default().spawn_local(async move {
            let result = self.tg.pin_message(chat_id, msg_id, false).await;
            self.finish_mutation();
            if !self.is_session_current(session_epoch) || !self.is_current(chat_id, epoch) {
                return;
            }
            match result {
                Ok(()) => self.messages.set_pinned_message(None),
                Err(error) => self.messages.show_error(&error),
            }
        });
    }

    fn send_reaction(self: Rc<Self>, msg_id: i32, emoji: String) {
        let Some(chat_id) = self.open_chat.get() else {
            return;
        };
        let Some(before) = self.messages.message(msg_id) else {
            return;
        };
        if before.deleted {
            return;
        }
        let target = if before
            .reactions
            .iter()
            .any(|reaction| reaction.chosen && reaction.emoji == emoji)
        {
            None
        } else {
            Some(emoji.clone())
        };
        let message_key = (chat_id, msg_id);
        let generation = self
            .reaction_generations
            .borrow()
            .get(&message_key)
            .copied()
            .unwrap_or(0)
            .wrapping_add(1);
        self.reaction_generations
            .borrow_mut()
            .insert(message_key, generation);
        let change_generation = self
            .message_change_generations
            .borrow()
            .get(&message_key)
            .copied()
            .unwrap_or(0);
        let Some(session_epoch) = self.begin_mutation() else {
            return;
        };
        if !self.messages.optimistic_reaction(msg_id, &emoji) {
            self.finish_mutation();
            return;
        }
        let epoch = self.epoch.get();
        glib::MainContext::default().spawn_local(async move {
            let result = self.tg.send_reaction(chat_id, msg_id, target).await;
            self.finish_mutation();
            if !self.is_session_current(session_epoch) {
                return;
            }
            match result {
                Ok(()) => {
                    let latest_mutation = self
                        .reaction_generations
                        .borrow()
                        .get(&message_key)
                        .copied()
                        == Some(generation);
                    if latest_mutation {
                        self.reaction_retry.borrow_mut().take();
                        self.messages.clear_reaction_error();
                    }
                }
                Err(error) => {
                    let latest_mutation = self
                        .reaction_generations
                        .borrow()
                        .get(&message_key)
                        .copied()
                        == Some(generation);
                    let unchanged = self
                        .message_change_generations
                        .borrow()
                        .get(&message_key)
                        .copied()
                        .unwrap_or(0)
                        == change_generation;
                    if latest_mutation && unchanged && self.is_current(chat_id, epoch) {
                        self.messages.merge_event(before);
                    }
                    if latest_mutation && self.is_current(chat_id, epoch) {
                        *self.reaction_retry.borrow_mut() = Some((chat_id, msg_id, emoji));
                        self.messages.show_reaction_error(&error);
                    }
                }
            }
        });
    }

    fn open_link(&self, url: &str) {
        if !super::markup::is_allowed_link(url) {
            self.messages.show_error("Blocked unsafe link");
            return;
        }
        // The probe must never reach the user's browser: count the launch
        // instead (same rule as launch_media / open_in_browser).
        if self.probe {
            self.probe_media_launches
                .set(self.probe_media_launches.get().wrapping_add(1));
            return;
        }
        if let Some(window) = self.window() {
            let launcher = gtk::UriLauncher::new(url);
            let messages = self.messages.clone();
            glib::MainContext::default().spawn_local(async move {
                if let Err(error) = launcher.launch_future(Some(&window)).await {
                    messages.show_error(error.message());
                }
            });
        }
    }

    fn open_profile(self: &Rc<Self>, user_id: i64, name: &str, source: Option<(i64, i32)>) {
        if !self.session_ready.get() || user_id <= 0 { return; }
        self.close_viewer();
        self.close_forward();
        self.close_contacts();
        self.close_new_group();
        self.switcher.close();
        self.close_settings();
        let settings = self.settings.get();
        let generation = self.profile.begin(&self.tg, user_id, name, source, settings.ui.show_avatars);
        self.apply_info_layout(self.current_window_width());
        let tg = self.tg.clone();
        let weak = Rc::downgrade(self);
        let session = self.session_epoch.get();
        glib::MainContext::default().spawn_local(async move {
            let result = glib::future_with_timeout(Duration::from_secs(20), tg.get_user_profile(user_id, source)).await
                .unwrap_or_else(|_| Err("Loading profile timed out; try again".into()));
            let Some(this) = weak.upgrade().filter(|s| s.is_session_current(session)) else { return };
            match result {
                Ok(info) => this.profile.finish(&tg, user_id, generation, info,
                    this.settings.get().ui.show_avatars, cfg!(feature = "calls")),
                Err(error) => this.profile.fail(user_id, generation, &error),
            }
        });
    }

    fn close_profile(&self) {
        if !self.profile.is_open() { return; }
        let source = self.profile.source();
        self.profile.close();
        if let Some((_, msg_id)) = source { self.messages.focus_message_or_composer(msg_id); }
        else { self.messages.focus_composer(); }
        self.apply_info_layout(self.current_window_width());
    }

    fn handle_profile_action(self: Rc<Self>, action: ProfileAction) {
        let Some(id) = self.profile.user_id() else { return };
        match action {
            ProfileAction::Close => self.close_profile(),
            ProfileAction::Retry => self.open_profile(id, "Profile", self.profile.source()),
            ProfileAction::Message => {
                let generation = self.profile.generation();
                let session = self.session_epoch.get();
                glib::MainContext::default().spawn_local(async move {
                    let result = self.tg.open_user(id).await;
                    if !self.is_session_current(session) || !self.profile.matches(id, generation) { return; }
                    match result {
                        Ok(summary) => { self.chatlist.set_summary(summary); self.open_chat(id); }
                        Err(error) => self.profile.fail(id, generation, &error),
                    }
                });
            }
            ProfileAction::Call => { self.close_profile(); self.start_call(id); }
            ProfileAction::Photo => {
                if let Some(info) = self.profile.info().filter(|info| info.has_photo) {
                    self.open_profile_photo(id, &info.title);
                }
            }
        }
    }

    fn open_profile_photo(self: &Rc<Self>, id: i64, title: &str) {
        self.close_viewer();
        let generation = self.viewer_generation.get().wrapping_add(1);
        self.viewer_generation.set(generation);
        self.viewer.present_profile(id, title, generation);
        self.apply_info_layout(self.current_window_width());
    }

    fn open_mention(self: Rc<Self>, user_id: i64) {
        let session_epoch = self.session_epoch.get();
        glib::MainContext::default().spawn_local(async move {
            match self.tg.open_user(user_id).await {
                Ok(summary) if self.is_session_current(session_epoch) => {
                    let chat_id = summary.id;
                    self.chatlist.set_summary(summary);
                    self.open_chat(chat_id);
                }
                Err(error) if self.is_session_current(session_epoch) => {
                    self.messages.show_error(&error)
                }
                _ => {}
            }
        });
    }

    /// Bot inline-keyboard callback button (wave 6E §6.1).
    fn press_callback_button(self: Rc<Self>, msg_id: i32, data: Vec<u8>) {
        let Some(chat_id) = self.open_chat.get().filter(|id| !is_virtual(*id)) else {
            return;
        };
        let epoch = self.epoch.get();
        glib::MainContext::default().spawn_local(async move {
            let result = self.tg.press_button(chat_id, msg_id, data).await;
            if !self.is_current(chat_id, epoch) {
                return;
            }
            // The button showed "…" while in flight: rebuild it either way.
            self.messages.refresh_keyboard(msg_id);
            match result {
                // Some(text) = the bot answered with a toast/alert (§6.1).
                Ok(Some(text)) => self.messages.show_info(&text),
                Ok(None) => {}
                Err(error) => {
                    shell_log!("press_button({chat_id}, {msg_id}): {error}");
                    self.messages.show_error(&error);
                }
            }
        });
    }

    /// Bot Start button (wave 6E §6.2): sends `/start`.
    fn send_start(self: Rc<Self>) {
        let Some(chat_id) = self.open_chat.get().filter(|id| !is_virtual(*id)) else {
            return;
        };
        let epoch = self.epoch.get();
        let title = self.title_for(chat_id);
        glib::MainContext::default().spawn_local(async move {
            let result = self.tg.send_text(chat_id, "/start", None).await;
            match result {
                Ok(message) => {
                    let message = self.apply_tombstone(message);
                    self.remember_last(&message);
                    self.dialog_upsert(
                        chat_id,
                        &title,
                        &message_preview(&message),
                        Some(message.ts),
                        UnreadUpdate::Delta(0),
                    );
                    if self.is_current(chat_id, epoch) {
                        // merge_event restores the composer in place of Start.
                        let inserted = self.messages.merge_event(message);
                        self.post_render(inserted);
                        self.messages.set_start_mode(false);
                    }
                }
                Err(error) => {
                    shell_log!("send_text({chat_id}, /start): {error}");
                    if self.is_current(chat_id, epoch) {
                        self.messages.show_error(&error);
                    }
                }
            }
        });
    }

    /// Back button from an open topic to the forum's topic list.
    fn go_back_from_topic(self: Rc<Self>) {
        let Some((forum_id, _)) = self.open_chat.get().and_then(split_topic_chat_id) else {
            return;
        };
        self.open_chat(forum_id);
    }

    /// Bot SwitchInline keyboard button (wave 6E §6.1): "@bot query" into the
    /// composer of this chat, or of the chat picked in the switcher.
    fn switch_inline(self: Rc<Self>, query: String, same_chat: bool, bot_username: String) {
        let text = format!("@{bot_username} {query}").trim_end().to_string();
        if same_chat {
            self.messages.set_composer_text(&text);
            self.messages.focus_composer();
            return;
        }
        *self.pending_inline_query.borrow_mut() = Some(text);
        self.open_switcher();
    }

    fn reply_last(&self) {
        if let Some(message) = self
            .messages
            .messages()
            .into_iter()
            .rev()
            .find(|message| !message.deleted)
        {
            self.messages.begin_reply(message.id);
        }
    }

    fn open_forward(self: &Rc<Self>, ids: Vec<i32>) {
        let Some(source_chat) = self.open_chat.get().filter(|chat_id| !is_virtual(*chat_id)) else {
            return;
        };
        let ids = self
            .messages
            .messages()
            .into_iter()
            .filter(|message| ids.contains(&message.id) && !message.deleted)
            .map(|message| message.id)
            .collect::<Vec<_>>();
        if ids.is_empty() {
            return;
        }
        self.close_viewer();
        self.close_profile();
        self.switcher.close();
        self.close_settings();
        self.close_forward();
        let generation = self.forward_generation.get().wrapping_add(1);
        self.forward_generation.set(generation);
        let selected = self.messages.messages().into_iter().filter(|message| ids.contains(&message.id)).collect::<Vec<_>>();
        let preview = selected.first().map(|message| {
            let text = if message.text.is_empty() { message.doc_name.clone().unwrap_or_else(|| "Media message".into()) } else { message.text.clone() };
            format!("{}{}", if message.sender.is_empty() { String::new() } else { format!("{}: ", message.sender) }, text.chars().take(180).collect::<String>())
        }).unwrap_or_default();
        self.forward.set_preview(&format!("{} message{}\n{}", ids.len(), if ids.len() == 1 { "" } else { "s" }, preview));
        self.forward.present(
            source_chat,
            ids,
            self.chatlist.ordered_summaries(),
            generation,
        );
        self.apply_info_layout(self.current_window_width());
    }

    fn close_forward(&self) {
        if !self.forward.is_open() {
            return;
        }
        let source = self.forward.source_ids().first().copied();
        self.forward_generation
            .set(self.forward_generation.get().wrapping_add(1));
        if let Some(source) = source {
            self.messages.focus_message_or_composer(source);
        } else {
            self.messages.focus_composer();
        }
        self.forward.close();
        self.apply_info_layout(self.current_window_width());
    }

    fn handle_forward_action(self: Rc<Self>, action: ForwardAction) {
        match action {
            ForwardAction::Close => self.close_forward(),
            ForwardAction::Submit(request) => self.forward_messages(request),
        }
    }

    fn forward_messages(self: Rc<Self>, request: ForwardRequest) {
        if request.generation != self.forward_generation.get() || !self.forward.is_open() {
            return;
        }
        let Some(session_epoch) = self.begin_mutation() else {
            return;
        };
        self.forward.set_busy(request.generation, true);
        glib::MainContext::default().spawn_local(async move {
            let total = request.targets.len();
            let mut successes = Vec::new();
            let mut failures = Vec::new();
            let mut last_error = None;
            for target in &request.targets {
                match self
                    .tg
                    .forward_messages(request.source_chat, request.ids.clone(), *target)
                    .await
                {
                    Ok(messages) => successes.push((*target, messages)),
                    Err(error) => {
                        failures.push(*target);
                        last_error = Some(error);
                    }
                }
            }
            self.finish_mutation();
            if !self.is_session_current(session_epoch)
                || request.generation != self.forward_generation.get()
                || !self.forward.is_open()
            {
                return;
            }
            let succeeded = successes.len();
            if successes.is_empty() {
                self.forward.show_result(
                    request.generation,
                    0,
                    total,
                    &failures,
                    last_error.as_deref(),
                );
                return;
            }
            let last_target = successes.last().map(|(target, _)| *target);
            if !failures.is_empty() {
                self.forward.show_result(
                    request.generation,
                    succeeded,
                    total,
                    &failures,
                    last_error.as_deref(),
                );
                // Keep the dialog-owned partial result visible before moving
                // to the last successful destination.
                glib::timeout_future(Duration::from_millis(900)).await;
                if request.generation != self.forward_generation.get() || !self.forward.is_open() {
                    return;
                }
            }
            self.close_forward();
            if let Some(last_target) = last_target {
                self.clone().open_chat(last_target);
            }
        });
    }

    fn open_viewer(self: &Rc<Self>, msg_id: i32) {
        let Some(chat_id) = self.open_chat.get() else {
            return;
        };
        let photos = self.messages.photo_messages();
        let path = self.messages.media_path(msg_id);
        if self.messages.search_is_open() {
            self.close_in_chat_search();
        }
        self.close_forward();
        self.switcher.close();
        self.close_settings();
        self.close_viewer();
        self.close_profile();
        let generation = self.viewer_generation.get().wrapping_add(1);
        self.viewer_generation.set(generation);
        if self
            .viewer
            .present(chat_id, photos, msg_id, path, generation)
        {
            self.apply_info_layout(self.current_window_width());
            self.clone().load_viewer_media(msg_id, generation);
        }
    }

    fn close_viewer(&self) {
        if !self.viewer.is_open() {
            return;
        }
        let source = self.viewer.source_id();
        self.viewer_generation
            .set(self.viewer_generation.get().wrapping_add(1));
        if let Some(source) = source {
            self.messages.focus_message_or_composer(source);
        } else {
            self.messages.focus_composer();
        }
        self.viewer.close();
        if self.profile.is_open() { self.profile.focus(); }
        self.apply_info_layout(self.current_window_width());
    }

    fn handle_viewer_action(self: Rc<Self>, action: ViewerAction) {
        match action {
            ViewerAction::Retry { msg_id, generation } => {
                if self.viewer_generation.get() != generation || !self.viewer.is_open() { return; }
                self.clone().reload_viewer_media(msg_id, generation);
            }
            ViewerAction::Load { msg_id, generation } => self.load_viewer_media(msg_id, generation),
            ViewerAction::Open { msg_id, generation } => {
                if self.viewer_generation.get() == generation
                    && let Some(path) = self.viewer_media_path(msg_id) {
                        self.launch_media(&path);
                    }
            }
            ViewerAction::Save { msg_id, generation } => self.save_viewer_media(msg_id, generation),
            ViewerAction::Close { source_id: _ } => self.close_viewer(),
        }
    }

    fn load_viewer_media(self: Rc<Self>, msg_id: i32, generation: u64) {
        if self.viewer_generation.get() != generation || !self.viewer.is_open() {
            return;
        }
        if self.viewer.profile_peer().is_some() {
            self.load_profile_photo(generation, false);
            return;
        }
        if let Some(path) = self.viewer_media_path(msg_id) {
            self.viewer.set_path(msg_id, generation, path);
            return;
        }
        if matches!(self.messages.media_state(msg_id), Some(MediaState::Failed)) {
            self.reload_viewer_media(msg_id, generation);
            return;
        }
        let weak = Rc::downgrade(&self);
        let registered = self.messages.on_media_ready(msg_id, move |path| {
            if let Some(this) = weak
                .upgrade()
                .filter(|this| this.viewer_generation.get() == generation && this.viewer.is_open())
            {
                this.viewer.set_path(msg_id, generation, path);
            }
        });
        if !registered {
            self.load_shared_viewer_media(msg_id, generation);
            return;
        }
        if matches!(
            self.messages.media_state(msg_id),
            Some(MediaState::NotStarted | MediaState::Failed)
        ) {
            self.start_media_download(msg_id, false);
        }
    }

    fn load_profile_photo(self: Rc<Self>, generation: u64, refresh: bool) {
        let Some(peer) = self.viewer.profile_peer() else { return };
        let tg = self.tg.clone();
        let weak = Rc::downgrade(&self);
        glib::MainContext::default().spawn_local(async move {
            let result = if refresh { tg.retry_profile_photo(peer).await } else { tg.download_profile_photo(peer).await };
            let Some(this) = weak.upgrade().filter(|this| this.viewer_generation.get() == generation
                && this.viewer.profile_peer() == Some(peer)) else { return };
            match result {
                Ok(Some(path)) => { this.viewer.set_path(0, generation, path); }
                Ok(None) => this.viewer.show_error(0, generation, "No profile photo available"),
                Err(_) => this.viewer.show_error(0, generation, "Could not load profile photo"),
            }
        });
    }

    fn reload_viewer_media(self: Rc<Self>, msg_id: i32, generation: u64) {
        if self.viewer.profile_peer().is_some() {
            self.load_profile_photo(generation, true);
            return;
        }
        let Some(chat_id) = self.viewer.chat_id() else { return };
        let tg = self.tg.clone();
        let weak = Rc::downgrade(&self);
        glib::MainContext::default().spawn_local(async move {
            let result = tg.retry_media(chat_id, msg_id).await;
            let Some(this) = weak.upgrade().filter(|this| this.viewer_generation.get() == generation
                && this.viewer.chat_id() == Some(chat_id)) else { return };
            match result {
                Ok(Some(path)) => { this.viewer.set_path(msg_id, generation, path); }
                _ => this.viewer.show_error(msg_id, generation, "Could not load image"),
            }
        });
    }

    fn viewer_media_path(&self, msg_id: i32) -> Option<PathBuf> {
        if self.viewer.profile_peer().is_some() { return self.viewer.path(msg_id); }
        self.messages.media_path(msg_id)
            .or_else(|| self.info.shared_path(msg_id))
            .or_else(|| self.viewer.path(msg_id))
    }

    fn load_shared_viewer_media(self: Rc<Self>, msg_id: i32, generation: u64) {
        let Some(chat_id) = self.viewer.chat_id() else { return };
        let bind_generation = self.info.bind_generation();
        let shared_generation = self.info.shared_generation();
        if !self.info.is_bound(chat_id)
            || self.info.shared_kind() != SharedKind::Photos
            || !self
                .info
                .shared_messages()
                .iter()
                .any(|message| message.id == msg_id)
        {
            return;
        }
        let tg = self.tg.clone();
        let weak = Rc::downgrade(&self);
        glib::MainContext::default().spawn_local(async move {
            let Ok(Some(path)) = tg.download_media(chat_id, msg_id).await else {
                if let Some(this) = weak.upgrade() {
                    this.viewer
                        .show_error(msg_id, generation, "Image unavailable");
                }
                return;
            };
            let decode_path = path.clone();
            let Ok(Ok(texture)) = gio::spawn_blocking(move || {
                gdk::Texture::from_filename(&decode_path)
            })
            .await
            else {
                if let Some(this) = weak.upgrade() {
                    this.viewer.show_error(msg_id, generation, "Could not open image");
                }
                return;
            };
            let Some(this) = weak.upgrade().filter(|this| {
                this.viewer_generation.get() == generation
                    && this.viewer.is_open()
                    && this.viewer.chat_id() == Some(chat_id)
            }) else {
                return;
            };
            this.info.set_thumbnail(chat_id, bind_generation, shared_generation, msg_id, path.clone(), &texture);
            this.viewer.set_path(msg_id, generation, path);
        });
    }

    fn save_viewer_media(self: Rc<Self>, msg_id: i32, generation: u64) {
        if self.viewer_generation.get() != generation {
            return;
        }
        let Some(source_path) = self.viewer_media_path(msg_id) else {
            return;
        };
        let Some(window) = self.window() else { return };
        let dialog = gtk::FileDialog::new();
        if let Some(name) = source_path.file_name().and_then(|name| name.to_str()) {
            dialog.set_initial_name(Some(name));
        }
        if let Some(pictures) = dirs::picture_dir() {
            dialog.set_initial_folder(Some(&gio::File::for_path(pictures)));
        }
        glib::MainContext::default().spawn_local(async move {
            let Ok(destination) = dialog.save_future(Some(&window)).await else {
                return;
            };
            if self.viewer_generation.get() != generation || !self.viewer.is_open() {
                return;
            }
            let source = gio::File::for_path(source_path);
            let (copy, _progress) = source.copy_future(
                &destination,
                gio::FileCopyFlags::OVERWRITE,
                glib::Priority::DEFAULT,
            );
            if let Err(error) = copy.await {
                self.viewer.show_error(msg_id, generation, error.message());
            }
        });
    }

    fn jump_to_message(self: Rc<Self>, msg_id: i32) {
        if self.messages.scroll_to_message(msg_id) {
            return;
        }
        let Some(chat_id) = self.open_chat.get().filter(|id| !is_virtual(*id)) else {
            return;
        };
        let epoch = self.epoch.get();
        let session_epoch = self.session_epoch.get();
        glib::MainContext::default().spawn_local(async move {
            let result = self.tg.get_messages(chat_id, vec![msg_id]).await;
            if !self.session_ready.get()
                || self.session_epoch.get() != session_epoch
                || !self.is_current(chat_id, epoch)
            {
                return;
            }
            match result {
                Ok(messages) => {
                    if let Some(message) = messages.into_iter().find(|message| message.id == msg_id)
                    {
                        self.jump_to_date_ensuring(message.ts, Some(message));
                    }
                }
                Err(error) => self.messages.show_error(&error),
            }
        });
    }

    fn open_edit_history(self: Rc<Self>, msg_id: i32) {
        let Some(chat_id) = self.open_chat.get() else {
            return;
        };
        let Some(message) = self.messages.message(msg_id) else {
            return;
        };
        if message.deleted || !message.edited || !self.settings.get().edit_history {
            return;
        }
        let epoch = self.epoch.get();
        let settings_gen = self.settings_gen.get();
        glib::MainContext::default().spawn_local(async move {
            let result = self.tg.get_edit_history(chat_id, msg_id).await;
            let valid = self.is_current(chat_id, epoch)
                && self.settings_gen.get() == settings_gen
                && self.settings.get().edit_history
                && self.messages.contains(msg_id)
                && !self.messages.is_deleted(msg_id);
            if !valid {
                return;
            }
            match result {
                Ok(versions) => {
                    self.messages.show_edit_history(msg_id, versions);
                }
                Err(error) => {
                    shell_log!("get_edit_history({chat_id}, {msg_id}): {error}");
                    self.messages.show_error(&error);
                }
            }
        });
    }

    /// Jump-to-date: re-render the open chat around a historical day. The view
    /// stays `detached` (▼ always visible) until the user reloads the latest
    /// page; pagination upward from the jumped page keeps working (C3).
    fn jump_to_date(self: Rc<Self>, date: DateTime<Local>) {
        self.jump_to_date_ensuring(date, None);
    }

    fn jump_to_date_ensuring(self: Rc<Self>, date: DateTime<Local>, ensure: Option<Msg>) {
        let Some(chat_id) = self.open_chat.get() else {
            return;
        };
        if is_virtual(chat_id) {
            return;
        }
        self.close_forward();
        self.close_viewer();
        self.close_profile();
        let epoch = self.bump_epoch();
        // History-only reset: same chat, so the composer draft, reply/edit
        // mode, and busy sensitivity are preserved (C5/C13).
        self.messages.reset_history(chat_id, epoch);
        let this = self.clone();
        glib::MainContext::default().spawn_local(async move {
            match this.tg.get_history_at_date(chat_id, date).await {
                Ok(mut messages) => {
                    if !this.is_current(chat_id, epoch) {
                        return;
                    }
                    if let Some(message) =
                        ensure.filter(|message| !messages.iter().any(|item| item.id == message.id))
                    {
                        messages.push(message);
                    }
                    this.apply_tombstones(chat_id, &mut messages);
                    let inserted = this.messages.finish_initial(messages);
                    this.post_render(inserted);
                    this.messages.set_detached(true);
                }
                Err(error) => {
                    shell_log!("get_history_at_date({chat_id}): {error}");
                    if this.is_current(chat_id, epoch) {
                        this.messages.fail_initial(&error);
                        // The store was reset for the jump: keep the view
                        // detached so ▼ offers the way back to the latest page.
                        this.messages.set_detached(true);
                    }
                }
            }
        });
    }

    /// ▼ while detached: reload the latest page like an initial load (C1/C4).
    /// `detached` is cleared only after the latest page for the current epoch
    /// has actually loaded; on error the omg-error line shows and ▼ stays
    /// visible so the user can retry.
    fn jump_to_latest(self: Rc<Self>) {
        let Some(chat_id) = self.open_chat.get() else {
            return;
        };
        if is_virtual(chat_id) {
            return;
        }
        self.close_forward();
        self.close_viewer();
        self.close_profile();
        let epoch = self.bump_epoch();
        // History-only reset: same chat, so the composer draft, reply/edit
        // mode, and busy sensitivity are preserved (C5/C13).
        self.messages.reset_history(chat_id, epoch);
        let this = self.clone();
        glib::MainContext::default().spawn_local(async move {
            match this.tg.get_history(chat_id, None).await {
                Ok(mut messages) => {
                    if !this.is_current(chat_id, epoch) {
                        return;
                    }
                    this.apply_tombstones(chat_id, &mut messages);
                    if let Some(last) = messages.iter().rev().find(|message| !message.deleted) {
                        this.remember_last(last);
                    }
                    let inserted = this.messages.finish_initial(messages);
                    this.post_render(inserted);
                    this.messages.set_detached(false);
                    if this.window_is_active() {
                        let latest = this
                            .messages
                            .last_message()
                            .map(|message| message.id)
                            .unwrap_or(0);
                        this.queue_mark_read(chat_id, latest, epoch);
                    }
                }
                Err(error) => {
                    shell_log!("get_history({chat_id}): {error}");
                    if this.is_current(chat_id, epoch) {
                        this.messages.fail_initial(&error);
                        this.messages.set_detached(true);
                    }
                }
            }
        });
    }

    fn append_virtual(&self, chat_id: i64, text: String, outgoing: bool, monospace: bool) -> i32 {
        let message = {
            let mut stores = self.virtual_stores.borrow_mut();
            let store = stores.entry(chat_id).or_default();
            store.append(chat_id, text, outgoing, monospace)
        };
        if self.open_chat.get() == Some(chat_id) {
            self.messages.merge_event(message.clone());
            self.messages.set_monospace(message.id, monospace);
        }
        self.refresh_virtual_rows(&self.settings.get());
        self.update_virtual_status(chat_id);
        message.id
    }

    fn begin_virtual_request(&self, chat_id: i64) {
        if let Some(store) = self.virtual_stores.borrow_mut().get_mut(&chat_id) {
            store.in_flight = store.in_flight.saturating_add(1);
        }
        self.update_virtual_status(chat_id);
    }

    fn end_virtual_request(&self, chat_id: i64) {
        if let Some(store) = self.virtual_stores.borrow_mut().get_mut(&chat_id) {
            store.in_flight = store.in_flight.saturating_sub(1);
        }
        self.update_virtual_status(chat_id);
    }

    fn finish_virtual(&self, chat_id: i64, result: Result<String, String>, monospace: bool) {
        self.end_virtual_request(chat_id);
        // A local reply that lands while logging out must not touch the
        // (soon to be cleared) message view.
        if !self.session_ready.get() {
            return;
        }
        let text = match result {
            Ok(text) => text,
            Err(error) if chat_id == ASSISTANT_CHAT && error.contains("no chat provider") => {
                format!(
                    "{error}\nadd `anthropic_api_key = \"…\"` (or openai/groq/gemini) under `[ai]` in ~/.config/omarchygram/config.toml, or run `ollama serve`"
                )
            }
            Err(error) => error,
        };
        self.append_virtual(chat_id, text, false, monospace);
    }

    fn update_virtual_status(&self, chat_id: i64) {
        if self.open_chat.get() != Some(chat_id) {
            return;
        }
        let in_flight = self
            .virtual_stores
            .borrow()
            .get(&chat_id)
            .is_some_and(|store| store.in_flight > 0);
        self.messages.set_status(if in_flight {
            Some(if chat_id == ASSISTANT_CHAT {
                "thinking…"
            } else {
                "running…"
            })
        } else {
            None
        });
    }

    fn submit_virtual(self: Rc<Self>, chat_id: i64) {
        if !self.session_ready.get() {
            return;
        }
        let settings = self.settings.get();
        if (chat_id == ASSISTANT_CHAT && !settings.ai.enabled)
            || (chat_id == OMARCHY_CHAT && !settings.os.enabled)
        {
            return;
        }
        let text = self.messages.composer_text();
        if text.trim().is_empty() {
            return;
        }
        self.append_virtual(chat_id, text.clone(), true, false);
        if self.messages.composer_text() == text {
            self.messages.set_composer_text("");
            self.messages.cancel_all_modes();
        }
        if chat_id == ASSISTANT_CHAT {
            self.dispatch_assistant(text);
        } else {
            let lines: Vec<String> = text
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(str::to_string)
                .collect();
            for line in lines {
                self.clone().dispatch_omarchy(line);
            }
        }
    }

    fn dispatch_assistant(self: Rc<Self>, line: String) {
        if !self.settings.get().ai.enabled {
            return;
        }
        self.begin_virtual_request(ASSISTANT_CHAT);
        let trimmed = line.trim();
        if trimmed == "/help" {
            self.finish_virtual(
                ASSISTANT_CHAT,
                Ok("Assistant commands\n/help\n/status\n/catchup [chat title]\n/translate <lang> <text>\n/summarize <text>\n/search <question>".into()),
                false,
            );
            return;
        }
        if trimmed == "/status" {
            let prefs = ai_prefs(&self.settings.get());
            glib::MainContext::default().spawn_local(async move {
                let providers = self.local.detect(prefs).await;
                let text = providers
                    .into_iter()
                    .map(|provider| {
                        format!(
                            "{} ({}): {} — {}",
                            provider.id,
                            provider.task.label(),
                            if provider.available {
                                "available"
                            } else {
                                "unavailable"
                            },
                            provider.detail
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                self.finish_virtual(ASSISTANT_CHAT, Ok(text), false);
            });
            return;
        }
        if let Some(rest) = trimmed.strip_prefix("/catchup") {
            let query = rest.trim().to_lowercase();
            let target = if query.is_empty() {
                self.recent_real_chats.borrow().first().copied()
            } else {
                self.chatlist
                    .ordered()
                    .into_iter()
                    .filter(|(id, _)| !is_virtual(*id))
                    .find_map(|(id, title)| title.to_lowercase().contains(&query).then_some(id))
            };
            let Some(chat_id) = target else {
                self.finish_virtual(ASSISTANT_CHAT, Err("no matching real chat".into()), false);
                return;
            };
            let title = self.title_for(chat_id);
            let prefs = ai_prefs(&self.settings.get());
            glib::MainContext::default().spawn_local(async move {
                let result = match self.tg.get_history(chat_id, None).await {
                    Ok(messages) => {
                        let transcript = transcript(&messages, 50, false);
                        let (system, user) = prompts::catch_up(&title, &transcript);
                        self.local
                            .chat(
                                prefs,
                                system,
                                vec![ChatMessage {
                                    role: Role::User,
                                    content: user,
                                }],
                            )
                            .await
                            .map(|reply| reply.text)
                    }
                    Err(error) => Err(error),
                };
                self.finish_virtual(ASSISTANT_CHAT, result, false);
            });
            return;
        }
        if let Some(rest) = trimmed.strip_prefix("/translate ") {
            let Some((lang, text)) = rest.trim().split_once(char::is_whitespace) else {
                self.finish_virtual(
                    ASSISTANT_CHAT,
                    Err("usage: /translate <lang> <text>".into()),
                    false,
                );
                return;
            };
            let (system, user) = prompts::translate(text.trim(), lang);
            self.spawn_assistant_chat(system, user);
            return;
        }
        if let Some(text) = trimmed.strip_prefix("/summarize ") {
            let (system, user) = prompts::summarize(text.trim());
            self.spawn_assistant_chat(system, user);
            return;
        }
        if let Some(question) = trimmed.strip_prefix("/search ") {
            let targets: Vec<(i64, String)> = self
                .chatlist
                .ordered()
                .into_iter()
                .filter(|(id, _)| !is_virtual(*id))
                .take(10)
                .collect();
            let prefs = ai_prefs(&self.settings.get());
            let question = question.trim().to_string();
            glib::MainContext::default().spawn_local(async move {
                let mut handles = Vec::new();
                for (chat_id, title) in targets {
                    let tg = self.tg.clone();
                    handles.push(
                        glib::MainContext::default().spawn_local(async move {
                            (title, tg.get_history(chat_id, None).await)
                        }),
                    );
                }
                let mut candidates = Vec::new();
                for handle in handles {
                    if let Ok((title, Ok(messages))) = handle.await {
                        candidates.push(search_transcript(&title, &messages));
                    }
                }
                let (system, user) = prompts::search(&question, &candidates.join("\n"));
                let result = self
                    .local
                    .chat(
                        prefs,
                        system,
                        vec![ChatMessage {
                            role: Role::User,
                            content: user,
                        }],
                    )
                    .await
                    .map(|reply| reply.text);
                self.finish_virtual(ASSISTANT_CHAT, result, false);
            });
            return;
        }

        let messages = {
            let stores = self.virtual_stores.borrow();
            stores
                .get(&ASSISTANT_CHAT)
                .map(|store| {
                    store
                        .msgs
                        .iter()
                        .rev()
                        .take(20)
                        .collect::<Vec<_>>()
                        .into_iter()
                        .rev()
                        .map(|message| ChatMessage {
                            role: if message.outgoing {
                                Role::User
                            } else {
                                Role::Assistant
                            },
                            content: message.text.clone(),
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        };
        let prefs = ai_prefs(&self.settings.get());
        glib::MainContext::default().spawn_local(async move {
            let result = self
                .local
                .chat(prefs, prompts::ASSISTANT.to_string(), messages)
                .await
                .map(|reply| reply.text);
            self.finish_virtual(ASSISTANT_CHAT, result, false);
        });
    }

    fn spawn_assistant_chat(self: Rc<Self>, system: String, user: String) {
        let prefs = ai_prefs(&self.settings.get());
        glib::MainContext::default().spawn_local(async move {
            let result = self
                .local
                .chat(
                    prefs,
                    system,
                    vec![ChatMessage {
                        role: Role::User,
                        content: user,
                    }],
                )
                .await
                .map(|reply| reply.text);
            self.finish_virtual(ASSISTANT_CHAT, result, false);
        });
    }

    fn dispatch_omarchy(self: Rc<Self>, line: String) {
        if !self.settings.get().os.enabled {
            return;
        }
        self.begin_virtual_request(OMARCHY_CHAT);
        match os::parse(&line) {
            Parsed::Empty => self.end_virtual_request(OMARCHY_CHAT),
            Parsed::Error(error) => {
                self.finish_virtual(OMARCHY_CHAT, Ok(error), false);
            }
            Parsed::Help => {
                let settings = self.settings.get();
                let actions = os::catalog(&settings.os.actions);
                self.finish_virtual(OMARCHY_CHAT, Ok(os::help_text(&actions)), false);
            }
            Parsed::List { filter } => {
                let settings = self.settings.get();
                let actions = os::catalog(&settings.os.actions);
                self.finish_virtual(OMARCHY_CHAT, Ok(os::list_text(&actions, &filter)), false);
            }
            Parsed::Run { name, args } => {
                let settings = self.settings.get();
                if !settings.os.enabled {
                    self.finish_virtual(
                        OMARCHY_CHAT,
                        Err("Omarchy actions are off — enable them in Settings".into()),
                        false,
                    );
                    return;
                }
                let actions = os::catalog(&settings.os.actions);
                let exact = actions.iter().find(|action| action.name == name).cloned();
                let action = exact.or_else(|| {
                    let lower = name.to_lowercase();
                    let matches: Vec<_> = actions
                        .iter()
                        .filter(|action| action.name.to_lowercase().starts_with(&lower))
                        .cloned()
                        .collect();
                    (matches.len() == 1).then(|| matches[0].clone())
                });
                let Some(action) = action else {
                    self.finish_virtual(
                        OMARCHY_CHAT,
                        Ok(format!("unknown action `{name}` — try `list`")),
                        false,
                    );
                    return;
                };
                let local = self.local.clone();
                glib::MainContext::default().spawn_local(async move {
                    let current = self.settings.get();
                    if !current.os.enabled {
                        self.finish_virtual(
                            OMARCHY_CHAT,
                            Err("Omarchy actions are off — enable them in Settings".into()),
                            true,
                        );
                        return;
                    }
                    let policy = OsPolicy {
                        enabled: current.os.enabled,
                        shell: current.os.shell,
                    };
                    let result = local.os_run(action, args, policy).await;
                    self.finish_virtual(OMARCHY_CHAT, result, true);
                });
            }
            Parsed::Shell(command) => {
                if !self.settings.get().os.shell {
                    self.finish_virtual(
                        OMARCHY_CHAT,
                        Ok(
                            "shell commands are off — enable 'Allow shell commands' in Settings"
                                .into(),
                        ),
                        false,
                    );
                    return;
                }
                let ticket = match os::request_shell(&command) {
                    Ok(ticket) => PendingShellTicket::new(ticket),
                    Err(error) => {
                        self.finish_virtual(OMARCHY_CHAT, Ok(error), false);
                        return;
                    }
                };
                glib::MainContext::default().spawn_local(async move {
                    let answer = self.confirm_shell(ticket.command()).await;
                    if answer == 1 {
                        let current = self.settings.get();
                        if current.os.enabled && current.os.shell {
                            let ticket = ticket.consume();
                            let result = self.local.os_shell_confirmed(ticket).await;
                            self.finish_virtual(OMARCHY_CHAT, result, true);
                        } else {
                            self.finish_virtual(
                                OMARCHY_CHAT,
                                Ok("shell commands are off — enable 'Allow shell commands' in Settings".into()),
                                false,
                            );
                        }
                    } else {
                        self.end_virtual_request(OMARCHY_CHAT);
                    }
                });
            }
        }
    }

    async fn confirm_shell(&self, command: &str) -> usize {
        let dialog = gtk::AlertDialog::builder()
            .message(command)
            .buttons(["Cancel", "Run"])
            .default_button(0)
            .cancel_button(0)
            .build();
        if self.probe {
            return self.probe_answer.take().unwrap_or(0);
        }
        let Some(window) = self.window() else {
            return 0;
        };
        dialog
            .choose_future(Some(&window))
            .await
            .ok()
            .and_then(|answer| usize::try_from(answer).ok())
            .unwrap_or(0)
    }

    fn submit_composer(self: Rc<Self>) {
        if !self.session_ready.get() {
            return;
        }
        let kind = self.open_chat.get();
        if let Some(chat_id) = kind.filter(|chat_id| is_virtual(*chat_id)) {
            self.submit_virtual(chat_id);
            return;
        }
        if self.composer_operation.get() || self.messages.is_busy() {
            return;
        }
        let Some(chat_id) = kind else {
            return;
        };
        let text = self.messages.composer_text();
        if text.is_empty() {
            return;
        }
        let epoch = self.epoch.get();
        let title = self.title_for(chat_id);
        let edit_id = self.messages.edit_id();
        let reply_to = self.messages.reply_to();
        let Some(session_epoch) = self.begin_mutation() else {
            return;
        };
        let token = self.acquire_composer();
        self.messages.clear_error();
        self.messages.set_busy(true);
        self.messages.start_send_feedback();
        let send_button = self.messages.send_button();
        let charge = self.effects.send_pressed(send_button.upcast_ref());
        if let Some(msg_id) = edit_id {
            let was_last = self.messages.is_last(msg_id);
            glib::MainContext::default().spawn_local(async move {
                charge.await;
                let result = self.tg.edit_text(chat_id, msg_id, &text).await;
                self.finish_mutation();
                if !self.is_session_current(session_epoch) {
                    return;
                }
                let owns_composer = self.release_composer(token);
                match result {
                    Ok(message) => {
                        let message = self.apply_tombstone(message);
                        if was_last && !message.deleted {
                            self.remember_last(&message);
                            if self.is_tracked_last(chat_id, msg_id) {
                                self.dialog_upsert(
                                    chat_id,
                                    &title,
                                    &message.text,
                                    Some(message.ts),
                                    UnreadUpdate::Delta(0),
                                );
                            }
                        }
                        if self.is_current(chat_id, epoch) {
                            self.messages.merge_event(message);
                        }
                        let epoch_is_current = self.is_current(chat_id, epoch);
                        self.messages
                            .complete_text_operation(&text, epoch_is_current);
                    }
                    Err(error) => {
                        shell_log!("edit_text({chat_id}, {msg_id}): {error}");
                        if self.is_current(chat_id, epoch) {
                            self.messages.show_error(&error);
                            self.effects.error_flash(&self.overlay);
                        }
                    }
                }
                if owns_composer {
                    self.messages.stop_send_feedback();
                    self.messages.set_busy(false);
                }
            });
            return;
        }

        let pending_id = self.pending_message_id.get();
        self.pending_message_id.set(pending_id.saturating_sub(1));
        let pending = Msg {
            id: pending_id,
            chat_id,
            chat_title: title.clone(),
            sender: "You".to_string(),
            text: text.clone(),
            ts: Local::now(),
            outgoing: true,
            reply_to,
            ..Msg::default()
        };
        let inserted = self.messages.merge_pending(pending);
        self.post_render(inserted);

        glib::MainContext::default().spawn_local(async move {
            charge.await;
            let result = self.tg.send_text(chat_id, &text, reply_to).await;
            self.finish_mutation();
            if !self.is_session_current(session_epoch) {
                return;
            }
            let owns_composer = self.release_composer(token);
            match result {
                Ok(message) => {
                    let message = self.apply_tombstone(message);
                    if !message.deleted {
                        self.remember_last(&message);
                        self.dialog_upsert(
                            chat_id,
                            &title,
                            &message_preview(&message),
                            Some(message.ts),
                            UnreadUpdate::Delta(0),
                        );
                    }
                    if self.is_current(chat_id, epoch) {
                        self.messages.remove(pending_id);
                        let inserted = self.messages.merge_event(message);
                        self.post_render(inserted);
                    }
                    let epoch_is_current = self.is_current(chat_id, epoch);
                    self.messages
                        .complete_text_operation(&text, epoch_is_current);
                    self.clear_sent_draft(chat_id);
                }
                Err(error) => {
                    shell_log!("send_text({chat_id}): {error}");
                    if self.is_current(chat_id, epoch) {
                        self.messages.remove(pending_id);
                        self.messages.show_error(&error);
                        self.effects.error_flash(&self.overlay);
                    }
                }
            }
            if owns_composer {
                self.messages.stop_send_feedback();
                self.messages.set_busy(false);
            }
        });
    }

    fn open_file_dialog(self: Rc<Self>) {
        if !self.session_ready.get() {
            return;
        }
        if self.composer_operation.get() || self.messages.is_busy() {
            return;
        }
        let Some(window) = self.window() else {
            return;
        };
        let Some(chat_id) = self.open_chat.get() else {
            return;
        };
        let epoch = self.epoch.get();
        let session_epoch = self.session_epoch.get();
        let title = self.title_for(chat_id);
        let token = self.acquire_composer();
        self.messages.clear_error();
        self.messages.set_busy(true);
        let dialog = gtk::FileDialog::new();
        glib::MainContext::default().spawn_local(async move {
            match dialog.open_future(Some(&window)).await {
                Ok(file) => {
                    self.open_caption_dialog(file, chat_id, epoch, session_epoch, token, title);
                }
                Err(error) if error.matches(gio::IOErrorEnum::Cancelled) => {
                    if self.is_session_current(session_epoch) && self.release_composer(token) {
                        self.messages.set_busy(false);
                    }
                }
                Err(error) => {
                    shell_log!("file dialog: {error}");
                    if self.is_session_current(session_epoch) && self.is_current(chat_id, epoch) {
                        self.messages.show_error(error.message());
                    }
                    if self.is_session_current(session_epoch) && self.release_composer(token) {
                        self.messages.set_busy(false);
                    }
                }
            }
        });
    }

    fn send_file(self: Rc<Self>, file: gio::File) {
        if !self.session_ready.get() {
            return;
        }
        if self.composer_operation.get() || self.messages.is_busy() {
            return;
        }
        let Some(chat_id) = self.open_chat.get() else {
            return;
        };
        let epoch = self.epoch.get();
        let session_epoch = self.session_epoch.get();
        let title = self.title_for(chat_id);
        let token = self.acquire_composer();
        self.messages.clear_error();
        self.messages.set_busy(true);
        self.open_caption_dialog(file, chat_id, epoch, session_epoch, token, title);
    }

    fn open_caption_dialog(
        self: Rc<Self>,
        file: gio::File,
        chat_id: i64,
        epoch: u64,
        session_epoch: u64,
        token: u64,
        title: String,
    ) {
        let Some(path) = file.path() else {
            if self.is_current(chat_id, epoch) {
                self.messages.show_error("only local files can be sent");
            }
            if self.release_composer(token) {
                self.messages.set_busy(false);
            }
            return;
        };
        let Some(window) = self.window() else {
            if self.release_composer(token) {
                self.messages.set_busy(false);
            }
            return;
        };
        self.close_forward();
        self.close_viewer();
        self.close_profile();
        self.close_contacts();
        self.close_new_group();
        self.close_stickers();
        self.close_caption_dialog();

        let dialog = gtk::Window::builder()
            .title("Send file")
            .transient_for(&window)
            .modal(true)
            .destroy_with_parent(true)
            .default_width(380)
            .resizable(false)
            .build();
        dialog.add_css_class("omg-window");
        let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
        content.add_css_class("omg-caption-dialog");
        let filename = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("file");
        let file_label = gtk::Label::new(Some(filename));
        file_label.set_halign(gtk::Align::Start);
        file_label.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        content.append(&file_label);
        let composer_snapshot = self.messages.composer_text();
        let caption = gtk::Entry::new();
        caption.set_placeholder_text(Some("Caption"));
        caption.set_text(&composer_snapshot);
        caption.set_activates_default(true);
        content.append(&caption);

        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        actions.set_halign(gtk::Align::End);
        let later = gtk::Button::with_label("Send later…");
        actions.append(&later);
        let cancel = gtk::Button::with_label("Cancel");
        let send = gtk::Button::with_label("Send");
        send.add_css_class("suggested-action");
        actions.append(&cancel);
        actions.append(&send);
        content.append(&actions);
        dialog.set_child(Some(&content));

        let accepted = Rc::new(Cell::new(false));
        // Cloned up front: `send.connect_clicked` below moves `accepted`,
        // `file` and `composer_snapshot` into its closure.
        let accepted_for_later = accepted.clone();
        let file_for_later = file.clone();
        let composer_for_later = composer_snapshot.clone();
        let weak = Rc::downgrade(&self);
        let accepted_for_close = accepted.clone();
        dialog.connect_close_request(move |dialog| {
            // The caption Entry lives in this dialog's own toplevel, so its
            // GtkText only gets a focus-out if THIS window drops the focus
            // while the entry is still mapped. Without it GTK warns from the
            // cursor-blink tick after the window is gone.
            gtk::prelude::GtkWindowExt::set_focus(dialog, None::<&gtk::Widget>);
            if let Some(this) = weak.upgrade() {
                this.caption_dialog.borrow_mut().take();
                this.apply_info_layout(this.current_window_width());
                if !accepted_for_close.get() && this.release_composer(token) {
                    this.messages.set_busy(false);
                    this.messages.focus_composer();
                }
            }
            glib::Propagation::Proceed
        });
        let dialog_for_cancel = dialog.clone();
        cancel.connect_clicked(move |_| dialog_for_cancel.close());

        let weak = Rc::downgrade(&self);
        let dialog_for_send = dialog.clone();
        let caption_for_send = caption.clone();
        send.connect_clicked(move |_| {
            let Some(this) = weak.upgrade() else { return };
            if !this.is_session_current(session_epoch) || !this.is_current(chat_id, epoch) {
                dialog_for_send.close();
                return;
            }
            accepted.set(true);
            this.caption_dialog.borrow_mut().take();
            gtk::prelude::GtkWindowExt::set_focus(&dialog_for_send, None::<&gtk::Widget>);
            dialog_for_send.close();
            this.apply_info_layout(this.current_window_width());
            let caption = caption_for_send.text().to_string();
            let this_for_send = this.clone();
            let file = file.clone();
            let title = title.clone();
            let composer_snapshot = composer_snapshot.clone();
            glib::MainContext::default().spawn_local(async move {
                this_for_send
                    .send_file_snapshot(
                        file, FileSendContext { chat_id, epoch, session_epoch, token },
                        caption,
                        composer_snapshot,
                        title,
                    )
                    .await;
            });
        });
        let send_for_activate = send.clone();
        caption.connect_activate(move |_| send_for_activate.emit_clicked());

        // "Send later…": the same picker the SEND button uses, anchored on
        // this button; scheduling closes the dialog like a normal send.
        let weak = Rc::downgrade(&self);
        let dialog_for_later = dialog.clone();
        let caption_for_later = caption.clone();
        let later_slot: Rc<RefCell<Option<gtk::Popover>>> = Rc::new(RefCell::new(None));
        let later_slot_for_click = later_slot.clone();
        later.connect_clicked(move |later| {
            let Some(this) = weak.upgrade() else { return };
            if let Some(old) = later_slot_for_click.borrow_mut().take() {
                old.popdown();
                if old.parent().is_some() {
                    old.unparent();
                }
            }
            let weak = Rc::downgrade(&this);
            let dialog_for_pick = dialog_for_later.clone();
            let caption_for_pick = caption_for_later.clone();
            let accepted_for_pick = accepted_for_later.clone();
            let file_for_pick = file_for_later.clone();
            let composer_for_pick = composer_for_later.clone();
            let slot_for_pick = later_slot_for_click.clone();
            let picker = SendLaterPopover::new(Rc::new(move |at| {
                let Some(this) = weak.upgrade() else { return };
                if let Some(popover) = slot_for_pick.borrow_mut().take() {
                    popover.popdown();
                    if popover.parent().is_some() {
                        popover.unparent();
                    }
                }
                if !this.is_session_current(session_epoch) || !this.is_current(chat_id, epoch) {
                    dialog_for_pick.close();
                    return;
                }
                let Some(path) = file_for_pick.path() else {
                    this.messages.show_error("only local files can be sent");
                    dialog_for_pick.close();
                    return;
                };
                accepted_for_pick.set(true);
                this.caption_dialog.borrow_mut().take();
                gtk::prelude::GtkWindowExt::set_focus(&dialog_for_pick, None::<&gtk::Widget>);
                dialog_for_pick.close();
                this.apply_info_layout(this.current_window_width());
                this.clone().send_file_later(
                    path, FileSendContext { chat_id, epoch, session_epoch, token },
                    caption_for_pick.text().to_string(),
                    composer_for_pick.clone(),
                    at,
                );
            }));
            picker.popover.set_parent(later);
            picker.popover.popup();
            *later_slot_for_click.borrow_mut() = Some(picker.popover.clone());
        });
        let later_slot_for_close = later_slot.clone();
        dialog.connect_destroy(move |_| {
            if let Some(popover) = later_slot_for_close.borrow_mut().take() {
                popover.popdown();
                if popover.parent().is_some() {
                    popover.unparent();
                }
            }
        });
        let escape = gtk::EventControllerKey::new();
        let dialog_for_escape = dialog.clone();
        escape.connect_key_pressed(move |_, key, _, _| {
            if key == gdk::Key::Escape {
                dialog_for_escape.close();
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
        dialog.add_controller(escape);
        *self.caption_dialog.borrow_mut() = Some(dialog.clone());
        self.apply_info_layout(self.current_window_width());
        dialog.present();
        caption.grab_focus();
    }

    fn close_caption_dialog(&self) {
        if let Some(dialog) = self.caption_dialog.borrow_mut().take() {
            dialog.close();
        }
    }

    async fn send_file_snapshot(
        self: Rc<Self>,
        file: gio::File,
        context: FileSendContext,
        caption: String,
        composer_snapshot: String,
        title: String,
    ) {
        let FileSendContext { chat_id, epoch, session_epoch, token } = context;
        if !self.is_session_current(session_epoch) || self.begin_mutation().is_none() {
            return;
        }
        let Some(path) = file.path() else {
            if self.is_current(chat_id, epoch) {
                self.messages.show_error("only local files can be sent");
            }
            self.finish_mutation();
            if self.release_composer(token) && self.is_session_current(session_epoch) {
                self.messages.set_busy(false);
            }
            return;
        };
        let result = self.tg.send_file(chat_id, path, &caption).await;
        self.finish_mutation();
        if !self.is_session_current(session_epoch) {
            return;
        }
        let owns_composer = self.release_composer(token);
        match result {
            Ok(message) => {
                let message = self.apply_tombstone(message);
                if !message.deleted {
                    self.remember_last(&message);
                    self.dialog_upsert(
                        chat_id,
                        &title,
                        &message_preview(&message),
                        Some(message.ts),
                        UnreadUpdate::Delta(0),
                    );
                }
                if self.is_current(chat_id, epoch) {
                    let inserted = self.messages.merge_event(message);
                    self.post_render(inserted);
                }
                let epoch_is_current = self.is_current(chat_id, epoch);
                self.messages
                    .complete_text_operation(&composer_snapshot, epoch_is_current);
            }
            Err(error) => {
                shell_log!("send_file({chat_id}): {error}");
                if self.is_current(chat_id, epoch) {
                    self.messages.show_error(&error);
                    self.effects.error_flash(&self.overlay);
                }
            }
        }
        if owns_composer {
            self.messages.set_busy(false);
        }
    }

    fn delete_message(self: Rc<Self>, msg_id: i32) {
        if !self.session_ready.get() {
            return;
        }
        let Some(chat_id) = self.open_chat.get() else {
            return;
        };
        let Some(message) = self.messages.message(msg_id) else {
            return;
        };
        if !message.outgoing {
            return;
        }
        let epoch = self.epoch.get();
        let was_last = self.messages.is_last(msg_id);
        let next_last = was_last
            .then(|| self.messages.last_before(msg_id))
            .flatten();
        let title = self.title_for(chat_id);
        let Some(session_epoch) = self.begin_mutation() else {
            return;
        };
        glib::MainContext::default().spawn_local(async move {
            let result = self.tg.delete_message(chat_id, msg_id).await;
            self.finish_mutation();
            if !self.is_session_current(session_epoch) {
                return;
            }
            match result {
                Ok(()) => {
                    if was_last {
                        let reconciled =
                            self.reconcile_deleted_last(chat_id, msg_id, next_last.clone());
                        let (preview, time) = reconciled
                            .as_ref()
                            .map(|message| (message_preview(message), Some(message.ts)))
                            .unwrap_or_else(|| (String::new(), None));
                        self.dialog_upsert(chat_id, &title, &preview, time, UnreadUpdate::Delta(0));
                    }
                    if self.is_current(chat_id, epoch)
                        && self.messages.animate_deleted(msg_id) {
                            glib::timeout_future(Duration::from_millis(500)).await;
                        }
                    if self.is_current(chat_id, epoch) {
                        self.messages.remove(msg_id);
                    }
                }
                Err(error) => {
                    shell_log!("delete_message({chat_id}, {msg_id}): {error}");
                    if self.is_current(chat_id, epoch) {
                        self.messages.show_error(&error);
                    }
                }
            }
            self.finish_chat_mutation();
        });
    }

    fn confirm_delete_selected(self: Rc<Self>) {
        if !self.messages.selection_all_outgoing() {
            return;
        }
        let ids = self.messages.selection_ids();
        if ids.is_empty() {
            return;
        }
        let Some(chat_id) = self.open_chat.get().filter(|chat_id| !is_virtual(*chat_id)) else {
            return;
        };
        let Some(window) = self.window() else { return };
        let dialog = gtk::AlertDialog::builder()
            .message(format!("Delete {} selected messages?", ids.len()))
            .buttons(["Cancel", "Delete"])
            .default_button(0)
            .cancel_button(0)
            .build();
        let epoch = self.epoch.get();
        let title = self.title_for(chat_id);
        glib::MainContext::default().spawn_local(async move {
            if dialog.choose_future(Some(&window)).await.ok() != Some(1)
                || !self.is_current(chat_id, epoch)
            {
                return;
            }
            let Some(session_epoch) = self.begin_mutation() else {
                return;
            };
            let result = self.tg.delete_messages(chat_id, ids.clone()).await;
            self.finish_mutation();
            if !self.is_session_current(session_epoch) || !self.is_current(chat_id, epoch) {
                return;
            }
            match result {
                Ok(()) => {
                    let last_was_deleted = self
                        .messages
                        .last_id()
                        .is_some_and(|id| ids.contains(&id));
                    let next_last = self.messages.last_excluding(&ids);
                    self.messages.drop_selection_ids(&ids);
                    for msg_id in &ids {
                        self.messages.remove(*msg_id);
                    }
                    self.messages.exit_selection_mode();
                    if last_was_deleted {
                        let (preview, time) = next_last
                            .as_ref()
                            .map(|message| (message_preview(message), Some(message.ts)))
                            .unwrap_or_else(|| (String::new(), None));
                        self.dialog_upsert(
                            chat_id,
                            &title,
                            &preview,
                            time,
                            UnreadUpdate::Delta(0),
                        );
                    }
                }
                Err(error) => self.messages.show_error(&error),
            }
        });
    }

    fn draft_reply(self: Rc<Self>, msg_id: i32) {
        if !self.settings.get().ai.enabled {
            return;
        }
        let Some(chat_id) = self.open_chat.get().filter(|id| !is_virtual(*id)) else {
            return;
        };
        let Some(target) = self.messages.message(msg_id) else {
            return;
        };
        let epoch = self.epoch.get();
        let composer_snapshot = self.messages.composer_text();
        let token = {
            let mut aux = self.aux.borrow_mut();
            aux.draft_token = aux.draft_token.wrapping_add(1);
            aux.draft_token
        };
        let title = self.title_for(chat_id);
        let context = transcript(&self.messages.messages(), 20, false);
        let target_text = message_content(&target);
        let (system, user) =
            prompts::draft_reply(&title, &context, &target_text, &composer_snapshot);
        let prefs = ai_prefs(&self.settings.get());
        glib::MainContext::default().spawn_local(async move {
            let result = self
                .local
                .chat(
                    prefs,
                    system,
                    vec![ChatMessage {
                        role: Role::User,
                        content: user,
                    }],
                )
                .await;
            let current_token = self.aux.borrow().draft_token;
            if current_token != token
                || !self.is_current(chat_id, epoch)
                || !self.settings.get().ai.enabled
                || self.messages.composer_text() != composer_snapshot
                || !self.messages.contains(msg_id)
            {
                return;
            }
            match result {
                Ok(reply) => self.messages.show_ai_draft(&reply.text),
                Err(error) => {
                    shell_log!("AI draft ({chat_id}, {msg_id}): {error}");
                    self.messages.show_error(&error);
                }
            }
        });
    }

    fn translate_message(self: Rc<Self>, msg_id: i32) {
        if !self.settings.get().ai.enabled {
            return;
        }
        let Some(chat_id) = self.open_chat.get().filter(|id| !is_virtual(*id)) else {
            return;
        };
        let Some(message) = self.messages.message(msg_id) else {
            return;
        };
        let key = (chat_id, msg_id);
        {
            let mut aux = self.aux.borrow_mut();
            if matches!(
                aux.translations.get(&key),
                Some(ReqState::InFlight | ReqState::Done(_))
            ) {
                return;
            }
            aux.translations.insert(key, ReqState::InFlight);
        }
        self.messages.clear_aux(msg_id);
        self.render_aux_for(msg_id);
        let (system, user) = prompts::translate(&message.text, "English");
        let prefs = ai_prefs(&self.settings.get());
        glib::MainContext::default().spawn_local(async move {
            let result = self
                .local
                .chat(
                    prefs,
                    system,
                    vec![ChatMessage {
                        role: Role::User,
                        content: user,
                    }],
                )
                .await
                .map(|reply| reply.text);
            match result {
                Ok(text) => {
                    self.aux
                        .borrow_mut()
                        .translations
                        .insert(key, ReqState::Done(text));
                    if self.open_chat.get() == Some(chat_id) {
                        self.render_aux_for(msg_id);
                    }
                }
                Err(error) => {
                    self.aux
                        .borrow_mut()
                        .translations
                        .insert(key, ReqState::Failed(error.clone()));
                    if self.open_chat.get() == Some(chat_id) && self.messages.contains(msg_id) {
                        self.messages.show_aux_error(msg_id, &error);
                    }
                }
            }
        });
    }

    fn summarize_message(self: Rc<Self>, msg_id: i32) {
        if !self.settings.get().ai.enabled {
            return;
        }
        let Some(chat_id) = self.open_chat.get().filter(|id| !is_virtual(*id)) else {
            return;
        };
        let Some(message) = self.messages.message(msg_id) else {
            return;
        };
        if message.text.chars().count() <= 300 {
            return;
        }
        let key = (chat_id, msg_id);
        {
            let mut aux = self.aux.borrow_mut();
            if matches!(
                aux.summaries.get(&key),
                Some(ReqState::InFlight | ReqState::Done(_))
            ) {
                return;
            }
            aux.summaries.insert(key, ReqState::InFlight);
        }
        self.messages.clear_aux(msg_id);
        self.render_aux_for(msg_id);
        let (system, user) = prompts::summarize(&message.text);
        let prefs = ai_prefs(&self.settings.get());
        glib::MainContext::default().spawn_local(async move {
            let result = self
                .local
                .chat(
                    prefs,
                    system,
                    vec![ChatMessage {
                        role: Role::User,
                        content: user,
                    }],
                )
                .await
                .map(|reply| reply.text);
            match result {
                Ok(text) => {
                    self.aux
                        .borrow_mut()
                        .summaries
                        .insert(key, ReqState::Done(text));
                    if self.open_chat.get() == Some(chat_id) {
                        self.render_aux_for(msg_id);
                    }
                }
                Err(error) => {
                    self.aux
                        .borrow_mut()
                        .summaries
                        .insert(key, ReqState::Failed(error.clone()));
                    if self.open_chat.get() == Some(chat_id) && self.messages.contains(msg_id) {
                        self.messages.show_aux_error(msg_id, &error);
                    }
                }
            }
        });
    }

    fn post_render(self: &Rc<Self>, ids: Vec<i32>) {
        self.resolve_missing_quotes();
        self.start_image_downloads(ids.clone());
        let settings = self.settings.get();
        for msg_id in ids {
            self.render_aux_for(msg_id);
            if self.messages.media_kind(msg_id) != Some(MediaKind::Voice) || !settings.ai.enabled {
                continue;
            }
            let state = self.open_chat.get().and_then(|chat_id| {
                self.aux
                    .borrow()
                    .transcripts
                    .get(&(chat_id, msg_id))
                    .cloned()
            });
            match state {
                Some(ReqState::InFlight) if !self.messages.has_media_continuation(msg_id) => {
                    let key = (self.open_chat.get().unwrap_or_default(), msg_id);
                    if !self.transcription_active.borrow().contains(&key) {
                        self.clone().arm_transcription(msg_id);
                    }
                }
                None if settings.ai.transcribe_auto => {
                    self.clone().request_transcription(msg_id);
                }
                _ => {}
            }
        }
        if self.messages.search_is_open() {
            self.refresh_in_chat_search();
        }
    }

    fn resolve_missing_quotes(self: &Rc<Self>) {
        let Some(chat_id) = self.open_chat.get().filter(|id| !is_virtual(*id)) else {
            return;
        };
        let epoch = self.epoch.get();
        let requested = self
            .messages
            .missing_reply_ids()
            .into_iter()
            .filter(|msg_id| {
                self.quote_requests
                    .borrow_mut()
                    .insert((chat_id, epoch, *msg_id))
            })
            .collect::<Vec<_>>();
        if requested.is_empty() {
            return;
        }
        let session_epoch = self.session_epoch.get();
        let tg = self.tg.clone();
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let result = tg.get_messages(chat_id, requested.clone()).await;
            let Some(this) = weak.upgrade() else { return };
            {
                let mut in_flight = this.quote_requests.borrow_mut();
                for msg_id in &requested {
                    in_flight.remove(&(chat_id, epoch, *msg_id));
                }
            }
            if !this.session_ready.get()
                || this.session_epoch.get() != session_epoch
                || !this.is_current(chat_id, epoch)
            {
                return;
            }
            match result {
                Ok(messages) => this.messages.fill_quote_messages(messages),
                Err(error) => eprintln!("get_messages quotes ({chat_id}): {error}"),
            }
        });
    }

    fn render_aux_for(&self, msg_id: i32) {
        let Some(chat_id) = self.open_chat.get() else {
            return;
        };
        let key = (chat_id, msg_id);
        let (transcript, translation, summary) = {
            let aux = self.aux.borrow();
            (
                done_text(aux.transcripts.get(&key)),
                done_text(aux.translations.get(&key)),
                done_text(aux.summaries.get(&key)),
            )
        };
        self.messages.render_aux(
            msg_id,
            transcript.as_deref(),
            translation.as_deref(),
            summary.as_deref(),
        );
    }

    fn arm_visible_transcriptions(self: &Rc<Self>) {
        let Some(chat_id) = self.open_chat.get().filter(|id| !is_virtual(*id)) else {
            return;
        };
        let ids: Vec<i32> = self
            .messages
            .messages()
            .into_iter()
            .filter(|message| message.media == Some(MediaKind::Voice))
            .map(|message| message.id)
            .collect();
        for msg_id in ids {
            let key = (chat_id, msg_id);
            let state = self.aux.borrow().transcripts.get(&key).cloned();
            match state {
                Some(ReqState::InFlight)
                    if !self.messages.has_media_continuation(msg_id)
                        && !self.transcription_active.borrow().contains(&key) =>
                {
                    self.clone().arm_transcription(msg_id);
                }
                None => self.clone().request_transcription(msg_id),
                Some(ReqState::InFlight | ReqState::Done(_) | ReqState::Failed(_)) => {}
            }
        }
    }

    fn request_transcription(self: Rc<Self>, msg_id: i32) {
        if !self.settings.get().ai.enabled {
            return;
        }
        let Some(chat_id) = self.open_chat.get().filter(|id| !is_virtual(*id)) else {
            return;
        };
        if self.messages.media_kind(msg_id) != Some(MediaKind::Voice) {
            return;
        }
        let key = (chat_id, msg_id);
        {
            let mut aux = self.aux.borrow_mut();
            if matches!(
                aux.transcripts.get(&key),
                Some(ReqState::InFlight | ReqState::Done(_))
            ) {
                return;
            }
            aux.transcripts.insert(key, ReqState::InFlight);
        }
        // Remove a prior transcript error while preserving any completed
        // translation or summary already rendered for this message.
        self.render_aux_for(msg_id);
        self.arm_transcription(msg_id);
    }

    fn arm_transcription(self: Rc<Self>, msg_id: i32) {
        let Some(chat_id) = self.open_chat.get().filter(|id| !is_virtual(*id)) else {
            return;
        };
        if self.messages.has_media_continuation(msg_id) {
            return;
        }
        let weak = Rc::downgrade(&self);
        if !self.messages.on_media_ready(msg_id, move |path| {
            if let Some(this) = weak.upgrade() {
                this.start_transcription_path(chat_id, msg_id, path);
            }
        }) {
            self.fail_transcription(chat_id, msg_id, "voice message is unavailable".into());
            return;
        }
        if matches!(
            self.messages.media_state(msg_id),
            Some(MediaState::NotStarted | MediaState::Failed)
        ) {
            self.clone().start_media_download(msg_id, false);
            if matches!(self.messages.media_state(msg_id), Some(MediaState::Failed)) {
                self.messages.drop_media_continuations(msg_id);
                self.fail_transcription(chat_id, msg_id, "voice message is unavailable".into());
            }
        }
    }

    fn start_transcription_path(self: Rc<Self>, chat_id: i64, msg_id: i32, path: PathBuf) {
        if !matches!(
            self.aux.borrow().transcripts.get(&(chat_id, msg_id)),
            Some(ReqState::InFlight)
        ) {
            return;
        }
        if !self
            .transcription_active
            .borrow_mut()
            .insert((chat_id, msg_id))
        {
            return;
        }
        let prefs = ai_prefs(&self.settings.get());
        glib::MainContext::default().spawn_local(async move {
            let result = self.local.transcribe(prefs, path).await;
            self.transcription_active
                .borrow_mut()
                .remove(&(chat_id, msg_id));
            match result {
                Ok(transcript) => {
                    self.aux
                        .borrow_mut()
                        .transcripts
                        .insert((chat_id, msg_id), ReqState::Done(transcript.text));
                    if self.open_chat.get() == Some(chat_id) {
                        self.render_aux_for(msg_id);
                    }
                }
                Err(error) => self.fail_transcription(chat_id, msg_id, error),
            }
        });
    }

    fn fail_transcription(&self, chat_id: i64, msg_id: i32, error: String) {
        let key = (chat_id, msg_id);
        if !matches!(
            self.aux.borrow().transcripts.get(&key),
            Some(ReqState::InFlight)
        ) {
            return;
        }
        self.aux
            .borrow_mut()
            .transcripts
            .insert(key, ReqState::Failed(error.clone()));
        if self.open_chat.get() == Some(chat_id) && self.messages.contains(msg_id) {
            self.messages.show_aux_error(msg_id, &error);
        }
    }

    fn start_image_downloads(self: &Rc<Self>, ids: Vec<i32>) {
        let map_tiles = self.settings.get().media.map_tiles;
        for msg_id in ids {
            if matches!(
                self.messages.media_kind(msg_id),
                Some(MediaKind::Photo | MediaKind::Sticker)
            ) || (map_tiles
                && matches!(
                    self.messages.media_kind(msg_id),
                    Some(MediaKind::Location | MediaKind::Venue)
                )
                && self.messages.geo_needs_map(msg_id))
            {
                self.clone().start_media_download(msg_id, false);
            }
        }
    }

    fn media_action(self: Rc<Self>, msg_id: i32, intent: player::OpenIntent) {
        if self.messages.media_kind(msg_id) == Some(MediaKind::Photo) {
            match self.messages.media_state(msg_id) {
                Some(MediaState::Done(_)) => self.open_viewer(msg_id),
                Some(MediaState::NotStarted | MediaState::Failed) => {
                    let epoch = self.epoch.get();
                    let weak = Rc::downgrade(&self);
                    self.messages.on_media_ready(msg_id, move |_| {
                        if let Some(this) = weak.upgrade().filter(|this| this.epoch.get() == epoch)
                        {
                            this.open_viewer(msg_id);
                        }
                    });
                    self.start_media_download(msg_id, false);
                }
                Some(MediaState::InFlight) => {
                    let epoch = self.epoch.get();
                    let weak = Rc::downgrade(&self);
                    self.messages.on_media_ready(msg_id, move |_| {
                        if let Some(this) = weak.upgrade().filter(|this| this.epoch.get() == epoch)
                        {
                            this.open_viewer(msg_id);
                        }
                    });
                }
                None => {}
            }
            return;
        }
        match self.messages.media_kind(msg_id) {
            Some(
                MediaKind::Voice | MediaKind::Audio | MediaKind::Video | MediaKind::VideoNote | MediaKind::Gif,
            ) => {
                self.play_inline(msg_id, intent);
                return;
            }
            Some(MediaKind::Sticker) if self.messages.player_exists(msg_id) => {
                self.play_inline(msg_id, intent);
                return;
            }
            _ => {}
        }
        match self.messages.media_state(msg_id) {
            Some(MediaState::Done(path)) => self.launch_media(&path),
            Some(MediaState::NotStarted | MediaState::Failed) => {
                self.start_media_download(msg_id, true)
            }
            Some(MediaState::InFlight) | None => {}
        }
    }

    /// Bring a row into the viewport and keep it there: rows above it resize
    /// while their media lands, which moves the target under the viewport.
    async fn scroll_into_view(&self, msg_id: i32) -> bool {
        for _ in 0..20 {
            if self.messages.row_visible(msg_id) {
                return true;
            }
            self.messages.scroll_to_message(msg_id);
            glib::timeout_future(Duration::from_millis(150)).await;
        }
        self.messages.row_visible(msg_id)
    }

    /// In-app playback for voice/audio/video/video-note/gif (§2). Nothing here
    /// ever launches an external player — `launch_media` is for documents.
    fn play_inline(self: Rc<Self>, msg_id: i32, intent: player::OpenIntent) {
        if intent == player::OpenIntent::Manual {
            self.playback_generation.set(self.playback_generation.get().wrapping_add(1));
        }
        match self.messages.media_state(msg_id) {
            Some(MediaState::Done(path)) => match self.messages.player_state(msg_id) {
                // A live pipeline: the button is a play/pause toggle.
                player::PlayerState::Playing | player::PlayerState::Paused => {
                    if intent == player::OpenIntent::Manual {
                        self.messages.toggle_media(msg_id);
                    }
                }
                player::PlayerState::Error => {
                    self.messages.retry_failed_playback(msg_id);
                    self.clone().play_when_ready(msg_id, intent);
                    self.start_media_download(msg_id, false);
                }
                _ => self.messages.play_media(msg_id, path, intent),
            },
            Some(MediaState::NotStarted | MediaState::Failed) => {
                self.clone().play_when_ready(msg_id, intent);
                self.start_media_download(msg_id, false);
            }
            // A download the autoplay pass (or an earlier click) already
            // started: just make sure it plays when it lands.
            Some(MediaState::InFlight) => self.play_when_ready(msg_id, intent),
            None => {}
        }
    }

    fn play_when_ready(self: Rc<Self>, msg_id: i32, intent: player::OpenIntent) {
        let epoch = self.epoch.get();
        let playback_generation = self.playback_generation.get();
        let weak = Rc::downgrade(&self);
        self.messages.on_media_ready(msg_id, move |path| {
            if let Some(this) = weak.upgrade().filter(|this| this.epoch.get() == epoch
                && (intent != player::OpenIntent::Manual || this.playback_generation.get() == playback_generation)) {
                // A newer Play/Pause action supersedes any older queued start,
                // even if its row/player was reclaimed while downloading.
                // A late completion (a second download, a stale callback) must
                // not re-open a player that is already open: re-opening calls
                // activate(), which would pause whatever the user started since
                // (seen in the gate: the voice re-opened and paused the music).
                if this.messages.player_exists(msg_id)
                    && !matches!(this.messages.player_state(msg_id), player::PlayerState::None | player::PlayerState::Error)
                {
                    if intent == player::OpenIntent::Manual {
                        // A real click queued behind autoplay owns the now-open
                        // player. In particular, a muted video note restarts
                        // with manual/sound intent instead of losing the click.
                        this.messages.toggle_media(msg_id);
                    }
                    return;
                }
                let (intent, resume_on_visible) = if intent == player::OpenIntent::AutoplayMuted {
                    let settings = this.settings.get();
                    let enabled = match this.messages.media_kind(msg_id) {
                        Some(MediaKind::Gif | MediaKind::Sticker) => {
                            settings.media.autoplay_gifs && player::animations_on()
                        }
                        Some(MediaKind::VideoNote) => settings.media.autoplay_video_notes,
                        _ => false,
                    };
                    if enabled && this.messages.row_visible(msg_id) {
                        (player::OpenIntent::AutoplayMuted, false)
                    } else {
                        (player::OpenIntent::Poster, enabled)
                    }
                } else {
                    (intent, false)
                };
                this.messages.play_media(msg_id, path, intent);
                if resume_on_visible {
                    this.messages.defer_media_autoplay(msg_id);
                }
            }
        });
    }

    fn start_media_download(self: Rc<Self>, msg_id: i32, launch_on_ready: bool) {
        let Some(chat_id) = self.open_chat.get() else {
            return;
        };
        let epoch = self.epoch.get();
        let retry = matches!(self.messages.media_state(msg_id), Some(MediaState::Failed));
        let Some((kind, media_generation)) = self.messages.begin_media(msg_id) else {
            return;
        };
        glib::MainContext::default().spawn_local(async move {
            let result = if retry { self.tg.retry_media(chat_id, msg_id).await }
                else { self.tg.download_media(chat_id, msg_id).await };
            match result {
                // Wave 6D: a .tgs sticker is Lottie, not an image — it must
                // never reach the texture decoder below.
                Ok(Some(path)) if kind == MediaKind::Sticker && is_lottie(&path) => {
                    if !self.is_current(chat_id, epoch) || !self.messages.contains(msg_id) {
                        return;
                    }
                    self.messages.finish_lottie(msg_id, media_generation, path);
                }
                // A video sticker must bypass texture decoding and enter the
                // retained muted-loop MediaFile path (§2.2).
                Ok(Some(path))
                    if kind == MediaKind::Sticker
                        && path
                            .extension()
                            .is_some_and(|ext| ext.eq_ignore_ascii_case("webm")) =>
                {
                    if !self.is_current(chat_id, epoch) || !self.messages.contains(msg_id) {
                        return;
                    }
                    self.messages.finish_media_path(msg_id, media_generation, path);
                }
                Ok(Some(path)) if matches!(kind, MediaKind::Photo | MediaKind::Sticker | MediaKind::Location | MediaKind::Venue) => {
                    let decode_path = path.clone();
                    let decoded =
                        gio::spawn_blocking(move || gdk::Texture::from_filename(&decode_path))
                            .await;
                    if !self.is_current(chat_id, epoch) || !self.messages.contains(msg_id) {
                        return;
                    }
                    match decoded {
                        Ok(Ok(texture)) => {
                            self.messages
                                .finish_image(msg_id, media_generation, path, &texture);
                        }
                        Ok(Err(error)) => {
                            shell_log!("decode media ({chat_id}, {msg_id}): {error}");
                            if self.messages.fail_media(msg_id, media_generation, true) {
                                self.messages.show_error(error.message());
                            }
                        }
                        Err(_) => {
                            shell_log!("decode media ({chat_id}, {msg_id}): decoder failed");
                            if self.messages.fail_media(msg_id, media_generation, true) {
                                self.messages.show_error("image unavailable");
                            }
                        }
                    }
                }
                Ok(Some(path)) => {
                    if !self.is_current(chat_id, epoch) || !self.messages.contains(msg_id) {
                        return;
                    }
                    let accepted = self
                        .messages
                        .finish_media_path(msg_id, media_generation, path.clone());
                    if accepted && launch_on_ready {
                        self.launch_media(&path);
                    }
                }
                Ok(None) => {
                    if self.is_current(chat_id, epoch) && self.messages.contains(msg_id) {
                        if self.viewer.chat_id() == Some(chat_id) {
                            self.viewer.show_error(msg_id, self.viewer_generation.get(), "Image unavailable");
                        }
                        let transcription_requested = kind == MediaKind::Voice
                            && matches!(
                                self.aux.borrow().transcripts.get(&(chat_id, msg_id)),
                                Some(ReqState::InFlight)
                            );
                        let accepted =
                            self.messages.fail_media(msg_id, media_generation, false);
                        if accepted && transcription_requested && self.tg.is_mock {
                            self.clone().start_transcription_path(
                                chat_id,
                                msg_id,
                                PathBuf::from(format!("mock-voice-{chat_id}-{msg_id}.ogg")),
                            );
                        } else if accepted && transcription_requested {
                            self.fail_transcription(
                                chat_id,
                                msg_id,
                                "voice message is unavailable".into(),
                            );
                        }
                    }
                }
                Err(error) => {
                    if self.is_current(chat_id, epoch) && self.viewer.chat_id() == Some(chat_id) {
                        self.viewer.show_error(msg_id, self.viewer_generation.get(), "Could not load image");
                    }
                    shell_log!("download_media({chat_id}, {msg_id}): {error}");
                    if self.is_current(chat_id, epoch) && self.messages.contains(msg_id)
                        && self.messages.fail_media(msg_id, media_generation, true) {
                            self.messages.show_error(&error);
                            if kind == MediaKind::Voice {
                                self.fail_transcription(chat_id, msg_id, error);
                            }
                        }
                }
            }
        });
    }

    fn launch_media(&self, path: &PathBuf) {
        if self.probe {
            self.probe_media_launches
                .set(self.probe_media_launches.get().wrapping_add(1));
            return;
        }
        let uri = gio::File::for_path(path).uri();
        if let Err(error) =
            gio::AppInfo::launch_default_for_uri(&uri, None::<&gio::AppLaunchContext>)
        {
            shell_log!("launch media: {error}");
            self.messages.show_error(error.message());
        }
    }

    fn queue_mark_read(self: &Rc<Self>, chat_id: i64, latest: i32, epoch: u64) {
        if !self.session_ready.get() || is_virtual(chat_id) {
            return;
        }
        if self.manual_unread_hold.borrow().contains(&chat_id) {
            return;
        }
        // Ghost mode suppresses read receipts; check the CURRENT snapshot so
        // toggling it on takes effect immediately.
        if self.settings.get().ghost_mode {
            return;
        }
        let (should_spawn, sent_through) = {
            let mut states = self.mark_reads.borrow_mut();
            let state = states.entry(chat_id).or_default();
            if state.epoch != epoch {
                *state = ReadState {
                    epoch,
                    ..ReadState::default()
                };
            }
            state.latest = state.latest.max(latest);
            if state.in_flight {
                (false, state.sent_through)
            } else {
                state.in_flight = true;
                state.sent_through = state.latest;
                (true, state.sent_through)
            }
        };
        if !should_spawn {
            return;
        }
        let this = self.clone();
        glib::MainContext::default().spawn_local(async move {
            let result = this.tg.mark_read(chat_id, sent_through).await;
            let (same_epoch, latest_seen, follow_up) = {
                let mut states = this.mark_reads.borrow_mut();
                let state = states.entry(chat_id).or_default();
                if state.epoch == epoch {
                    state.in_flight = false;
                    (true, state.latest, state.latest > sent_through)
                } else {
                    (false, state.latest, false)
                }
            };
            match result {
                Ok(()) => {
                    if same_epoch
                        && latest_seen == sent_through
                        && this.is_current(chat_id, epoch)
                        && this.window_is_active()
                    {
                        this.chatlist.clear_unread(chat_id);
                    }
                }
                Err(error) => {
                    shell_log!("mark_read({chat_id}): {error}");
                    if this.is_current(chat_id, epoch) {
                        this.messages.show_error(&error);
                    }
                }
            }
            if follow_up && this.open_chat.get() == Some(chat_id) && this.window_is_active() {
                let next_epoch = this.epoch.get();
                this.queue_mark_read(chat_id, latest_seen, next_epoch);
            }
        });
    }

    fn bump_epoch(&self) -> u64 {
        // The timeout may already have fired and removed itself (GLib-CRITICAL
        // "Source ID … was not found" aborted a log-out under fatal-criticals).
        if let Some(source) = self.typing_timeout.borrow_mut().take()
            && let Some(live) = glib::MainContext::default().find_source_by_id(&source) {
                live.destroy();
            }
        let next = self.epoch.get().wrapping_add(1);
        self.epoch.set(next);
        next
    }

    /// Take the C5/A7 composer lock and return the token that owns it.
    fn acquire_composer(&self) -> u64 {
        let token = self.composer_token.get().wrapping_add(1);
        self.composer_token.set(token);
        self.composer_operation.set(true);
        token
    }

    /// Release the composer lock only when `token` still owns it. A late
    /// completion whose chat/epoch moved on (A7/A8) therefore leaves the lock
    /// — and the busy state that goes with it — to whoever took it since.
    fn release_composer(&self, token: u64) -> bool {
        if self.composer_token.get() != token {
            return false;
        }
        self.composer_operation.set(false);
        true
    }

    /// Drop the lock whoever owns it and invalidate every outstanding token
    /// (session reset / logout).
    fn force_release_composer(&self) {
        self.composer_token
            .set(self.composer_token.get().wrapping_add(1));
        self.composer_operation.set(false);
    }

    fn begin_mutation(&self) -> Option<u64> {
        if !self.session_ready.get() {
            return None;
        }
        let session_epoch = self.session_epoch.get();
        self.mutations_in_flight
            .set(self.mutations_in_flight.get().saturating_add(1));
        Some(session_epoch)
    }

    fn finish_mutation(&self) {
        self.mutations_in_flight
            .set(self.mutations_in_flight.get().saturating_sub(1));
    }

    fn is_session_current(&self, session_epoch: u64) -> bool {
        self.session_ready.get() && self.session_epoch.get() == session_epoch
    }

    fn apply_tombstone(&self, mut message: Msg) -> Msg {
        if self.settings.get().anti_delete {
            let mut tombstones = self.tombstones.borrow_mut();
            let ids = tombstones.entry(message.chat_id).or_default();
            if message.deleted {
                ids.insert(message.id);
            }
            if ids.contains(&message.id) {
                message.deleted = true;
            }
        }
        message
    }

    fn apply_tombstones(&self, chat_id: i64, messages: &mut Vec<Msg>) {
        if !self.settings.get().anti_delete {
            messages.retain(|message| !message.deleted);
            return;
        }
        let mut tombstones = self.tombstones.borrow_mut();
        let ids = tombstones.entry(chat_id).or_default();
        for message in messages.iter() {
            if message.deleted {
                ids.insert(message.id);
            }
        }
        for message in messages {
            if ids.contains(&message.id) {
                message.deleted = true;
            }
        }
    }

    fn tombstone_contains(&self, chat_id: i64, msg_id: i32) -> bool {
        self.tombstones
            .borrow()
            .get(&chat_id)
            .is_some_and(|ids| ids.contains(&msg_id))
    }

    fn tombstones_empty(&self, chat_id: i64) -> bool {
        self.tombstones
            .borrow()
            .get(&chat_id)
            .is_none_or(HashSet::is_empty)
    }

    fn flags_settled(&self) -> bool {
        !self.flags_in_flight.get()
            && self.flags_pending.borrow().is_none()
            && !self.anti_reload_pending.get()
    }

    /// True when `chat_id` (a plain dialog id, as chat-scoped events carry)
    /// addresses the open chat — including an open topic of that forum.
    fn open_chat_is(&self, chat_id: i64) -> bool {
        self.open_chat.get().map(dialog_id) == Some(chat_id)
    }

    fn is_current(&self, chat_id: i64, epoch: u64) -> bool {
        self.session_ready.get()
            && self.open_chat.get() == Some(chat_id)
            && self.epoch.get() == epoch
    }

    fn title_for(&self, chat_id: i64) -> String {
        let chat_id = dialog_id(chat_id);
        self.chatlist
            .ordered()
            .into_iter()
            .find_map(|(id, title)| (id == chat_id).then_some(title))
            .unwrap_or_else(|| "Unknown".to_string())
    }

    fn remember_last(&self, message: &Msg) {
        let mut last_by_chat = self.last_by_chat.borrow_mut();
        let should_replace = last_by_chat
            .get(&message.chat_id)
            .is_none_or(|current| message.id >= current.id);
        if should_replace {
            last_by_chat.insert(message.chat_id, message.clone());
        }
    }

    fn is_tracked_last(&self, chat_id: i64, msg_id: i32) -> bool {
        self.last_by_chat
            .borrow()
            .get(&chat_id)
            .is_some_and(|message| message.id == msg_id)
    }

    fn reconcile_deleted_last(
        &self,
        chat_id: i64,
        deleted_id: i32,
        fallback: Option<Msg>,
    ) -> Option<Msg> {
        let mut last_by_chat = self.last_by_chat.borrow_mut();
        if last_by_chat
            .get(&chat_id)
            .is_some_and(|message| message.id == deleted_id)
        {
            if let Some(message) = fallback {
                last_by_chat.insert(chat_id, message);
            } else {
                last_by_chat.remove(&chat_id);
            }
        }
        last_by_chat.get(&chat_id).cloned()
    }

    fn window(&self) -> Option<gtk::ApplicationWindow> {
        self.widget
            .root()?
            .downcast::<gtk::ApplicationWindow>()
            .ok()
    }

    fn clear_window_focus(&self) {
        if let Some(root) = self.widget.root() {
            root.set_focus(None::<&gtk::Widget>);
        }
    }

    fn window_is_active(&self) -> bool {
        self.window().is_some_and(|window| window.is_visible() && window.is_active())
    }

    fn start_auth_probe(self: &Rc<Self>) {
        if !self.probe || self.auth_probe_started.replace(true) {
            return;
        }
        let this = self.clone();
        glib::MainContext::default().spawn_local(async move {
            if this.auth.state() == AuthState::NeedCredentials {
                probe_step("credentials invalid disabled");
                this.auth.probe_fill_credentials("0", "not-a-hash");
                if this.auth.probe_continue_sensitive() {
                    probe_fail("credentials invalid enabled");
                    return;
                }

                probe_step("credentials valid enabled");
                this.auth.probe_fill_credentials("12345", PROBE_API_HASH);
                if !this.auth.probe_continue_sensitive() {
                    probe_fail("credentials valid disabled");
                    return;
                }
                this.auth.probe_submit_credentials();

                let fail_once = std::env::var("OMG_MOCK_FAIL_ONCE").is_ok_and(|value| {
                    value
                        .split(',')
                        .any(|name| name.trim() == "SubmitCredentials")
                });
                if fail_once {
                    probe_step("credentials inline retry");
                    if !poll_until(3000, || {
                        this.auth
                            .probe_credentials_error()
                            .is_some_and(|error| error.contains("mock: transient failure"))
                    })
                    .await
                    {
                        probe_fail("credentials inline error");
                        return;
                    }
                    let (api_id, api_hash) = this.auth.probe_credential_values();
                    if api_id != "12345"
                        || api_hash != PROBE_API_HASH
                        || !this.auth.probe_continue_sensitive()
                    {
                        probe_fail("credentials retry state");
                        return;
                    }
                    this.auth.probe_submit_credentials();
                }
                probe_step("credentials submit");
                if !poll_until(3000, || this.auth.state() == AuthState::NeedPhone).await {
                    probe_fail("credentials submit");
                    return;
                }
                probe_step("credentials secret absent from log");
                if log_ring_contains(PROBE_API_HASH) {
                    probe_fail("credentials secret reached log");
                    return;
                }
            }

            probe_step("auth phone");
            this.auth.probe_submit("123");
            if !poll_until(3000, || this.auth.state() == AuthState::NeedCode).await {
                probe_fail("auth phone");
                return;
            }
            probe_step("auth code");
            this.auth.probe_submit("2fa");
            if !poll_until(3000, || this.auth.state() == AuthState::NeedPassword).await {
                probe_fail("auth code");
                return;
            }
            probe_step("auth password");
            this.auth.probe_submit("x");
            if !poll_until(3000, || this.session_ready.get()).await {
                probe_fail("auth password");
            }
        });
    }

    fn start_probe(self: &Rc<Self>) {
        if !self.probe || self.probe_started.replace(true) {
            return;
        }
        let this = self.clone();
        glib::MainContext::default().spawn_local(async move {
            this.run_probe().await;
        });
    }

    async fn run_probe(self: Rc<Self>) {
        probe_step("stories preserve pending chat load");
        let dialogs_revision = self.dialogs_revision.get();
        self.handle_event(Event::StoriesChanged);
        if self.dialogs_revision.get() != dialogs_revision {
            probe_fail("story update invalidated pending chat list");
            return;
        }
        probe_step("load dialogs");
        if self.dialogs_in_flight.get() && !self.dialogs_loaded.get()
            && (!self.dialogs_error_box.is_visible() || !self.dialogs_spinner.is_visible()
                || self.dialogs_retry.is_visible())
        {
            probe_fail("initial dialog loading feedback");
            return;
        }
        if !poll_until(3000, || {
            self.dialogs_loaded.get()
                || (self.dialogs_error_box.is_visible() && self.dialogs_retry.is_visible())
        })
        .await
        {
            probe_fail("load dialogs");
            return;
        }
        if self.dialogs_error_box.is_visible() {
            probe_step("dialogs error retry");
            self.dialogs_retry.emit_clicked();
        }
        if !poll_until(3500, || {
            self.dialogs_loaded.get() && !self.chatlist.ordered().is_empty()
        })
        .await
        {
            probe_fail("dialogs retry");
            return;
        }

        probe_step("duplicate dialog snapshots keep one row per chat");
        let mut snapshot = self.chatlist.ordered_summaries().into_iter()
            .filter(|chat| !is_virtual(chat.id)).collect::<Vec<_>>();
        let expected = snapshot.len();
        snapshot.extend(snapshot.clone());
        self.chatlist.set_chats(snapshot);
        if self.chatlist.ordered_summaries().iter().filter(|chat| !is_virtual(chat.id)).count() != expected {
            probe_fail("duplicate dialog snapshot");
            return;
        }

        if let Some(restored) = self.probe_restored_ui_state {
            let RestoredUiState {
                sidebar_width: width,
                sidebar_collapsed: explicitly_collapsed,
                folder_id,
                info_panel_open,
                info_width,
            } = restored;
            probe_step("restored UI state");
            if !poll_until(3500, || {
                let window_width = self.window().map(|window| window.width()).unwrap_or(1100);
                let collapsed = explicitly_collapsed || window_width < 800;
                let position = if collapsed {
                    64
                } else {
                    clamp_sidebar_width(width, window_width)
                };
                self.ui_state.borrow().sidebar_width == width
                    && self.ui_state.borrow().sidebar_collapsed == explicitly_collapsed
                    && self.ui_state.borrow().folder_id == folder_id
                    && self.ui_state.borrow().info_panel_open == info_panel_open
                    && self.ui_state.borrow().info_width == info_width
                    && self.chatlist.mode() == SidebarMode::Dialogs(folder_id)
                    && self.effective_sidebar_collapsed.get() == collapsed
                    && self.paned.position() == position
            })
            .await
            {
                probe_fail("restored UI state");
                return;
            }
        }

        if std::env::var_os("OMG_PROBE_MEDIA_PROFILE_ONLY").is_some() {
            if self.run_media_profile_probe().await {
                probe_step("media and profiles PASS");
                if let Some(app) = self.window().and_then(|w| w.application()) { app.quit(); }
            }
            return;
        }

        if environment_listed("OMG_MOCK_SLOW", "GetDialogs")
            || environment_listed("OMG_MOCK_SLOW", "SearchGlobal")
            || environment_listed("OMG_MOCK_SLOW", "DownloadAvatar")
        {
            probe_step("slow dialogs search avatar generations");
            let marta = self
                .chatlist
                .ordered()
                .into_iter()
                .find_map(|(id, title)| (title == "Marta").then_some(id));
            let deni = self
                .chatlist
                .ordered()
                .into_iter()
                .find_map(|(id, title)| (title == "Deni").then_some(id));
            let (Some(marta), Some(deni)) = (marta, deni) else {
                probe_fail("slow generation fixtures");
                return;
            };
            self.load_dialogs();
            self.clone().open_chat(marta);
            self.clone().open_chat(deni);
            self.chatlist.set_search_text("green");
            glib::timeout_future(Duration::from_millis(400)).await;
            self.chatlist.clear_search();
            glib::timeout_future(Duration::from_millis(1900)).await;
            if self.chatlist.selected() != Some(deni)
                || self.messages.header_title() != "Deni"
                || self.messages.header_avatar_key() != deni
                || !self.chatlist.avatar_keys_match()
            {
                probe_fail("slow generation stale widget");
                return;
            }
        }

        probe_step("sidebar collapse");
        self.toggle_sidebar();
        if !poll_until(1000, || self.effective_sidebar_collapsed.get()).await {
            probe_fail("sidebar collapse");
            return;
        }
        self.toggle_sidebar();
        if !poll_until(1000, || !self.effective_sidebar_collapsed.get()).await {
            probe_fail("sidebar expand");
            return;
        }

        probe_step("search chats mar");
        self.chatlist.set_search_text("mar");
        if !poll_until(3000, || self.chatlist.search_counts().0 > 0).await {
            probe_fail("search chats mar");
            return;
        }
        if environment_listed("OMG_MOCK_FAIL_ONCE", "SearchGlobal") {
            if !poll_until(1500, || self.chatlist.search_has_message_error()).await {
                probe_fail("search messages transient error");
                return;
            }
            probe_step("search messages retry");
            self.chatlist.activate_message_retry();
            if !poll_until(3500, || self.chatlist.search_messages_ready()).await {
                probe_fail("search messages transient retry");
                return;
            }
        }
        probe_step("search messages green");
        self.chatlist.set_search_text("green");
        if !poll_until(3500, || {
            self.chatlist.search_counts().1 > 0 || self.chatlist.search_has_message_error()
        })
        .await
        {
            probe_fail("search messages green");
            return;
        }
        if self.chatlist.search_has_message_error() {
            probe_step("search messages retry");
            self.chatlist.activate_message_retry();
            if !poll_until(3500, || self.chatlist.search_counts().1 > 0).await {
                probe_fail("search messages retry");
                return;
            }
        }
        self.chatlist.clear_search();
        if !poll_until(1000, || self.chatlist.search_text().is_empty()).await {
            probe_fail("search escape clear");
            return;
        }

        probe_step("main menu");
        self.clone().open_main_menu();
        if !poll_until(1000, || self.main_menu.is_open()).await {
            probe_fail("main menu open");
            return;
        }
        self.main_menu.dismiss();

        probe_step("archived list");
        self.chatlist.show_archived();
        if !poll_until(1000, || {
            self.chatlist.mode() == SidebarMode::Archived
                && self.chatlist.visible_titles() == vec!["Old project".to_string()]
        })
        .await
        {
            probe_fail("archived list");
            return;
        }
        self.chatlist.show_dialogs_from_archive();

        probe_step("folder Work");
        if !poll_until(3000, || {
            self.chatlist.select_folder(1);
            let mut titles = self.chatlist.visible_titles();
            titles.sort();
            titles == vec!["Arch Linux ARM".to_string(), "Deni".to_string()]
        })
        .await
        {
            probe_fail("folder Work");
            return;
        }
        self.chatlist.select_folder(0);
        self.ui_state.borrow_mut().folder_id = 0;

        self.settings.update(|settings| {
            settings.ai.enabled = true;
            settings.os.enabled = true;
            settings.ai.ollama_url = "http://127.0.0.1:1".into();
        });
        probe_step("first chat");
        if !poll_until(1000, || {
            let ordered = self.chatlist.ordered();
            ordered.first().is_some_and(|(id, _)| *id == ASSISTANT_CHAT)
                && ordered.get(1).is_some_and(|(id, _)| *id == OMARCHY_CHAT)
        })
        .await
        {
            probe_fail("virtual chat prefix");
            return;
        }

        let Some((first_id, _)) = self
            .chatlist
            .ordered()
            .into_iter()
            .find(|(id, _)| !is_virtual(*id))
        else {
            probe_fail("first chat");
            return;
        };
        self.clone().open_chat(first_id);
        probe_step("find Marta");
        if !poll_until(3000, || {
            self.open_chat.get() == Some(first_id)
                && !self.messages.is_loading()
                && !self.messages.is_empty()
        })
        .await
        {
            probe_fail("open first chat");
            return;
        }

        let marta = self
            .chatlist
            .ordered()
            .into_iter()
            .find_map(|(id, title)| (title == "Marta").then_some(id));
        let Some(marta) = marta else {
            probe_fail("find Marta");
            return;
        };
        probe_step("row context menu");
        if !self.chatlist.open_row_menu(marta)
            || !poll_until(1000, || self.chatlist.row_menu_open()).await
        {
            probe_fail("row context menu");
            return;
        }
        self.chatlist.dismiss_popovers();

        let group = self
            .chatlist
            .ordered()
            .into_iter()
            .find_map(|(id, title)| (title == "Arch Linux ARM").then_some(id));
        let Some(group) = group else {
            probe_fail("find group");
            return;
        };
        self.clone().open_chat(group);
        probe_step("group sender names");
        if !poll_until(3500, || {
            self.open_chat.get() == Some(group)
                && !self.messages.is_loading()
                && self.messages.has_sender_name()
        })
        .await
        {
            probe_fail("group sender names");
            return;
        }
        let group_has_call = self.messages.header_call_visible();
        let bot = self
            .chatlist
            .ordered()
            .into_iter()
            .find_map(|(id, title)| (title == "Omarchy Bot").then_some(id));
        let Some(bot) = bot else {
            probe_fail("call button present: bot fixture");
            return;
        };
        self.clone().open_chat(bot);
        if !poll_until(3000, || {
            self.open_chat.get() == Some(bot) && !self.messages.is_loading()
        })
        .await
        {
            probe_fail("call button present: open bot");
            return;
        }
        let bot_has_call = self.messages.header_call_visible();
        self.clone().open_chat(marta);
        probe_step("open Marta");
        if !poll_until(3000, || {
            self.open_chat.get() == Some(marta)
                && !self.messages.is_loading()
                && !self.messages.is_empty()
        })
        .await
        {
            probe_fail("open Marta");
            return;
        }
        probe_step("background close and reopen preserves session");
        if !poll_until(3500, || !self.messages.history_is_refreshing()).await {
            probe_fail("history refresh before background"); return;
        }
        let Some(window) = self.window() else { probe_fail("background window"); return; };
        let Some(app) = window.application() else { probe_fail("background application"); return; };
        let epoch = self.session_epoch.get();
        let selected = self.open_chat.get();
        window.close();
        if window.is_visible() || self.presence_online.get() != Some(false) || !self.session_ready.get() {
            probe_fail("close hides window and marks offline"); return;
        }
        if self.tg.get_dialogs().await.is_err() || !app.windows().contains(window.upcast_ref::<gtk::Window>()) {
            probe_fail("backend and window survive hiding"); return;
        }
        app.activate();
        if !poll_until(1500, || window.is_visible() && window.is_mapped()).await
            || self.session_epoch.get() != epoch || self.open_chat.get() != selected
            || app.windows().iter().filter(|w| w.widget_name() == "omarchygram-main").count() != 1 {
            probe_fail("reopen reuses session and main window"); return;
        }
        probe_step("cached chat opens while fresh history is pending");
        self.clone().open_chat(bot);
        self.clone().open_chat(marta);
        if !poll_until(2500, || !self.messages.is_loading() && !self.messages.is_empty()).await {
            probe_fail("cached chat visible"); return;
        }
        if std::env::var("OMG_MOCK_LATENCY_MS").ok().and_then(|v| v.parse::<u64>().ok()).is_some_and(|ms| ms >= 400)
            && !self.messages.history_is_refreshing() {
            probe_fail("cache must render before delayed server history"); return;
        }
        if !poll_until(3500, || !self.messages.history_is_refreshing()).await {
            probe_fail("cached history refresh completes"); return;
        }
        probe_step("history refresh preserves live edits deletions and failure content");
        let history_view = MessagesView::new(self.effects.clone());
        history_view.reset_chat(marta, "History regression", 1);
        history_view.begin_history_load();
        let first = Msg { id: 1, chat_id: marta, text: "before edit".into(), ..Msg::default() };
        let second = Msg { id: 2, chat_id: marta, text: "deleted".into(), ..Msg::default() };
        let snapshot = vec![first.clone(), second];
        let mut edited = first; edited.text = "live edit".into();
        history_view.merge_event(edited.clone());
        history_view.remove(2);
        history_view.finish_cached(snapshot.clone());
        if history_view.message(1) != Some(edited.clone()) || history_view.contains(2) {
            probe_fail("cache cannot undo early live events"); return;
        }
        history_view.fail_initial("Offline");
        if history_view.message(1) != Some(edited.clone()) || history_view.is_loading() {
            probe_fail("failed refresh preserves cache"); return;
        }
        history_view.finish_refreshed(snapshot.clone(), &snapshot);
        if history_view.message(1) != Some(edited) || history_view.contains(2) || history_view.history_is_refreshing() {
            probe_fail("fresh history cannot undo live events"); return;
        }
        probe_step("status file written");
        let expected = self.chatlist.unread_totals();
        let writes_before = crate::status::writes();
        if !poll_until(1500, || {
            crate::status::probe_snapshot().is_some_and(|s| {
                s["version"] == 1
                    && s["running"] == true
                    && s["unread"] == expected.unread
                    && s["unread_chats"] == expected.unread_chats
                    && s["unread_with_muted"] == expected.unread_with_muted
                    && s["unread_chats_with_muted"] == expected.unread_chats_with_muted
                    && s["call"].is_null()
            })
        })
        .await
        {
            probe_fail("status file written");
            return;
        }
        // The fixture has unread chats, so the exact match above is not a
        // vacuous all-zeros comparison; and the writer must have written.
        if expected.unread_with_muted == 0 || writes_before == 0 {
            probe_fail("status file written (fixture has no unread or no write happened)");
            return;
        }
        probe_step("call button present");
        if group_has_call || bot_has_call || !self.messages.header_call_visible() {
            probe_fail("call button present");
            return;
        }

        self.messages.probe_click_header_call();
        probe_step("call outgoing ringing");
        if !poll_until(600, || self.call.phase() == Some(CallPhase::Requesting)).await {
            probe_fail("call outgoing ringing");
            return;
        }
        probe_step("call outgoing connects");
        if !poll_until(3500, || {
            self.call.phase() == Some(CallPhase::Active) && self.call.emoji_visible_nonempty()
        })
        .await
        {
            probe_fail("call outgoing connects");
            return;
        }
        probe_step("status file reports active call");
        let writes_before_call = crate::status::writes();
        if !poll_until(1500, || {
            crate::status::probe_snapshot().is_some_and(|s| {
                s["call"]["phase"] == "active"
                    && s["call"]["peer"] == "Marta"
                    && s["call"]["outgoing"] == true
                    && s["call"]["muted"] == false
                    && s["call"]["connected_at"].is_string()
            }) && crate::status::writes() > writes_before_call
        })
        .await
        {
            probe_fail("status file reports active call");
            return;
        }
        self.call.probe_toggle_mute();
        probe_step("call mute");
        if !poll_until(1000, || self.call.muted()).await {
            probe_fail("call mute");
            return;
        }
        self.call.probe_hang_up();
        probe_step("call hang up");
        if !poll_until(2500, || !self.call.is_open()).await {
            probe_fail("call hang up");
            return;
        }
        probe_step("status file clears call");
        if !poll_until(1500, || {
            crate::status::probe_snapshot().is_some_and(|s| s["call"].is_null())
        })
        .await
        {
            probe_fail("status file clears call");
            return;
        }

        let incoming = |id| CallInfo {
            id,
            peer_id: marta,
            peer_name: "Marta".to_string(),
            outgoing: false,
            phase: CallPhase::Incoming,
            muted: false,
            emojis: String::new(),
            connected_at: None,
            end_reason: None,
                    error: None,
        };
        self.handle_call_changed(incoming(-1));
        probe_step("call incoming rings");
        if !poll_until(500, || self.call.phase() == Some(CallPhase::Incoming)).await {
            probe_fail("call incoming rings");
            return;
        }
        self.call.probe_accept();
        probe_step("call incoming accept");
        if !poll_until(1000, || {
            self.call.phase() == Some(CallPhase::Active) && self.call.emoji_visible_nonempty()
        })
        .await
        {
            probe_fail("call incoming accept");
            return;
        }
        self.call.probe_hang_up();
        if !poll_until(2500, || !self.call.is_open()).await {
            probe_fail("call incoming accept cleanup");
            return;
        }
        self.handle_call_changed(incoming(-2));
        if !poll_until(500, || self.call.phase() == Some(CallPhase::Incoming)).await {
            probe_fail("call incoming decline setup");
            return;
        }
        self.call.probe_hang_up();
        probe_step("call incoming decline");
        if !poll_until(2500, || !self.call.is_open()).await {
            probe_fail("call incoming decline");
            return;
        }
        if environment_listed("OMG_MOCK_FAIL_ONCE", "DownloadMedia") {
            probe_step("media download error retry");
            if !poll_until(3500, || {
                matches!(self.messages.media_state(100), Some(MediaState::Failed))
                    || matches!(self.messages.media_state(103), Some(MediaState::Failed))
            })
            .await
            {
                probe_fail("media download transient error");
                return;
            }
            let failed_id = if matches!(self.messages.media_state(100), Some(MediaState::Failed)) {
                100
            } else {
                103
            };
            if !self
                .messages
                .error_text()
                .contains("mock: transient failure")
                || !self.messages.media_retryable(failed_id)
            {
                probe_fail("media download error shown");
                return;
            }
            self.clone()
                .media_action(failed_id, player::OpenIntent::Manual);
            if !poll_until(3500, || {
                matches!(
                    self.messages.media_state(failed_id),
                    Some(MediaState::Done(_))
                )
            })
            .await
            {
                probe_fail("media download retry");
                return;
            }
            if self.viewer.is_open() {
                self.close_viewer();
                self.close_profile();
            }
        }
        if self.messages.has_sender_name() {
            probe_fail("1:1 sender names");
            return;
        }
        probe_step("header more menu");
        self.messages.open_header_menu();
        if !poll_until(1000, || self.messages.header_menu_open()).await {
            probe_fail("header more menu");
            return;
        }
        self.messages.dismiss_header_menu();

        probe_step("per-chat drafts");
        self.messages.set_composer_text("Marta local draft");
        self.composer_draft_changed();
        let deni_for_draft = self
            .chatlist
            .ordered()
            .into_iter()
            .find_map(|(id, title)| (title == "Deni").then_some(id));
        let Some(deni_for_draft) = deni_for_draft else {
            probe_fail("find Deni for draft");
            return;
        };
        self.clone().open_chat(deni_for_draft);
        if !poll_until(3500, || {
            self.open_chat.get() == Some(deni_for_draft)
                && !self.messages.is_loading()
                && self.messages.composer_text() == "let me check"
                && self.messages.header_title() == "Deni"
                && self.chatlist.selected() == Some(deni_for_draft)
        })
        .await
        {
            probe_fail("server draft restore");
            return;
        }
        self.clone().open_chat(marta);
        if !poll_until(3500, || {
            self.open_chat.get() == Some(marta)
                && !self.messages.is_loading()
                && self.messages.composer_text() == "Marta local draft"
                && self.messages.header_title() == "Marta"
                && self.chatlist.selected() == Some(marta)
        })
        .await
        {
            probe_fail("local draft restore");
            return;
        }
        self.messages.set_composer_text("");
        self.composer_draft_changed();

        // Package 5C: in-chat search owns a generation independent of the
        // global sidebar search.  Exercise an abandoned slow request before
        // the ordinary hit/miss path.
        self.clone().open_chat(deni_for_draft);
        if !poll_until(3500, || {
            self.open_chat.get() == Some(deni_for_draft)
                && !self.messages.is_loading()
                && self.messages.contains(203)
        })
        .await
        {
            probe_fail("open Deni for in-chat search");
            return;
        }
        if environment_listed("OMG_MOCK_SLOW", "SearchMessages") {
            probe_step("in-chat search stale chat switch");
            self.open_in_chat_search();
            self.messages.set_search_text("lockfile");
            self.clone().open_chat(marta);
            glib::timeout_future(Duration::from_millis(1800)).await;
            if self.open_chat.get() != Some(marta)
                || self.messages.search_is_open()
                || self.messages.header_title() != "Marta"
            {
                probe_fail("in-chat search stale result");
                return;
            }
            self.clone().open_chat(deni_for_draft);
            if !poll_until(3500, || {
                self.open_chat.get() == Some(deni_for_draft)
                    && !self.messages.is_loading()
                    && self.messages.contains(203)
            })
            .await
            {
                probe_fail("restore Deni after slow search");
                return;
            }
        }

        self.clone().open_chat(marta);
        if !poll_until(3500, || {
            self.open_chat.get() == Some(marta)
                && !self.messages.is_loading()
                && self.messages.contains(100)
                && !self.messages.contains(91)
        })
        .await
        {
            probe_fail("open Marta for detached search");
            return;
        }
        probe_step("in-chat search detached hit");
        self.open_in_chat_search();
        self.messages.set_search_text("scroll-back");
        if !poll_until(3800, || {
            (self.messages.search_position_text() == "1 of 1" && self.messages.is_detached())
                || self.messages.search_retry_visible()
        })
        .await
        {
            probe_fail("in-chat search detached response");
            return;
        }
        if self.messages.search_retry_visible() {
            probe_step("in-chat search error retry");
            if self.messages.search_text() != "scroll-back"
                || !self
                    .messages
                    .search_position_text()
                    .contains("mock: transient failure")
            {
                probe_fail("in-chat search retained input and error");
                return;
            }
            self.messages.trigger_search_retry();
            if !poll_until(3800, || {
                self.messages.search_position_text() == "1 of 1" && self.messages.is_detached()
            })
            .await
            {
                probe_fail("in-chat search retry success");
                return;
            }
        }
        if !self.messages.contains(91)
            || !self.messages.row_has_css_class(91, "omg-hit")
            || !self.messages.row_has_css_class(91, "omg-hit-active")
        {
            probe_fail("in-chat search detached highlight");
            return;
        }
        self.messages.trigger_jump_to_latest();
        probe_step("in-chat search jump to latest");
        if !poll_until(3800, || {
            !self.messages.is_detached()
                && !self.messages.is_loading()
                && self.messages.contains(100)
                && !self.messages.contains(91)
        })
        .await
        {
            probe_fail("in-chat search jump to latest");
            return;
        }
        probe_step("in-chat search no results");
        self.messages.set_search_text("zzzz");
        if !poll_until(3800, || {
            self.messages.search_position_text() == "No results"
        })
        .await
        {
            probe_fail("in-chat search no results");
            return;
        }
        self.close_in_chat_search();
        if self.messages.search_is_open() {
            probe_fail("in-chat search close");
            return;
        }

        self.clone().open_chat(deni_for_draft);
        if !poll_until(3500, || {
            self.open_chat.get() == Some(deni_for_draft)
                && !self.messages.is_loading()
                && self.messages.contains(208)
        })
        .await
        {
            probe_fail("restore Deni for formatting");
            return;
        }

        probe_step("formatting markup");
        let formatted = self.messages.rendered_markup(206).unwrap_or_default();
        let preformatted = self.messages.rendered_markup(207).unwrap_or_default();
        let spoiler = self.messages.rendered_markup(208).unwrap_or_default();
        if !formatted.contains("<b>")
            || !formatted.contains("<tt>")
            || !formatted.contains("<span background=")
            || !formatted.contains("<a href=")
            || !preformatted.contains("<tt>")
            || !self.messages.message_has_pre_block(207)
            || !spoiler.contains("alpha=\"1%\"")
            || spoiler.contains("omg-spoiler:")
            || !self.messages.message_has_css_class(208, "omg-spoiler")
        {
            probe_fail("formatting markup fixtures");
            return;
        }
        probe_step("spoiler click reveal");
        if !self.messages.probe_click_spoiler(208)
            || !poll_until(1000, || {
                self.messages
                    .rendered_markup(208)
                    .is_some_and(|markup| !markup.contains("alpha=\"1%\""))
                    && !self.messages.message_has_css_class(208, "omg-spoiler")
            })
            .await
        {
            probe_fail("spoiler click reveal");
            return;
        }

        self.clone().open_chat(marta);
        if !poll_until(3500, || {
            self.open_chat.get() == Some(marta)
                && !self.messages.is_loading()
                && self.messages.contains(104)
        })
        .await
        {
            probe_fail("open Marta for reactions");
            return;
        }
        probe_step("reaction context quick row");
        if !poll_until(3500, || self.messages.available_reactions_settled()).await {
            probe_fail("available reactions settled");
            return;
        }
        if !self.messages.probe_choose_quick_reaction(104, "👍")
            && (!self.messages.probe_retry_available_reactions(104)
                || !poll_until(3500, || self.messages.available_reactions_settled()).await
                || !self.messages.probe_choose_quick_reaction(104, "👍"))
            {
                probe_fail("reaction quick row");
                return;
            }
        if environment_listed("OMG_MOCK_FAIL_ONCE", "SendReaction") {
            if !poll_until(3500, || self.messages.reaction_retry_visible()).await {
                probe_fail("reaction transient error retry");
                return;
            }
            let reverted = self.messages.reaction(104, "👍");
            if !self
                .messages
                .error_text()
                .contains("mock: transient failure")
                || !reverted.is_some_and(|reaction| !reaction.chosen && reaction.count == 1)
            {
                probe_fail("reaction rollback scope");
                return;
            }
            probe_step("reaction retry");
            self.messages.trigger_reaction_retry();
        }
        if !poll_until(3500, || {
            self.messages
                .reaction(104, "👍")
                .is_some_and(|reaction| reaction.chosen && reaction.count == 2)
                && !self.messages.reaction_retry_visible()
        })
        .await
        {
            probe_fail("reaction add result");
            return;
        }
        probe_step("reaction remove");
        if !self.messages.probe_choose_quick_reaction(104, "👍") {
            probe_fail("reaction remove quick row");
            return;
        }
        if !poll_until(3500, || {
            self.messages
                .reaction(104, "👍")
                .is_some_and(|reaction| !reaction.chosen && reaction.count == 1)
        })
        .await
        {
            probe_fail("reaction remove result");
            return;
        }
        probe_step("reaction ellipsis chooser");
        if !self.messages.probe_open_reaction_chooser(104)
            || !self.messages.probe_pick_reaction_emoji("❤️")
            || !poll_until(3500, || {
                self.messages
                    .reaction(104, "❤️")
                    .is_some_and(|reaction| reaction.chosen && reaction.count == 1)
            })
            .await
        {
            probe_fail("reaction ellipsis chooser");
            return;
        }

        // Resetting the history starts fresh image fills, making the slow
        // viewer/chat-switch race deterministic even when earlier thumbnails
        // have already completed.
        if environment_listed("OMG_MOCK_SLOW", "DownloadMedia") {
            probe_step("viewer download stale chat switch");
            self.clone().force_reload(marta);
            if !poll_until(3500, || {
                !self.messages.is_loading() && self.messages.contains(103)
            })
            .await
            {
                probe_fail("viewer slow history reload");
                return;
            }
            self.open_viewer(103);
            if !self.viewer.is_open() {
                probe_fail("viewer slow open");
                return;
            }
            self.clone().open_chat(deni_for_draft);
            glib::timeout_future(Duration::from_millis(1900)).await;
            if self.open_chat.get() != Some(deni_for_draft)
                || self.messages.header_title() != "Deni"
                || self.viewer.is_open()
            {
                probe_fail("viewer stale completion");
                return;
            }
            self.clone().open_chat(marta);
            if !poll_until(3500, || {
                self.open_chat.get() == Some(marta)
                    && !self.messages.is_loading()
                    && self.messages.contains(103)
            })
            .await
            {
                probe_fail("restore Marta after slow viewer");
                return;
            }
        }
        probe_step("viewer previous next close");
        if !poll_until(4200, || {
            matches!(self.messages.media_state(100), Some(MediaState::Done(_)))
                && matches!(self.messages.media_state(103), Some(MediaState::Done(_)))
        })
        .await
        {
            probe_fail("viewer photos downloaded");
            return;
        }
        probe_step("photo-only message has visible image geometry");
        if !poll_until(1500, || self.messages.image_is_allocated(103)).await {
            probe_fail("loaded photo collapsed to its timestamp");
            return;
        }
        self.messages.scroll_to_bottom();
        if !poll_until(1500, || self.messages.last_message().is_some_and(|message| self.messages.row_visible(message.id))).await {
            probe_fail("newest message is outside the scroll range after images load");
            return;
        }
        self.open_viewer(103);
        if !self.viewer.is_open() || self.viewer.current_id() != Some(103) {
            probe_fail("viewer open");
            return;
        }
        let _ = self.viewer.probe_key(gdk::Key::Left);
        if self.viewer.current_id() != Some(100) {
            probe_fail("viewer Left key");
            return;
        }
        let _ = self.viewer.probe_key(gdk::Key::Right);
        if self.viewer.current_id() != Some(103) {
            probe_fail("viewer Right key");
            return;
        }
        let _ = self.viewer.probe_key(gdk::Key::Escape);
        if self.viewer.is_open() || !poll_until(1000, || self.messages.message_has_focus(103)).await
        {
            probe_fail("viewer Escape focus return");
            return;
        }

        let news = self
            .chatlist
            .ordered()
            .into_iter()
            .find_map(|(id, title)| (title == "Omarchy News").then_some(id));
        let Some(news) = news else {
            probe_fail("find Omarchy News");
            return;
        };
        self.clone().open_chat(news);
        probe_step("pinned message jump unpin");
        if environment_listed("OMG_MOCK_FAIL_ONCE", "GetPinnedMessage") {
            if !poll_until(3500, || self.messages.pinned_retry_visible()).await {
                probe_fail("pinned message transient error");
                return;
            }
            self.messages.trigger_pinned_retry();
        }
        if !poll_until(3500, || {
            !self.messages.is_loading()
                && self.messages.pinned_id() == Some(501)
                && self.messages.pinned_bar_visible()
                && self.messages.web_preview_visible(503)
        })
        .await
        {
            probe_fail("pinned message load");
            return;
        }
        if self.messages.has_sender_name() {
            probe_fail("channel sender names");
            return;
        }
        probe_step("long pinned message caps four lines and expands");
        let original = self.messages.message(501).expect("pinned fixture");
        let mut long_pin = original.clone();
        long_pin.text = (1..=40).map(|n| format!("Pinned line {n}: long announcements remain accessible after expanding.")).collect::<Vec<_>>().join("\n");
        self.messages.set_pinned_message(Some(long_pin));
        if !poll_until(1500, || {
            let (expand, expanded, text_height, _) = self.messages.probe_pinned_metrics();
            expand && !expanded && text_height > 0 && text_height <= 90
        }).await { probe_fail("pinned message collapsed height"); return; }
        self.messages.trigger_pinned_expand();
        if !poll_until(1500, || {
            let (expand, expanded, text_height, bar_height) = self.messages.probe_pinned_metrics();
            expand && expanded && text_height > 240 && bar_height <= 280
        }).await { probe_fail("pinned message expanded scroll bound"); return; }
        self.messages.trigger_pinned_expand();
        if !poll_until(1500, || self.messages.probe_pinned_metrics().2 <= 90).await {
            probe_fail("pinned message collapse restores height"); return;
        }
        self.messages.set_pinned_message(Some(original));
        self.messages.trigger_pinned();
        if !poll_until(1000, || self.messages.message_has_focus(501)).await {
            probe_fail("pinned message jump");
            return;
        }
        self.clone().unpin_message(501);
        if !poll_until(3500, || self.messages.pinned_id().is_none()).await {
            probe_fail("pinned message unpin");
            return;
        }

        self.clone().open_chat(deni_for_draft);
        if !poll_until(3500, || {
            self.open_chat.get() == Some(deni_for_draft)
                && !self.messages.is_loading()
                && self.messages.contains(201)
        })
        .await
        {
            probe_fail("open Deni for forward");
            return;
        }
        if environment_listed("OMG_MOCK_SLOW", "ForwardMessages") {
            probe_step("forward stale chat switch");
            self.open_forward(vec![201]);
            if !self.forward.probe_select("Marta") {
                probe_fail("forward slow select target");
                return;
            }
            self.forward.probe_submit();
            self.clone().open_chat(news);
            glib::timeout_future(Duration::from_millis(1800)).await;
            if self.open_chat.get() != Some(news)
                || self.forward.is_open()
                || self.messages.header_title() != "Omarchy News"
            {
                probe_fail("forward stale completion");
                return;
            }
            self.clone().open_chat(deni_for_draft);
            if !poll_until(3500, || {
                self.open_chat.get() == Some(deni_for_draft)
                    && !self.messages.is_loading()
                    && self.messages.contains(201)
            })
            .await
            {
                probe_fail("restore Deni after slow forward");
                return;
            }
        }
        probe_step("forward Deni to Marta");
        self.open_forward(vec![201]);
        if !self.forward.is_open() || !self.forward.probe_select("Marta") {
            probe_fail("forward dialog target");
            return;
        }
        self.forward.probe_submit();
        if environment_listed("OMG_MOCK_FAIL_ONCE", "ForwardMessages") {
            if !poll_until(3800, || {
                self.forward.is_open()
                    && self
                        .forward
                        .probe_status()
                        .contains("mock: transient failure")
            })
            .await
            {
                probe_fail("forward transient error");
                return;
            }
            probe_step("forward retry");
            self.forward.probe_submit();
        }
        if !poll_until(4200, || {
            self.open_chat.get() == Some(marta)
                && !self.messages.is_loading()
                && self.messages.messages().iter().any(|message| {
                    message.text == "the build is green again"
                        && message.forwarded_from.as_deref() == Some("Deni")
                        && self
                            .messages
                            .forwarded_header_text(message.id)
                            .is_some_and(|header| header.contains("Forwarded from Deni"))
                })
        })
        .await
        {
            probe_fail("forward success and header");
            return;
        }

        probe_step("video and gif cards");
        self.clone().open_chat(group);
        if !poll_until(3500, || {
            !self.messages.is_loading()
                && self.messages.has_media_card(MediaKind::Video)
                && self.messages.has_media_card(MediaKind::Gif)
        })
        .await
        {
            probe_fail("video and gif cards");
            return;
        }
        let old_project = self
            .chatlist
            .ordered()
            .into_iter()
            .find_map(|(id, title)| (title == "Old project").then_some(id));
        let Some(old_project) = old_project else {
            probe_fail("find Old project");
            return;
        };
        probe_step("media card stale download");
        let launches_before_switch = self.probe_media_launches.get();
        if !self.messages.trigger_media(404) {
            probe_fail("video card Download");
            return;
        }
        self.clone().open_chat(old_project);
        glib::timeout_future(Duration::from_millis(2600)).await;
        if self.open_chat.get() != Some(old_project)
            || self.probe_media_launches.get() != launches_before_switch
        {
            probe_fail("media stale completion launched");
            return;
        }
        probe_step("audio video-note unsupported cards");
        if !poll_until(3500, || {
            !self.messages.is_loading()
                && self.messages.has_media_card(MediaKind::Audio)
                && self.messages.has_media_card(MediaKind::VideoNote)
                && self.messages.has_media_card(MediaKind::Unsupported)
        })
        .await
        {
            probe_fail("archived media cards");
            return;
        }
        // 6A: voice/audio/video cards are inline players — nothing about them
        // ever reaches an external application any more.
        probe_step("media card Download done");
        let launches_before_audio = self.probe_media_launches.get();
        if !self.messages.trigger_media(702)
            || !poll_until(6000, || {
                matches!(
                    self.messages.player_state(702),
                    player::PlayerState::Playing | player::PlayerState::Error
                )
            })
            .await
        {
            probe_fail("audio card did not reach playing/error inline");
            return;
        }
        if self.probe_media_launches.get() != launches_before_audio {
            probe_fail("audio card launched externally");
            return;
        }
        // The mock renders a real video note with ffmpeg; where the system has
        // no mp4 decoder (or no ffmpeg) the card must show the inline §1.7
        // error instead — either way it settles and launches nothing.
        probe_step("media card video-note plays or is unavailable");
        let launches_before_unavailable = self.probe_media_launches.get();
        if !self.messages.trigger_media(703)
            || !poll_until(6000, || {
                matches!(
                    self.messages.player_state(703),
                    player::PlayerState::Playing | player::PlayerState::Error
                )
            })
            .await
        {
            probe_fail("video-note card never settled to playing/error");
            return;
        }
        if self.probe_media_launches.get() != launches_before_unavailable {
            probe_fail("video-note card launched externally");
            return;
        }
        if !self.run_wave6d_probe().await {
            return;
        }

        // ---- Wave 6A: in-app playback (specs/spec-wave6.md §1.11) ----
        let media_lab = self
            .chatlist
            .ordered()
            .into_iter()
            .find_map(|(id, title)| (title == "Media Lab").then_some(id));
        let Some(media_lab) = media_lab else {
            probe_fail("find Media Lab");
            return;
        };
        self.clone().open_chat(media_lab);
        if !poll_until(5000, || {
            self.open_chat.get() == Some(media_lab)
                && !self.messages.is_loading()
                && self.messages.contains(800)
                && self.messages.player_exists(800)
                && self.messages.player_exists(801)
        })
        .await
        {
            probe_fail("open Media Lab with inline players");
            return;
        }
        let launches_before_players = self.probe_media_launches.get();

        // Finish a policy-enabled loop while it is off-screen. It must stay a
        // poster now, remember that it may resume, and start only after its
        // row later enters the viewport.
        probe_step("player offscreen autoplay poster");
        self.messages.scroll_to_bottom();
        if !poll_until(2_000, || !self.messages.row_visible(804)).await {
            probe_fail("GIF row was not off-screen for deferred autoplay");
            return;
        }
        self.clone()
            .play_when_ready(804, player::OpenIntent::AutoplayMuted);
        self.clone().start_media_download(804, false);
        if !poll_until(8_000, || {
            matches!(
                self.messages.player_state(804),
                player::PlayerState::Paused | player::PlayerState::Error
            )
        })
        .await
        {
            probe_fail("off-screen GIF did not settle as a poster/error");
            return;
        }
        if self.messages.player_state(804) == player::PlayerState::Paused
            && !self.messages.player_resumes_when_visible(804)
        {
            probe_fail("off-screen GIF poster forgot deferred autoplay");
            return;
        }

        probe_step("player voice play");
        let _ = self.scroll_into_view(800).await;
        // A transient download error on an inline player must leave a usable
        // retry action, not a disabled/hidden play button.
        if !self
            .messages
            .fail_media(800, self.messages.media_generation(800).unwrap_or(0), true)
            || !self.messages.media_retryable(800)
            || !self.messages.player_retry_available(800)
            || !self.messages.trigger_media(800)
            || !poll_until(5000, || {
                matches!(
                    self.messages.player_state(800),
                    player::PlayerState::Playing | player::PlayerState::Error
                )
            })
            .await
        {
            probe_fail("voice did not reach playing/error");
            return;
        }
        // Where the system can decode ogg/opus the voice really plays; where it
        // cannot, the card carries the inline error and the rest of the audio
        // assertions have nothing to measure.
        let voice_plays = self.messages.player_state(800) == player::PlayerState::Playing;

        probe_step("player voice seek");
        if voice_plays {
            // Pause first so the position only moves because of the seek, then
            // seek both ways on the 3 s fixture (§1.10).
            self.messages.toggle_media(800);
            glib::timeout_future(Duration::from_millis(300)).await;
            if self.messages.player_tick_active(800) {
                probe_fail("voice progress tick survived pause");
                return;
            }
            self.messages.player_seek(800, 0.0);
            if !poll_until(2000, || self.messages.player_position(800) <= 0.4).await {
                probe_fail(&format!(
                    "voice seek to the start left the position at {:.2}s",
                    self.messages.player_position(800)
                ));
                return;
            }
            self.messages.player_seek(800, 0.9);
            if !poll_until(2000, || self.messages.player_position(800) >= 1.5).await {
                probe_fail(&format!(
                    "voice seek forward left the position at {:.2}s",
                    self.messages.player_position(800)
                ));
                return;
            }
            self.messages.player_seek(800, 0.0);
            self.messages.toggle_media(800);
            if !poll_until(3000, || {
                self.messages.player_state(800) == player::PlayerState::Playing
                    && self.messages.player_tick_active(800)
            })
            .await
            {
                probe_fail("voice did not resume after seeking");
                return;
            }
        } else if self.messages.player_state(800) != player::PlayerState::Error {
            probe_fail("voice neither plays nor shows an error");
            return;
        }

        probe_step("player speed");
        if self.messages.player_speed(800) != 1.0 {
            probe_fail("voice speed did not start at 1x");
            return;
        }
        let first = self.messages.player_cycle_speed(800);
        let second = self.messages.player_cycle_speed(800);
        if first != 1.5 || second != 2.0 || self.messages.player_speed(800) != 2.0 {
            probe_fail(&format!("speed cycle wrong: 1 -> {first} -> {second}"));
            return;
        }
        if (self.settings.get().media.voice_speed - 2.0).abs() > 0.01 {
            probe_fail("speed not persisted to settings.media.voice_speed");
            return;
        }
        if self.messages.player_cycle_speed(800) != 1.0
            || (self.settings.get().media.voice_speed - 1.0).abs() > 0.01
        {
            probe_fail("speed did not cycle back to 1x");
            return;
        }

        probe_step("player music");
        let _ = self.scroll_into_view(801).await;
        if !self.messages.trigger_media(801)
            || !poll_until(10_000, || {
                matches!(
                    self.messages.player_state(801),
                    player::PlayerState::Playing | player::PlayerState::Error
                )
            })
            .await
        {
            probe_fail(&format!(
                "music did not reach playing/error (state {:?}, media {:?}, exists {}, visible {}, active sound {})",
                self.messages.player_state(801),
                self.messages.media_state(801),
                self.messages.player_exists(801),
                self.messages.row_visible(801),
                self.messages.active_player_count()
            ));
            return;
        }
        let music_plays = self.messages.player_state(801) == player::PlayerState::Playing;

        probe_step("player single active");
        if voice_plays
            && music_plays
            && !poll_until(3000, || {
                self.messages.player_state(800) == player::PlayerState::Paused
            })
            .await
        {
            probe_fail("voice not paused when the music started");
            return;
        }
        if self.messages.active_player_count() > 1 {
            probe_fail(&format!(
                "expected at most one sound player, got {}",
                self.messages.active_player_count()
            ));
            return;
        }

        probe_step("player pending manual intent");
        let _ = self.scroll_into_view(802).await;
        // Model the exact race: autoplay registered first, then the user's
        // click while the download is pending. The late manual continuation
        // must operate on the player opened by the first continuation.
        self.clone()
            .play_when_ready(802, player::OpenIntent::AutoplayMuted);
        self.clone()
            .media_action(802, player::OpenIntent::Manual);
        if !poll_until(8000, || {
                matches!(
                    self.messages.player_state(802),
                    player::PlayerState::Playing | player::PlayerState::Error
                )
            })
            .await
        {
            probe_fail("pending manual video intent did not reach playing/error");
            return;
        }
        if self.messages.player_state(802) == player::PlayerState::Playing
            && !self.messages.player_manual_sound_requested(802)
        {
            probe_fail("pending manual video intent did not request sound");
            return;
        }

        probe_step("player video");
        if !matches!(
            self.messages.player_state(802),
            player::PlayerState::Playing | player::PlayerState::Error
        ) {
            probe_fail("video did not remain playing/error after pending manual intent");
            return;
        }

        probe_step("player video fullscreen");
        let video_plays = self.messages.player_state(802) == player::PlayerState::Playing;
        if video_plays {
            // Drive the gesture's logical callback directly (no synthetic
            // desktop input): the delayed single press must be cancelled by
            // the second press, leaving playback running in fullscreen.
            self.messages.probe_player_picture_press(802, 1);
            self.messages.probe_player_picture_press(802, 2);
            glib::timeout_future(Duration::from_millis(350)).await;
            // Without mp4 decoders on the machine the pipeline errors a
            // moment after play(): that closes fullscreen by design and is an
            // accepted terminal state (§1.7), not a paused player.
            let state = self.messages.player_state(802);
            if state != player::PlayerState::Error
                && (!self.messages.player_fullscreen_open() || state != player::PlayerState::Playing)
            {
                probe_fail("video double click paused playback or missed fullscreen");
                return;
            }
            self.messages.player_close_fullscreen();
            glib::timeout_future(Duration::from_millis(300)).await;
            if self.messages.player_fullscreen_open() {
                probe_fail("video fullscreen did not close");
                return;
            }
        } else {
            self.messages.player_fullscreen(802);
            glib::timeout_future(Duration::from_millis(300)).await;
            if self.messages.player_fullscreen_open() {
                // A card showing the inline error has nothing to show fullscreen.
                probe_fail("fullscreen opened for a video that cannot play");
                return;
            }
        }

        // No click on the circle: it autoplays muted while its row is visible.
        probe_step("player video note");
        let gtk_settings = gtk::Settings::default();
        if let Some(settings) = gtk_settings.as_ref() {
            settings.set_gtk_enable_animations(false);
        }
        if !self.scroll_into_view(803).await {
            probe_fail("video note row never became visible");
            return;
        }
        if !poll_until(8000, || {
            matches!(
                self.messages.player_state(803),
                player::PlayerState::Playing | player::PlayerState::Error
            )
        })
        .await
        {
            probe_fail("video note did not autoplay (or fail) while visible");
            return;
        }
        if self.messages.player_state(803) == player::PlayerState::Playing {
            // Video-note autoplay is independent of the animations master. A
            // user click then becomes an explicit with-sound restart (the
            // probe sink itself remains muted).
            if !self.messages.trigger_media(803)
                || !self.messages.player_manual_sound_requested(803)
                || self.messages.player_state(803) != player::PlayerState::Playing
            {
                if let Some(settings) = gtk_settings.as_ref() {
                    settings.set_gtk_enable_animations(true);
                }
                probe_fail("manual video-note click did not request sound");
                return;
            }
        }
        if let Some(settings) = gtk_settings.as_ref() {
            settings.set_gtk_enable_animations(true);
        }

        // Same for the GIF: settings.media.autoplay_gifs plus the animations
        // master start it, no click involved.
        probe_step("player gif autoplay");
        if !self.scroll_into_view(804).await {
            probe_fail("gif row never became visible");
            return;
        }
        if !poll_until(8000, || {
            self.messages.player_exists(804)
                && matches!(
                    self.messages.player_state(804),
                    player::PlayerState::Playing | player::PlayerState::Error
                )
        })
        .await
        {
            probe_fail("gif did not autoplay (or fail) while visible");
            return;
        }
        if self.messages.player_state(804) == player::PlayerState::Playing {
            self.settings
                .update(|settings| settings.media.autoplay_gifs = false);
            if !poll_until(2000, || {
                self.messages.player_state(804) == player::PlayerState::Paused
                    && !self.messages.player_tick_active(804)
            })
            .await
            {
                probe_fail("autoplay_gifs off did not pause the live GIF");
                return;
            }
            self.settings
                .update(|settings| settings.media.autoplay_gifs = true);
            if !poll_until(2000, || {
                self.messages.player_state(804) == player::PlayerState::Playing
            })
            .await
            {
                probe_fail("autoplay_gifs on did not resume the visible GIF");
                return;
            }
            if let Some(settings) = gtk::Settings::default() {
                settings.set_gtk_enable_animations(false);
                if !poll_until(2000, || {
                    self.messages.player_state(804) == player::PlayerState::Paused
                })
                .await
                {
                    settings.set_gtk_enable_animations(true);
                    probe_fail("animations master off did not pause the live GIF");
                    return;
                }
                settings.set_gtk_enable_animations(true);
                if !poll_until(2000, || {
                    self.messages.player_state(804) == player::PlayerState::Playing
                })
                .await
                {
                    probe_fail("animations master on did not resume the visible GIF");
                    return;
                }
            }
        }
        if self.probe_media_launches.get() != launches_before_players {
            probe_fail("an inline player launched an external application");
            return;
        }

        probe_step("player scroll pause");
        if !self.scroll_into_view(801).await {
            probe_fail("music row never became visible");
            return;
        }
        if music_plays {
            // The 3 s clip may still be playing or may have rewound to Paused
            // by now (latency variant): only click when it is not playing,
            // a click on a playing player would pause it.
            let needs_click = self.messages.player_state(801) != player::PlayerState::Playing;
            if (needs_click && !self.messages.trigger_media(801))
                || !poll_until(4000, || {
                    self.messages.player_state(801) == player::PlayerState::Playing
                })
                .await
            {
                probe_fail("music did not restart before the scroll test");
                return;
            }
            self.messages.scroll_to_bottom();
            if !poll_until(3000, || {
                !self.messages.row_visible(801)
                    && self.messages.player_state(801) != player::PlayerState::Playing
            })
            .await
            {
                probe_fail("music kept playing after its row scrolled away");
                return;
            }
        }

        probe_step("player stops on chat switch");
        self.clone().open_chat(marta);
        // Marta may legitimately hold a muted autoplay circle (the 6F probe
        // sent one there); what must be gone are Media Lab's players and any
        // sound.
        if !poll_until(5000, || {
            self.open_chat.get() == Some(marta)
                && !self.messages.is_loading()
                && self.messages.contains(104)
                && !self.messages.player_exists(800)
                && !self.messages.player_exists(801)
                && !self.messages.player_exists(802)
                && self.messages.active_player_count() == 0
                && !self.messages.player_fullscreen_open()
        })
        .await
        {
            probe_fail("players not stopped / Marta not restored");
            return;
        }

        probe_step("player retained stream reuse");
        self.clone().open_chat(media_lab);
        if !poll_until(5000, || {
            self.open_chat.get() == Some(media_lab)
                && !self.messages.is_loading()
                && self.messages.player_exists(802)
        })
        .await
            || !self.scroll_into_view(802).await
            || !self.messages.trigger_media(802)
            || !poll_until(8000, || {
                matches!(
                    self.messages.player_state(802),
                    player::PlayerState::Playing | player::PlayerState::Error
                )
            })
            .await
            || !self.messages.player_reused_retained_stream(802)
        {
            probe_fail("rebuilt video row did not reuse its retained stream");
            return;
        }

        probe_step("player row removal");
        if self.messages.remove(802).is_none() || self.messages.player_exists(802) {
            probe_fail("removed row left a player handle behind");
            return;
        }

        probe_step("player history reset");
        let history_epoch = self.bump_epoch();
        self.messages.reset_history(media_lab, history_epoch);
        if !self.messages.player_registry_empty() {
            probe_fail("same-chat history reset left player handles behind");
            return;
        }
        self.clone().open_chat(marta);
        if !poll_until(5000, || {
            self.open_chat.get() == Some(marta)
                && !self.messages.is_loading()
                && self.messages.contains(104)
        })
        .await
        {
            probe_fail("Marta not restored after player history-reset probe");
            return;
        }

        let mom_for_wave5d = self
            .chatlist
            .ordered()
            .into_iter()
            .find_map(|(id, title)| (title == "Mom").then_some(id));
        let Some(mom_for_wave5d) = mom_for_wave5d else {
            probe_fail("find Mom for regular-use probe");
            return;
        };
        if !self
            .run_wave5d_probe(group, marta, deni_for_draft, mom_for_wave5d)
            .await
        {
            return;
        }

        let Some(window) = self.window() else {
            probe_fail("sidebar resize window");
            return;
        };
        let auto_resize_state = {
            let state = self.ui_state.borrow();
            (
                state.sidebar_width,
                state.sidebar_collapsed,
                state.folder_id,
            )
        };
        probe_step("sidebar auto-collapse 600");
        let resize = self.resize_window_for_probe(&window, 600).await;
        if !poll_until(1000, || {
            self.effective_sidebar_collapsed.get()
                && self.paned.position() == 64
                && sidebar_position_fits(600, self.paned.position())
                && !self.chatlist.folder_tabs_visible()
                && {
                    let state = self.ui_state.borrow();
                    (
                        state.sidebar_width,
                        state.sidebar_collapsed,
                        state.folder_id,
                    ) == auto_resize_state
                }
        })
        .await
        {
            probe_fail("sidebar auto-collapse 600");
            return;
        }
        if !poll_until(1500, || {
            let (bubble, _) = self.messages.bubble_metrics();
            bubble > 0 && resize.pane_width > 0 && bubble <= probe_bubble_limit(resize.pane_width)
        })
        .await
        {
            probe_fail("bubble width clamp");
            return;
        }
        self.resize_window_for_probe(&window, 1100).await;
        if !poll_until(1000, || {
            !self.effective_sidebar_collapsed.get()
                && self.paned.position() == clamp_sidebar_width(auto_resize_state.0, 1100)
                && sidebar_position_fits(1100, self.paned.position())
                && self.chatlist.folder_tabs_visible()
                && {
                    let state = self.ui_state.borrow();
                    (
                        state.sidebar_width,
                        state.sidebar_collapsed,
                        state.folder_id,
                    ) == auto_resize_state
                }
        })
        .await
        {
            probe_fail("sidebar restore 1100");
            return;
        }
        probe_step("sidebar boundary 799 to 800");
        self.resize_window_for_probe(&window, 799).await;
        if !poll_until(1000, || {
            self.effective_sidebar_collapsed.get()
                && self.paned.position() == 64
                && sidebar_position_fits(799, self.paned.position())
                && !self.chatlist.folder_tabs_visible()
        })
        .await
        {
            probe_fail("sidebar boundary 799");
            return;
        }
        self.resize_window_for_probe(&window, 800).await;
        if !poll_until(1000, || {
            !self.effective_sidebar_collapsed.get()
                && self.paned.position() == clamp_sidebar_width(auto_resize_state.0, 800)
                && sidebar_position_fits(800, self.paned.position())
                && self.chatlist.folder_tabs_visible()
                && {
                    let state = self.ui_state.borrow();
                    (
                        state.sidebar_width,
                        state.sidebar_collapsed,
                        state.folder_id,
                    ) == auto_resize_state
                }
        })
        .await
        {
            probe_fail("sidebar boundary 800");
            return;
        }
        probe_step("explicit collapse survives narrow resize");
        self.toggle_sidebar();
        self.resize_window_for_probe(&window, 600).await;
        self.resize_window_for_probe(&window, 1100).await;
        if !self.effective_sidebar_collapsed.get()
            || !self.ui_state.borrow().sidebar_collapsed
            || self.ui_state.borrow().sidebar_width != auto_resize_state.0
            || self.ui_state.borrow().folder_id != auto_resize_state.2
            || self.paned.position() != 64
            || !sidebar_position_fits(1100, self.paned.position())
            || self.chatlist.folder_tabs_visible()
        {
            probe_fail("explicit collapse survives narrow resize");
            return;
        }
        self.toggle_sidebar();
        self.resize_window_for_probe(&window, 1100).await;
        if !poll_until(1000, || {
            !self.effective_sidebar_collapsed.get()
                && !self.ui_state.borrow().sidebar_collapsed
                && self.ui_state.borrow().sidebar_width == auto_resize_state.0
                && self.ui_state.borrow().folder_id == auto_resize_state.2
                && self.paned.position() == clamp_sidebar_width(auto_resize_state.0, 1100)
                && sidebar_position_fits(1100, self.paned.position())
                && self.chatlist.folder_tabs_visible()
        })
        .await
        {
            probe_fail("restore expanded window allocation");
            return;
        }
        probe_step("pagination ready");
        if !poll_until(3000, || self.messages.pagination_ready()).await {
            probe_fail("pagination ready");
            return;
        }
        self.messages.trigger_pagination();
        probe_step("pagination merge");
        if !poll_until(3000, || self.messages.contains(90)).await {
            probe_fail("pagination merge");
            return;
        }
        if self.messages.receipt_text(91).as_deref() != Some(icons::CHECK_DOUBLE) {
            probe_fail("pagination preserves read-outbox ticks");
            return;
        }

        self.messages.set_composer_text("probe message");
        self.clone().submit_composer();
        probe_step("send message");
        if !poll_until(3000, || {
            self.messages.find_outgoing_text("probe message").is_some()
        })
        .await
        {
            probe_fail("send message");
            return;
        }
        let Some(sent_id) = self.messages.find_outgoing_text("probe message") else {
            probe_fail("find sent message");
            return;
        };
        let typing_generation = self.messages.typing_generation();
        if self.messages.receipt_text(sent_id).as_deref() != Some(icons::CHECK) {
            probe_fail("sent message initial receipt");
            return;
        }
        probe_step("read outbox receipt");
        if !poll_until(3500, || {
            self.messages.receipt_text(sent_id).as_deref() == Some(icons::CHECK_DOUBLE)
        })
        .await
        {
            probe_fail("read outbox receipt");
            return;
        }
        probe_step("typing event");
        if !poll_until(3000, || {
            self.messages.typing_generation() > typing_generation
        })
        .await
        {
            probe_fail("typing event");
            return;
        }
        probe_step("mock reply");
        if !poll_until(3000, || self.messages.contains_text("(mock reply) got it")).await {
            probe_fail("mock reply");
            return;
        }

        self.messages.begin_edit(sent_id);
        self.messages.set_composer_text("probe edited");
        self.clone().submit_composer();
        probe_step("edit message");
        if !poll_until(3000, || {
            self.messages
                .message(sent_id)
                .is_some_and(|message| message.text == "probe edited" && message.edited)
        })
        .await
        {
            probe_fail("edit message");
            return;
        }

        self.clone().delete_message(sent_id);
        probe_step("delete message");
        if !poll_until(3000, || !self.messages.contains(sent_id)).await {
            probe_fail("delete message");
            return;
        }

        // Settings panel (wave 1): open via the Ctrl+, path, toggle
        // show_seconds on/off, assert the time labels re-render.
        self.toggle_settings();
        probe_step("open settings");
        if !poll_until(1000, || self.settings_open()).await {
            probe_fail("open settings");
            return;
        }

        for page in super::settings_view::PAGE_NAMES {
            probe_step(&format!("settings page {page}"));
            self.settings_view.probe_show_page(page);
            if self.settings_view.visible_page().as_deref() != Some(*page) {
                probe_fail("settings page order");
                return;
            }
        }
        probe_step("settings search and empty results");
        self.settings_view.probe_search("Send on Enter");
        if self.settings_view.visible_page().as_deref() != Some("messaging") { probe_fail("settings search messaging"); return; }
        self.settings_view.probe_search("zzzz_no_such_setting");
        if self.settings_view.visible_page().as_deref() != Some("search-empty") { probe_fail("settings search empty"); return; }
        self.settings_view.probe_search("");
        self.settings_view.probe_show_page("account");
        probe_step("account get me");
        if !poll_until(3000, || {
            self.settings_view.probe_account_name() == "Leo Test"
        })
        .await
        {
            probe_fail("account get me");
            return;
        }

        self.settings_view.probe_show_page("keyboard");
        probe_step("keyboard Primary canonicalizes to Control");
        let control_f = keys::canonical("<Control>f");
        if control_f.is_none() || control_f != keys::canonical("<Primary>f") {
            probe_fail("keyboard Primary canonicalization");
            return;
        }
        probe_step("keyboard capture");
        if !self.settings_view.keys().probe_capture(
            "switcher",
            gdk::Key::z,
            gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::SHIFT_MASK,
        ) || keys::canonical(&self.settings_view.keys().probe_accel("switcher"))
            != keys::canonical("<Control><Shift>z")
            || !self.settings_view.keys().probe_save()
        {
            probe_fail("keyboard capture");
            return;
        }
        probe_step("keyboard capture rejects modifier-only and invalid");
        if !self.settings_view.keys().probe_rejected_capture(
            "switcher",
            gdk::Key::Shift_L,
            gdk::ModifierType::SHIFT_MASK,
        ) || !self.settings_view.keys().probe_rejected_capture(
            "switcher",
            gdk::Key::VoidSymbol,
            gdk::ModifierType::empty(),
        ) {
            probe_fail("keyboard invalid capture rejection");
            return;
        }

        self.messages.set_composer_text("aéz");
        self.messages.probe_select_composer(1, 2);
        keys::wrap_buffer_selection(&self.messages.composer().buffer(), "**", "**");
        probe_step("keyboard live marker wrap");
        if self.messages.composer_text() != "a**é**z" || self.messages.probe_composer_cursor() != 6
        {
            probe_fail("keyboard live marker wrap");
            return;
        }
        self.messages.set_composer_text("");
        self.settings_view
            .keys()
            .probe_stage("switcher", "<Primary>f");
        probe_step("keyboard same-group conflict");
        if self
            .settings_view
            .keys()
            .probe_conflict_note("switcher")
            .is_none_or(|note| !note.contains("Search chats and messages"))
            || self.settings_view.keys().probe_save_sensitive()
            || self.settings_view.keys().probe_save()
        {
            probe_fail("keyboard conflict rejection");
            return;
        }
        self.settings_view.keys().probe_reset_all();
        probe_step("keyboard reset all");
        if self.settings_view.keys().probe_accel("switcher") != "<Control>k" {
            probe_fail("keyboard reset all");
            return;
        }

        self.settings_view.probe_show_page("ai");
        probe_step("AI key save and never prefill");
        self.settings_view
            .probe_ai_key_save("openai", "probe-openai-key");
        if !self.settings_view.probe_ai_key_is_set("openai")
            || self.settings_view.probe_ai_key_status("openai").as_deref() != Some("set")
            || !self.settings_view.probe_ai_key_entry_empty("openai")
        {
            probe_fail("AI key save");
            return;
        }
        self.settings_view.probe_show_page("appearance");
        self.settings_view.probe_show_page("ai");
        if !self.settings_view.probe_ai_key_entry_empty("openai") {
            probe_fail("AI key was prefilled");
            return;
        }
        probe_step("AI empty save preserves stored key");
        self.settings_view.probe_ai_key_save("openai", "");
        if !self.settings_view.probe_ai_key_is_set("openai")
            || !self.settings_view.probe_ai_key_entry_empty("openai")
        {
            probe_fail("AI empty save changed key");
            return;
        }
        probe_step("AI key clear");
        self.settings_view.probe_ai_key_clear("openai");
        if self.settings_view.probe_ai_key_is_set("openai")
            || self.settings_view.probe_ai_key_status("openai").as_deref() != Some("not set")
        {
            probe_fail("AI key clear");
            return;
        }

        self.settings_view.probe_show_page("account");
        probe_step("change credentials dialog save");
        if !self.settings_view.probe_open_change_credentials()
            || !self.settings_view.probe_open_change_credentials()
            || !self
                .settings_view
                .probe_submit_change_credentials("54321", PROBE_API_HASH)
            || !self.session_ready.get()
        {
            probe_fail("change credentials dialog save");
            return;
        }
        self.settings_view.dismiss_transients();

        self.settings
            .update(|settings| settings.ui.markdown_send = false);
        probe_step("markdown send flag off");
        if !poll_until(1000, || {
            self.flags_settled() && !self.desired_flags.get().markdown_send
        })
        .await
        {
            probe_fail("markdown send flag off");
            return;
        }
        self.settings
            .update(|settings| settings.ui.markdown_send = true);
        self.settings
            .update(|settings| settings.show_seconds = true);
        probe_step("show seconds");
        if !poll_until(1000, || {
            self.messages
                .last_time_label()
                .is_some_and(|text| time_has_seconds(&text))
        })
        .await
        {
            probe_fail("show seconds");
            return;
        }
        self.settings
            .update(|settings| settings.show_seconds = false);
        probe_step("hide seconds");
        if !poll_until(1000, || {
            self.messages
                .last_time_label()
                .is_some_and(|text| !time_has_seconds(&text))
        })
        .await
        {
            probe_fail("hide seconds");
            return;
        }
        self.close_settings();
        probe_step("close settings");
        if !poll_until(1000, || !self.settings_open()).await {
            probe_fail("close settings");
            return;
        }

        // Jump to today's date (detached), then ▼ reloads the latest page.
        self.clone().open_chat(marta);
        let today_end = Local::now()
            .date_naive()
            .and_hms_opt(23, 59, 59)
            .and_then(|naive| naive.and_local_timezone(Local).earliest())
            .unwrap_or_else(Local::now);
        self.clone().jump_to_date(today_end);
        probe_step("jump to date");
        if !poll_until(3000, || {
            self.messages.is_detached() && !self.messages.is_loading() && !self.messages.is_empty()
        })
        .await
        {
            probe_fail("jump to date");
            return;
        }
        self.messages.trigger_jump_to_latest();
        probe_step("reload latest");
        if !poll_until(3000, || {
            !self.messages.is_detached() && !self.messages.is_loading() && !self.messages.is_empty()
        })
        .await
        {
            probe_fail("reload latest");
            return;
        }

        // Wave 2: anti-delete must reach the backend before the forced reload.
        self.settings.update(|settings| settings.anti_delete = true);
        probe_step("find Deni");
        if !poll_until(3000, || self.flags_settled() && !self.messages.is_loading()).await {
            probe_fail("anti-delete flags before data");
            return;
        }
        let deni = self
            .chatlist
            .ordered()
            .into_iter()
            .find_map(|(id, title)| (title == "Deni").then_some(id));
        let Some(deni) = deni else {
            probe_fail("find Deni");
            return;
        };
        self.clone().open_chat(deni);
        probe_step("archived deleted row");
        if !poll_until(3000, || {
            self.open_chat.get() == Some(deni)
                && !self.messages.is_loading()
                && self.messages.contains(205)
                && self.messages.is_marked_deleted(205)
        })
        .await
        {
            probe_fail("archived deleted row");
            return;
        }

        let last_id = self
            .messages
            .last_message()
            .map(|message| message.id)
            .unwrap_or(0);
        self.messages.set_composer_text("please delete this");
        self.clone().submit_composer();
        probe_step("live deleted tombstone");
        if !poll_until(6000, || {
            self.messages
                .find_incoming_text_after("(mock reply) got it", last_id)
                .is_some_and(|msg_id| {
                    self.messages.is_marked_deleted(msg_id) && self.tombstone_contains(deni, msg_id)
                })
        })
        .await
        {
            probe_fail("live deleted tombstone");
            return;
        }

        self.settings
            .update(|settings| settings.anti_delete = false);
        probe_step("disable anti-delete reload");
        if !poll_until(3500, || {
            self.flags_settled()
                && !self.messages.is_loading()
                && !self.messages.contains(205)
                && self.messages.contains(204)
                && self.tombstones_empty(deni)
        })
        .await
        {
            probe_fail("disable anti-delete reload");
            return;
        }

        self.settings
            .update(|settings| settings.edit_history = true);
        let previous_last_id = self
            .messages
            .last_message()
            .map(|message| message.id)
            .unwrap_or(0);
        self.messages.set_composer_text("edit this");
        self.clone().submit_composer();
        probe_step("live edited reply");
        if !poll_until(6000, || {
            self.messages
                .find_edited_incoming_after(previous_last_id)
                .is_some()
        })
        .await
        {
            probe_fail("live edited reply");
            return;
        }
        let Some(edited_reply) = self.messages.find_edited_incoming_after(previous_last_id) else {
            probe_fail("find edited reply");
            return;
        };
        let edited_text = self
            .messages
            .message(edited_reply)
            .map(|message| message.text)
            .unwrap_or_default();
        self.messages.clear_history_probe();
        self.clone().open_edit_history(edited_reply);
        probe_step("edit history popover");
        if !poll_until(3000, || {
            self.messages.history_version_count() >= 1
                && self.messages.history_current_text().as_deref() == Some(edited_text.as_str())
        })
        .await
        {
            probe_fail("edit history popover");
            return;
        }
        self.messages.dismiss_row_popovers();

        let render_count = self.messages.initial_render_count();
        self.clone().force_reload(deni);
        self.clone().force_reload(deni);
        probe_step("force reload race");
        if !poll_until(3500, || {
            !self.messages.is_loading()
                && self.messages.initial_render_count() == render_count.wrapping_add(1)
                && self.messages.ids_unique()
                && self.messages.rendered_row_count() == self.messages.len()
        })
        .await
        {
            probe_fail("force reload race");
            return;
        }

        // Wave 3: local Omarchy chat, including the ticketed shell gate.
        self.clone().open_chat(OMARCHY_CHAT);
        probe_step("Omarchy seed");
        if !poll_until(1000, || {
            self.open_chat.get() == Some(OMARCHY_CHAT)
                && self
                    .virtual_stores
                    .borrow()
                    .get(&OMARCHY_CHAT)
                    .and_then(|store| store.msgs.last())
                    .is_some_and(|message| message.text.contains("Omarchy control"))
        })
        .await
        {
            probe_fail("Omarchy seed");
            return;
        }

        let before_help = virtual_last_id(&self.virtual_stores, OMARCHY_CHAT);
        self.messages.set_composer_text("help");
        self.clone().submit_composer();
        probe_step("Omarchy help");
        if !poll_until(1500, || {
            self.virtual_stores
                .borrow()
                .get(&OMARCHY_CHAT)
                .and_then(|store| store.msgs.last())
                .is_some_and(|message| {
                    message.id > before_help && message.text.contains("Omarchy control")
                })
        })
        .await
        {
            probe_fail("Omarchy help");
            return;
        }

        let before_shell_off = virtual_last_id(&self.virtual_stores, OMARCHY_CHAT);
        self.messages.set_composer_text("run echo hi");
        self.clone().submit_composer();
        probe_step("shell disabled");
        if !poll_until(1500, || {
            self.virtual_stores
                .borrow()
                .get(&OMARCHY_CHAT)
                .and_then(|store| store.msgs.last())
                .is_some_and(|message| {
                    message.id > before_shell_off && message.text.contains("shell commands are off")
                })
        })
        .await
        {
            probe_fail("shell disabled");
            return;
        }

        self.settings.update(|settings| settings.os.shell = true);
        self.probe_answer.set(Some(0));
        let mono_before = self
            .virtual_stores
            .borrow()
            .get(&OMARCHY_CHAT)
            .map(|store| store.mono_ids.len())
            .unwrap_or(0);
        self.messages.set_composer_text("run echo hi");
        self.clone().submit_composer();
        probe_step("shell ticket release");
        if !poll_until(1500, || {
            self.virtual_stores
                .borrow()
                .get(&OMARCHY_CHAT)
                .is_some_and(|store| store.in_flight == 0)
        })
        .await
            || self
                .virtual_stores
                .borrow()
                .get(&OMARCHY_CHAT)
                .is_none_or(|store| store.mono_ids.len() != mono_before)
        {
            probe_fail("shell cancel");
            return;
        }
        let release = match os::request_shell("x") {
            Ok(ticket) => ticket,
            Err(_) => {
                probe_fail("shell ticket release");
                return;
            }
        };
        os::cancel_shell(release);

        self.probe_answer.set(Some(1));
        let before_run = virtual_last_id(&self.virtual_stores, OMARCHY_CHAT);
        self.messages.set_composer_text("run echo hi");
        self.clone().submit_composer();
        probe_step("shell run");
        if !poll_until(2000, || {
            self.virtual_stores
                .borrow()
                .get(&OMARCHY_CHAT)
                .and_then(|store| store.msgs.last())
                .is_some_and(|message| {
                    message.id > before_run
                        && message.text == "hi"
                        && self.messages.is_monospace(message.id)
                })
        })
        .await
        {
            probe_fail("shell run");
            return;
        }

        self.settings.update(|settings| settings.os.shell = true);
        self.probe_answer.set(Some(1));
        let before_recheck = virtual_last_id(&self.virtual_stores, OMARCHY_CHAT);
        self.messages.set_composer_text("run echo no");
        self.clone().submit_composer();
        // The ticket exists now; turn the switch off before the scripted Run
        // answer is consumed so the confirmation-time re-check is exercised.
        self.settings.update(|settings| settings.os.shell = false);
        probe_step("shell confirmation re-check");
        if !poll_until(1500, || {
            self.virtual_stores
                .borrow()
                .get(&OMARCHY_CHAT)
                .and_then(|store| store.msgs.last())
                .is_some_and(|message| {
                    message.id > before_recheck && message.text.contains("shell commands are off")
                })
        })
        .await
        {
            probe_fail("shell confirmation re-check");
            return;
        }

        // Assistant commands use the offline AI provider in smoke mode.
        self.clone().open_chat(ASSISTANT_CHAT);
        let before_status = virtual_last_id(&self.virtual_stores, ASSISTANT_CHAT);
        self.messages.set_composer_text("/status");
        self.clone().submit_composer();
        probe_step("Assistant status");
        if !poll_until(2000, || {
            self.virtual_stores
                .borrow()
                .get(&ASSISTANT_CHAT)
                .and_then(|store| store.msgs.last())
                .is_some_and(|message| {
                    message.id > before_status && message.text.contains("mock (chat)")
                })
        })
        .await
        {
            probe_fail("Assistant status");
            return;
        }

        let before_hello = virtual_last_id(&self.virtual_stores, ASSISTANT_CHAT);
        self.messages.set_composer_text("hello");
        self.clone().submit_composer();
        probe_step("Assistant chat");
        if !poll_until(2500, || {
            self.virtual_stores
                .borrow()
                .get(&ASSISTANT_CHAT)
                .and_then(|store| store.msgs.last())
                .is_some_and(|message| {
                    message.id > before_hello && message.text.starts_with("(mock ai)")
                })
        })
        .await
        {
            probe_fail("Assistant chat");
            return;
        }

        let before_catchup = virtual_last_id(&self.virtual_stores, ASSISTANT_CHAT);
        self.messages.set_composer_text("/catchup marta");
        self.clone().submit_composer();
        probe_step("Assistant catchup");
        if !poll_until(3000, || {
            self.virtual_stores
                .borrow()
                .get(&ASSISTANT_CHAT)
                .and_then(|store| store.msgs.last())
                .is_some_and(|message| {
                    message.id > before_catchup && message.text.contains("Thursday")
                })
        })
        .await
        {
            probe_fail("Assistant catchup");
            return;
        }

        let mom = self
            .chatlist
            .ordered()
            .into_iter()
            .find_map(|(id, title)| (title == "Mom").then_some(id));
        let Some(mom) = mom else {
            probe_fail("find Mom");
            return;
        };
        self.clone().open_chat(mom);
        probe_step("open Mom");
        if !poll_until(2500, || {
            self.open_chat.get() == Some(mom)
                && !self.messages.is_loading()
                && self.messages.contains(301)
        })
        .await
        {
            probe_fail("open Mom");
            return;
        }
        self.clone().request_transcription(301);
        probe_step("voice transcript");
        if !poll_until(2500, || self.messages.aux_contains(301, "transcript:")).await {
            probe_fail("voice transcript");
            return;
        }
        let transcript_state = self.aux.borrow().transcripts.get(&(mom, 301)).cloned();
        self.clone().request_transcription(301);
        probe_step("voice transcript dedupe");
        if self.aux.borrow().transcripts.get(&(mom, 301)).cloned() != transcript_state {
            probe_fail("voice transcript dedupe");
            return;
        }

        self.clone().open_chat(marta);
        probe_step("draft target");
        if !poll_until(2500, || {
            self.open_chat.get() == Some(marta) && !self.messages.is_loading()
        })
        .await
        {
            probe_fail("reopen Marta for draft");
            return;
        }
        let Some(target) = self.messages.last_id() else {
            probe_fail("draft target");
            return;
        };
        self.clone().draft_reply(target);
        probe_step("AI draft");
        if !poll_until(2500, || {
            self.messages.composer_text().contains("(mock ai)") && self.messages.ai_draft_visible()
        })
        .await
        {
            probe_fail("AI draft");
            return;
        }
        self.messages.cancel_mode();
        probe_step("discard AI draft");
        if !self.messages.composer_text().is_empty() || self.messages.ai_draft_visible() {
            probe_fail("discard AI draft");
            return;
        }

        self.clone().open_chat(OMARCHY_CHAT);
        self.settings.update(|settings| settings.os.enabled = false);
        probe_step("disable Omarchy chat");
        if !poll_until(1000, || {
            self.open_chat.get().is_none()
                && self.messages.is_empty_state()
                && self
                    .chatlist
                    .ordered()
                    .iter()
                    .all(|(id, _)| *id != OMARCHY_CHAT)
        })
        .await
        {
            probe_fail("disable Omarchy chat");
            return;
        }

        // Wave 4: exercise every radio alternative, then run the full
        // phosphor hook traversal and prove all registered ticks stop when
        // animations are switched off again.
        for group in [RadioGroup::Entry, RadioGroup::Send, RadioGroup::Switch] {
            for id in group_ids(group) {
                self.settings
                    .update(|settings| select_radio(settings, id, group_ids(group)));
            }
        }
        self.settings.update(apply_full_phosphor);
        glib::timeout_future(Duration::from_millis(200)).await;

        self.clone().open_chat(marta);
        probe_step("animation open chat");
        if !poll_until(2500, || {
            self.open_chat.get() == Some(marta) && !self.messages.is_loading()
        })
        .await
        {
            probe_fail("animation open chat");
            return;
        }
        glib::timeout_future(Duration::from_millis(200)).await;

        self.messages.set_composer_text("hi");
        self.clone().submit_composer();
        probe_step("animation receive");
        if !poll_until(2500, || self.messages.find_outgoing_text("hi").is_some()).await {
            probe_fail("animation send");
            return;
        }
        self.clone().open_chat(deni);
        glib::timeout_future(Duration::from_millis(200)).await;
        self.clone().open_chat(marta);
        glib::timeout_future(Duration::from_millis(200)).await;
        self.clone().open_chat(deni);
        probe_step("animation mention");
        if !poll_until(3500, || self.chatlist.unread(marta) > 0).await {
            probe_fail("animation mention");
            return;
        }
        glib::timeout_future(Duration::from_millis(200)).await;

        // ----- wave 6C: polls, location, scheduled (spec §1.11) -----
        self.clone().open_chat(marta);
        if !poll_until(3500, || {
            self.open_chat.get() == Some(marta)
                && !self.messages.is_loading()
                && !self.messages.is_empty()
        })
        .await
        {
            probe_fail("6C open Marta");
            return;
        }

        // The attach menu must offer all three entries and actually run the
        // item that was clicked.
        self.messages.show_attach_menu();
        probe_step("attach menu");
        if !poll_until(1000, || self.messages.attach_menu_open()).await {
            probe_fail("attach menu open");
            return;
        }
        let items = self.messages.attach_menu_items();
        if items.len() != 3
            || !items[0].contains("File")
            || !items[1].contains("Poll")
            || !items[2].contains("Location")
        {
            probe_fail("attach menu items");
            return;
        }
        if !self.messages.probe_attach_menu_click("Poll") {
            probe_fail("attach menu click");
            return;
        }

        probe_step("poll dialog open");
        if !poll_until(1500, || self.poll_dialog.is_open()).await {
            probe_fail("poll dialog open");
            return;
        }
        if self.messages.attach_menu_open() {
            probe_fail("poll dialog open: attach menu still up");
            return;
        }

        // A completion from an earlier open must not close or unlock the
        // replacement dialog. Submit, cancel, and reopen before the spawned
        // future gets a chance to poll.
        probe_step("poll dialog generation guard");
        self.poll_dialog
            .probe_fill("Old dialog poll", &["First", "Second"]);
        let stale_polls_before = self
            .messages
            .messages()
            .iter()
            .filter(|message| message.media == Some(MediaKind::Poll))
            .count();
        let poll_mutations_before = self.mutations_in_flight.get();
        self.poll_dialog.probe_submit();
        self.close_poll_dialog();
        self.clone().open_poll_dialog();
        self.poll_dialog
            .probe_fill("Replacement dialog", &["Keep", "Open"]);
        if !poll_until(4_000, || {
            self.messages
                .messages()
                .iter()
                .filter(|message| message.media == Some(MediaKind::Poll))
                .count()
                > stale_polls_before
                && self.mutations_in_flight.get() <= poll_mutations_before
        })
        .await
            || !self.poll_dialog.is_open()
            || !self.poll_dialog.probe_can_create()
        {
            probe_fail("poll dialog generation guard");
            return;
        }

        // One option is not a poll: Create must stay disabled until a second
        // one is filled in (spec §4.5).
        probe_step("poll dialog validate");
        self.poll_dialog.probe_fill("Ridge or lake?", &["Ridge"]);
        if self.poll_dialog.probe_can_create() {
            probe_fail("poll dialog validate: enabled with one option");
            return;
        }
        self.poll_dialog.probe_fill("Ridge or lake?", &["Ridge", "Lake"]);
        if !self.poll_dialog.probe_can_create() {
            probe_fail("poll dialog validate: disabled with two options");
            return;
        }
        // Quiz mode needs a correct answer before Create comes back.
        self.poll_dialog.probe_set_quiz(true);
        if self.poll_dialog.probe_can_create() {
            probe_fail("poll dialog validate: quiz without an answer");
            return;
        }
        if !self.poll_dialog.probe_set_correct(1) || !self.poll_dialog.probe_can_create() {
            probe_fail("poll dialog validate: quiz with an answer");
            return;
        }
        self.poll_dialog.probe_set_quiz(false);
        // Removing an option below the minimum is refused.
        self.poll_dialog.probe_remove_last();
        if self.poll_dialog.probe_option_count() != 2 {
            probe_fail("poll dialog validate: dropped below two options");
            return;
        }

        probe_step("poll dialog send");
        let polls_before = self
            .messages
            .messages()
            .iter()
            .filter(|message| message.media == Some(MediaKind::Poll))
            .count();
        self.poll_dialog.probe_submit();
        if !poll_until(4000, || {
            !self.poll_dialog.is_open()
                && self
                    .messages
                    .messages()
                    .iter()
                    .filter(|message| message.media == Some(MediaKind::Poll))
                    .count()
                    > polls_before
        })
        .await
        {
            probe_fail("poll dialog send");
            return;
        }
        if !self
            .messages
            .messages()
            .iter()
            .any(|message| message.poll.as_ref().is_some_and(|poll| poll.question == "Ridge or lake?"))
        {
            probe_fail("poll dialog send: the sent poll is missing");
            return;
        }

        // Location: out-of-range coordinates block Send, a valid point sends.
        self.clone().open_location_dialog();
        if !poll_until(1500, || self.location_dialog.is_open()).await {
            probe_fail("location dialog open");
            return;
        }
        probe_step("location requires deliberate selection");
        if self.location_dialog.probe_can_send() { probe_fail("unconfirmed default location is sendable"); return; }
        probe_step("location place search and deliberate system pin");
        self.location_dialog.probe_search_places("Museum");
        if !poll_until(3_000, || self.location_dialog.probe_place_count() == 1).await { probe_fail("location place search"); return; }
        self.location_dialog.probe_pick_place();
        if !self.location_dialog.probe_can_send() { probe_fail("location selected place"); return; }
        self.location_dialog.probe_current_location();
        if !poll_until(1_000, || self.location_dialog.probe_location_status().starts_with("Estimated accuracy")).await
            || self.location_dialog.probe_can_send() { probe_fail("location system pin confirmation"); return; }
        probe_step("location dialog generation guard");
        self.location_dialog.probe_set_point(52.51, 13.40);
        let stale_locations_before = self
            .messages
            .messages()
            .iter()
            .filter(|message| message.media == Some(MediaKind::Location))
            .count();
        let location_mutations_before = self.mutations_in_flight.get();
        self.location_dialog.probe_send();
        self.close_location_dialog();
        self.clone().open_location_dialog();
        if !poll_until(4_000, || {
            self.messages
                .messages()
                .iter()
                .filter(|message| message.media == Some(MediaKind::Location))
                .count()
                > stale_locations_before
                && self.mutations_in_flight.get() <= location_mutations_before
        })
        .await
            || !self.location_dialog.is_open()
            || self.location_dialog.probe_can_send()
        {
            probe_fail("location dialog generation guard");
            return;
        }
        probe_step("location dialog send");
        self.location_dialog.probe_type_coords("120.0", "13.405");
        if self.location_dialog.probe_can_send() || self.location_dialog.probe_error().is_empty() {
            probe_fail("location dialog send: out-of-range point accepted");
            return;
        }
        self.location_dialog.probe_set_point(52.52, 13.405);
        if !self.location_dialog.probe_can_send() {
            probe_fail("location dialog send: valid point refused");
            return;
        }
        let locations_before = self
            .messages
            .messages()
            .iter()
            .filter(|message| message.media == Some(MediaKind::Location))
            .count();
        self.location_dialog.probe_send();
        if !poll_until(4000, || {
            !self.location_dialog.is_open()
                && self
                    .messages
                    .messages()
                    .iter()
                    .filter(|message| message.media == Some(MediaKind::Location))
                    .count()
                    > locations_before
        })
        .await
        {
            probe_fail("location dialog send");
            return;
        }
        if self.ui_state.borrow().last_location != Some((52.52, 13.405)) {
            probe_fail("location dialog send: point not remembered");
            return;
        }

        // Picking a grid cell re-centres the point and refetches the tiles.
        self.clone().open_location_dialog();
        if !poll_until(1500, || self.location_dialog.is_open()).await {
            probe_fail("location dialog pick: not open");
            return;
        }
        probe_step("location dialog pick");
        if !self.location_dialog.probe_grid_visible() {
            probe_fail("location dialog pick: no map grid");
            return;
        }
        if !poll_until(4000, || self.location_dialog.probe_tiles_loaded() == 9).await {
            probe_fail("location dialog pick: tiles never arrived");
            return;
        }
        self.location_dialog.probe_confirm_point();
        if !self.location_dialog.probe_can_send() { probe_fail("confirmed location is not sendable"); return; }
        probe_step("location map unavailable");
        self.location_dialog.probe_fail_map_fetch();
        if !self.location_dialog.probe_map_error().contains("Map unavailable")
            || !self.location_dialog.probe_can_send()
        {
            probe_fail("location map unavailable: no usable inline error");
            return;
        }
        self.location_dialog.probe_retry_map();
        if !poll_until(4000, || {
            self.location_dialog.probe_map_error().is_empty()
                && self.location_dialog.probe_tiles_loaded() == 9
        })
        .await
        {
            probe_fail("location map unavailable: retry");
            return;
        }
        let before = self.location_dialog.point();
        self.location_dialog.probe_click_cell(0);
        let after = self.location_dialog.point();
        if after == before || after.lat <= before.lat || after.lon >= before.lon {
            probe_fail("location dialog pick: the pin did not move north-west");
            return;
        }
        self.location_dialog.probe_zoom_out();
        if self.location_dialog.probe_zoom() != 11 {
            probe_fail("location dialog pick: zoom out");
            return;
        }
        if !poll_until(4000, || self.location_dialog.probe_tiles_loaded() == 9).await {
            probe_fail("location dialog pick: tiles never refetched");
            return;
        }
        self.close_location_dialog();

        // Send later: the picker refuses a past time and schedules a future
        // one; the strip then shows the two fixtures plus this message.
        self.messages.set_composer_text("see you later");
        self.messages.show_send_later();
        probe_step("send later open");
        if !poll_until(1000, || self.messages.send_later_open()).await {
            probe_fail("send later open");
            return;
        }
        self.messages
            .probe_send_later_set(Local::now() - chrono::Duration::hours(1));
        if !self.messages.probe_send_later_schedule() {
            probe_fail("send later open: no picker");
            return;
        }
        if !self.messages.send_later_open() || self.messages.probe_send_later_error().is_empty() {
            probe_fail("send later open: a past time was accepted");
            return;
        }

        probe_step("send later send");
        let scheduled_before = self.messages.scheduled_count();
        self.messages
            .probe_send_later_set(Local::now() + chrono::Duration::hours(2));
        self.messages.probe_send_later_schedule();
        if !poll_until(4000, || {
            !self.messages.send_later_open()
                && self.messages.scheduled_count() == scheduled_before + 1
                && self.messages.composer_text().is_empty()
        })
        .await
        {
            probe_fail("send later send");
            return;
        }

        probe_step("scheduled strip");
        if self.messages.scheduled_count() != 3 {
            probe_fail("scheduled strip: count is not 3");
            return;
        }
        if self.messages.probe_scheduled_bar_label() != "3 scheduled messages" {
            probe_fail("scheduled strip: label");
            return;
        }
        probe_step("scheduled refresh generation guard");
        let scheduled_count = self.messages.scheduled_count();
        let stale_generation = self.scheduled_refresh_generation.get();
        self.clone().refresh_scheduled(marta);
        if self.apply_scheduled_refresh(
            marta,
            self.epoch.get(),
            self.session_epoch.get(),
            stale_generation,
            Vec::new(),
        ) || self.messages.scheduled_count() != scheduled_count
        {
            probe_fail("scheduled refresh generation guard");
            return;
        }
        self.messages.probe_scheduled_toggle();
        if !self.messages.scheduled_panel_open() || self.messages.probe_scheduled_rows() != 3 {
            probe_fail("scheduled strip: panel");
            return;
        }

        // Send now: the soonest scheduled message becomes a real one.
        probe_step("scheduled send now");
        let ids = self.messages.scheduled_ids();
        let Some(&first) = ids.first() else {
            probe_fail("scheduled send now: nothing scheduled");
            return;
        };
        if !self.messages.probe_scheduled_send_now(first) {
            probe_fail("scheduled send now: no row");
            return;
        }
        if !poll_until(4000, || {
            self.messages.scheduled_count() == 2
                && self.messages.find_outgoing_text("see you later").is_some()
        })
        .await
        {
            probe_fail("scheduled send now");
            return;
        }

        // Delete: the strip empties and hides itself.
        probe_step("scheduled delete");
        for id in self.messages.scheduled_ids() {
            if !self.messages.probe_scheduled_delete(id) {
                probe_fail("scheduled delete: no row");
                return;
            }
            if !poll_until(4000, || !self.messages.scheduled_ids().contains(&id)).await {
                probe_fail("scheduled delete");
                return;
            }
        }
        if self.messages.scheduled_count() != 0 || self.messages.scheduled_panel_open() {
            probe_fail("scheduled delete: the strip survived");
            return;
        }

        // A successful scheduled attachment must clear the persisted draft as
        // well as the visible, signal-blocked composer text.
        probe_step("scheduled file clears draft");
        let file_caption = "scheduled attachment caption";
        self.messages.set_composer_text(file_caption);
        self.capture_current_draft(marta);
        let file_token = self.acquire_composer();
        self.messages.set_busy(true);
        self.clone().send_file_later(
            PathBuf::from("/tmp/omarchygram-probe-attachment.txt"), FileSendContext { chat_id: marta, epoch: self.epoch.get(), session_epoch: self.session_epoch.get(), token: file_token },
            file_caption.to_string(),
            file_caption.to_string(),
            Local::now() + chrono::Duration::hours(3),
        );
        if !poll_until(4_000, || {
            self.messages.scheduled_count() == 1 && self.messages.composer_text().is_empty()
        })
        .await
        {
            probe_fail("scheduled file clears draft: send");
            return;
        }
        self.clone().open_chat(deni);
        self.clone().open_chat(marta);
        if !poll_until(4_000, || {
            self.open_chat.get() == Some(marta)
                && !self.messages.is_loading()
                && self.messages.composer_text().is_empty()
                && self.messages.scheduled_count() == 1
        })
        .await
        {
            probe_fail("scheduled file clears draft: reopened caption");
            return;
        }
        let scheduled_file_ids = self.messages.scheduled_ids();
        self.clone().delete_scheduled(marta, scheduled_file_ids);
        if !poll_until(4_000, || self.messages.scheduled_count() == 0).await {
            probe_fail("scheduled file clears draft: cleanup");
            return;
        }

        self.effects.theme_switched(&self.overlay);
        glib::timeout_future(Duration::from_millis(200)).await;
        self.effects.launched(&self.overlay);
        glib::timeout_future(Duration::from_millis(200)).await;

        self.settings.update(super::anim::apply_purist);
        glib::timeout_future(Duration::from_millis(200)).await;
        if self.effects.live_tick_count() != 0 {
            probe_fail("animation tick cleanup");
            return;
        }

        // ---- wave 6E: bots and forums (specs/spec-wave6.md §6, §1.11) ----
        let chat_by_title = |wanted: &str| {
            self.chatlist
                .ordered()
                .into_iter()
                .find_map(|(id, title)| (title == wanted).then_some(id))
        };
        let (Some(bot_id), Some(helper_id), Some(forum_id)) = (
            chat_by_title("Omarchy Bot"),
            chat_by_title("Helper Bot"),
            chat_by_title("Omarchy Forum"),
        ) else {
            probe_fail("6E fixtures missing");
            return;
        };

        self.clone().open_chat(bot_id);
        if !poll_until(4000, || {
            self.open_chat.get() == Some(bot_id)
                && !self.messages.is_loading()
                && self.chat_info.borrow().contains_key(&bot_id)
        })
        .await
        {
            probe_fail("open Omarchy Bot");
            return;
        }
        let keyboard_id = self
            .messages
            .messages()
            .into_iter()
            .find(|message| message.keyboard.is_some())
            .map(|message| message.id);
        let Some(keyboard_id) = keyboard_id else {
            probe_fail("bot keyboard message");
            return;
        };
        let first_button = |this: &Rc<Self>| {
            this.messages
                .keyboard_button_labels(keyboard_id)
                .first()
                .cloned()
                .unwrap_or_default()
        };
        if first_button(&self) != "Next theme" {
            probe_fail("bot keyboard render");
            return;
        }

        // A Callback button: the answer renames the button (MessageChanged).
        probe_step("bot keyboard rebuild focus");
        if !self
            .messages
            .focus_keyboard_button(keyboard_id, "Lock")
        {
            probe_fail("bot keyboard focus setup");
            return;
        }
        probe_step("bot keyboard callback");
        if !self
            .messages
            .click_keyboard_button(keyboard_id, "Next theme")
        {
            probe_fail("bot keyboard next-theme button");
            return;
        }
        if !poll_until(4000, || {
            first_button(&self) == "Next theme ✓"
                && self
                    .messages
                    .message(keyboard_id)
                    .and_then(|message| message.keyboard)
                    .and_then(|keyboard| keyboard.rows.first().and_then(|row| row.first()).cloned())
                    .is_some_and(|button| button.text == "Next theme ✓")
        })
        .await
        {
            probe_fail("bot keyboard callback label");
            return;
        }
        if !self.messages.composer_has_focus() {
            probe_fail("bot keyboard rebuild did not move focus");
            return;
        }

        // A Callback button the bot answers with an alert: the info bar.
        probe_step("bot keyboard alert");
        if !self.messages.click_keyboard_button(keyboard_id, "Lock") {
            probe_fail("bot keyboard lock button");
            return;
        }
        if !poll_until(4000, || {
            self.messages.info_visible()
                && self.messages.error_text() == "Locked (mock)"
                && self.messages.keyboard_button_ready(keyboard_id, "Lock")
        })
        .await
        {
            probe_fail("bot keyboard alert info bar");
            return;
        }

        // A Url button: counted, never launched (the probe owns no browser).
        probe_step("bot keyboard url");
        let launches_before = self.probe_media_launches.get();
        if !self.messages.click_keyboard_button(keyboard_id, "Docs") {
            probe_fail("bot keyboard docs button");
            return;
        }
        if !poll_until(2000, || {
            self.probe_media_launches.get() == launches_before.wrapping_add(1)
        })
        .await
        {
            probe_fail("bot keyboard url launch");
            return;
        }
        glib::timeout_future(Duration::from_millis(200)).await;
        if self.probe_media_launches.get() != launches_before.wrapping_add(1) {
            probe_fail("bot keyboard url launched twice");
            return;
        }

        // A SwitchInline button: "@bot query" into this chat's composer.
        probe_step("bot keyboard switch inline");
        if !self.messages.click_keyboard_button(keyboard_id, "Search") {
            probe_fail("bot keyboard search button");
            return;
        }
        if !poll_until(2000, || {
            self.messages.composer_text() == "@omarchy_bot omarchy"
        })
        .await
        {
            probe_fail("bot keyboard switch inline composer");
            return;
        }

        // Escape is consumed by the switcher's entry controller, so its
        // explicit cancellation callback must discard SwitchInline text.
        probe_step("bot switch inline cancel");
        self.clone()
            .switch_inline("stale".into(), false, "omarchy_bot".into());
        if !self.switcher.is_open() || self.pending_inline_query.borrow().is_none() {
            probe_fail("bot switch inline pending setup");
            return;
        }
        self.switcher.cancel();
        if self.switcher.is_open()
            || self.pending_inline_query.borrow().is_some()
            || !self.messages.composer_has_focus()
        {
            probe_fail("bot switch inline cancel retained pending text");
            return;
        }

        // "/" lists the bot's commands; Enter fills the selected one.
        probe_step("bot command autocomplete");
        self.messages.set_composer_text("/");
        if !poll_until(2000, || {
            self.messages.command_popover_open() && self.messages.command_popover_rows() == 4
        })
        .await
        {
            probe_fail("bot command popover");
            return;
        }
        self.messages.set_composer_text("/the");
        if !poll_until(2000, || {
            self.messages.command_popover_rows() == 1
                && self.messages.command_popover_selected().as_deref() == Some("/theme")
        })
        .await
        {
            probe_fail("bot command filter");
            return;
        }
        self.messages.command_popover_activate();
        if !poll_until(2000, || {
            self.messages.composer_text() == "/theme " && !self.messages.command_popover_open()
        })
        .await
        {
            probe_fail("bot command fill");
            return;
        }
        self.messages.set_composer_text("");

        // An empty bot chat shows Start in place of the composer.
        self.clone().open_chat(helper_id);
        probe_step("bot username seeded on reset");
        if self.messages.bot_username() != "omarchy_helper_bot" {
            probe_fail("bot username was not seeded synchronously");
            return;
        }
        if !poll_until(4000, || {
            self.open_chat.get() == Some(helper_id) && !self.messages.is_loading()
        })
        .await
        {
            probe_fail("open Helper Bot");
            return;
        }
        probe_step("bot start");
        if !poll_until(3000, || {
            self.messages.start_button_visible()
                && self.messages.start_button_has_focus()
                && !self.messages.composer_visible()
        })
        .await
        {
            probe_fail("bot start button");
            return;
        }
        self.messages.click_start();
        if !poll_until(5000, || {
            self.messages
                .messages()
                .iter()
                .any(|message| message.outgoing && message.text == "/start")
                && !self.messages.start_button_visible()
                && self.messages.composer_visible()
        })
        .await
        {
            probe_fail("bot start sends /start");
            return;
        }

        // A forum opens its topic list, pinned first.
        self.clone().open_chat(forum_id);
        probe_step("forum topics list");
        if !poll_until(5000, || {
            self.open_chat.get() == Some(forum_id)
                && self.content_area.visible_child_name().as_deref() == Some("topics")
                && self.topics.topic_titles().len() == 4
        })
        .await
        {
            probe_fail("forum topic list");
            return;
        }
        if self.topics.topic_titles().first().map(String::as_str) != Some("Bugs") {
            probe_fail("forum pinned topic first");
            return;
        }

        probe_step("forum topic refresh focus");
        if !self.topics.focus_index(0) {
            probe_fail("forum topic focus setup");
            return;
        }
        let generation = self.forum_topics_generation.get();
        let current_topics = self.forum_topics.borrow().clone();
        if !self.apply_forum_topics(forum_id, self.epoch.get(), generation, current_topics)
            || !self.topics.header_control_has_focus()
        {
            probe_fail("forum topic refresh did not preserve safe focus");
            return;
        }

        probe_step("forum stale refresh ignored");
        let visible_topics = self.topics.topic_titles();
        if self.apply_forum_topics(
            forum_id,
            self.epoch.get(),
            generation.wrapping_sub(1),
            Vec::new(),
        ) || self.topics.topic_titles() != visible_topics
        {
            probe_fail("forum stale refresh replaced current rows");
            return;
        }

        // Opening a topic: history of that topic only, in a message pane.
        let themes_row = self
            .topics
            .topic_titles()
            .iter()
            .position(|title| title == "Themes");
        let Some(themes_row) = themes_row else {
            probe_fail("forum Themes topic missing");
            return;
        };
        let themes_chat = self
            .forum_topics
            .borrow()
            .iter()
            .find(|topic| topic.title == "Themes")
            .map(|topic| topic.chat_id);
        let Some(themes_chat) = themes_chat else {
            probe_fail("forum Themes chat id");
            return;
        };
        self.topics.open_index(themes_row as i32);
        probe_step("forum open topic");
        if !poll_until(5000, || {
            self.open_chat.get() == Some(themes_chat)
                && self.content_area.visible_child_name().as_deref() == Some("messages")
                && !self.messages.is_loading()
                && self.messages.len() == 8
        })
        .await
        {
            probe_fail("forum open topic");
            return;
        }

        probe_step("scheduled topic refresh");
        let refresh_generation = self.scheduled_refresh_generation.get();
        self.handle_event(Event::ScheduledChanged { chat_id: forum_id });
        if self.open_chat.get() != Some(themes_chat)
            || self.scheduled_refresh_generation.get() == refresh_generation
        {
            probe_fail("scheduled topic refresh");
            return;
        }

        probe_step("forum topic header");
        if self.messages.header_title() != "Omarchy Forum › Themes" {
            probe_fail("forum topic breadcrumb");
            return;
        }
        if !self.messages.back_button_visible() || self.chatlist.selected() != Some(forum_id) {
            probe_fail("forum topic header chrome");
            return;
        }
        self.messages.click_back();
        if !poll_until(4000, || {
            self.open_chat.get() == Some(forum_id)
                && self.content_area.visible_child_name().as_deref() == Some("topics")
                && self.topics.topic_titles().len() == 4
        })
        .await
        {
            probe_fail("forum back to topic list");
            return;
        }

        // New topic: the dialog creates it, the list grows, it opens.
        probe_step("forum create topic");
        self.topics.open_dialog();
        if !self.topics.dialog_is_open() || !self.topics.cancel_visible() {
            probe_fail("new topic dialog");
            return;
        }
        self.topics.set_dialog_title("Probe Topic");
        probe_step("forum create topic retry state");
        self.topics.show_create_error("probe: retryable failure");
        if !self.topics.create_retry_visible()
            || self.topics.create_error_text() != "probe: retryable failure"
        {
            probe_fail("new topic retry affordance");
            return;
        }
        self.topics.submit_dialog();
        if !self.topics.dialog_is_open() || !self.topics.create_is_pending() {
            probe_fail("new topic dialog closed while pending");
            return;
        }
        probe_step("forum topic cancel while pending");
        if !self.topics.cancel_usable() {
            probe_fail("Cancel became insensitive during topic creation");
            return;
        }
        self.topics.probe_cancel();
        if self.topics.dialog_is_open() || !self.topics.create_is_pending() {
            probe_fail("Cancel did not dismiss an in-flight topic dialog");
            return;
        }
        if environment_listed("OMG_MOCK_FAIL_ONCE", "CreateTopic") {
            probe_step("forum create topic backend retry");
            if !poll_until(4000, || !self.topics.create_is_pending()).await {
                probe_fail("stale topic error left create pending");
                return;
            }
            self.topics.open_dialog();
            self.topics.set_dialog_title("Probe Topic");
            self.topics.submit_dialog();
        }
        if !poll_until(5000, || {
            self.open_chat
                .get()
                .and_then(split_topic_chat_id)
                .is_some_and(|(forum, _)| forum == forum_id)
                && self.messages.header_title() == "Omarchy Forum › Probe Topic"
                && !self.topics.dialog_is_open()
        })
        .await
        {
            probe_fail("forum create topic opens it");
            return;
        }
        self.messages.click_back();
        if !poll_until(5000, || {
            self.content_area.visible_child_name().as_deref() == Some("topics")
                && self.topics.topic_titles().len() == 5
                && self
                    .topics
                    .topic_titles()
                    .iter()
                    .any(|title| title == "Probe Topic")
        })
        .await
        {
            probe_fail("forum create topic row");
            return;
        }

        // A message sent in a topic is answered in that topic only.
        let themes_row = self
            .topics
            .topic_titles()
            .iter()
            .position(|title| title == "Themes");
        let Some(themes_row) = themes_row else {
            probe_fail("forum Themes topic after create");
            return;
        };
        self.topics.open_index(themes_row as i32);
        if !poll_until(5000, || {
            self.open_chat.get() == Some(themes_chat) && !self.messages.is_loading()
        })
        .await
        {
            probe_fail("reopen forum topic");
            return;
        }
        probe_step("forum topic new message");
        let known_ids: Vec<i32> = self
            .messages
            .messages()
            .into_iter()
            .map(|message| message.id)
            .collect();
        self.messages.set_composer_text("ping from probe");
        self.clone().submit_composer();
        if !poll_until(9000, || {
            self.messages.messages().iter().any(|message| {
                !message.outgoing
                    && !known_ids.contains(&message.id)
                    && message.topic_id == Some(20)
            })
        })
        .await
        {
            probe_fail("forum topic reply");
            return;
        }
        let reply_id = self
            .messages
            .messages()
            .into_iter()
            .filter(|message| !message.outgoing && !known_ids.contains(&message.id))
            .map(|message| message.id)
            .max();
        let Some(reply_id) = reply_id else {
            probe_fail("forum topic reply id");
            return;
        };
        let general_chat = self
            .forum_topics
            .borrow()
            .iter()
            .find(|topic| topic.title == "General")
            .map(|topic| topic.chat_id);
        let Some(general_chat) = general_chat else {
            probe_fail("forum General topic missing");
            return;
        };
        self.clone().open_chat(general_chat);
        if !poll_until(5000, || {
            self.open_chat.get() == Some(general_chat)
                && !self.messages.is_loading()
                && self.messages.len() >= 5
        })
        .await
        {
            probe_fail("open General topic");
            return;
        }
        if self.messages.message(reply_id).is_some() {
            probe_fail("forum topic reply leaked into General");
            return;
        }
        if self.messages.header_title() != "Omarchy Forum › General" {
            probe_fail("forum General breadcrumb");
            return;
        }

        if !self.run_media_profile_probe().await { return; }

        probe_step("logout stops active player");
        self.clone().open_chat(media_lab);
        if !poll_until(5_000, || {
            self.open_chat.get() == Some(media_lab)
                && !self.messages.is_loading()
                && self.messages.player_exists(800)
        })
        .await
        {
            probe_fail("open Media Lab before logout playback check");
            return;
        }
        let _ = self.scroll_into_view(800).await;
        if !self.messages.trigger_media(800)
            || !poll_until(5_000, || {
                matches!(
                    self.messages.player_state(800),
                    player::PlayerState::Playing | player::PlayerState::Error
                )
            })
            .await
        {
            probe_fail("player did not settle before logout");
            return;
        }

        probe_step("logout clears stories");
        self.open_stories_for_peer(1);
        if !poll_until(4_000, || self.stories_viewer.is_open()).await {
            probe_fail("logout clears stories: viewer did not open");
            return;
        }
        if !self.settings_open() {
            self.toggle_settings();
        }
        self.settings_view.probe_show_page("account");
        probe_step("log out");
        if !self.settings_view.probe_open_logout_dialog()
            || !self.settings_view.probe_confirm_logout()
        {
            probe_fail("log out confirm dialog");
            return;
        }
        if self.stories_viewer.is_open()
            || !self.stories_viewer.probe_state_is_clear()
            || self.stories_strip.peer_count() != 0
            || !self.messages.player_registry_empty()
            || self.messages.active_player_count() != 0
            || self.messages.video_recorder_visible()
        {
            probe_fail("log out left account-scoped story/player state active");
            return;
        }
        let fail_once = std::env::var("OMG_MOCK_FAIL_ONCE")
            .is_ok_and(|value| value.split(',').any(|name| name.trim() == "LogOut"));
        if fail_once {
            probe_step("log out inline retry");
            if !poll_until(3000, || {
                self.session_ready.get()
                    && self
                        .settings_view
                        .probe_logout_error()
                        .is_some_and(|error| error.contains("mock: transient failure"))
            })
            .await
            {
                probe_fail("log out failure state");
                return;
            }
            if !self.settings_view.probe_open_logout_dialog()
                || !self.settings_view.probe_confirm_logout()
            {
                probe_fail("log out retry confirm dialog");
                return;
            }
        }
        if !poll_until(3000, || {
            !self.session_ready.get()
                && self.stack.visible_child_name().as_deref() == Some("auth")
                && self.auth.state() == AuthState::NeedPhone
                && !self.settings_view.probe_logout_dialog_open()
        })
        .await
        {
            probe_fail("log out phone step");
            return;
        }

        let Some(window) = self.window() else {
            probe_fail("find application window");
            return;
        };

        let Some(application) = window.application() else {
            probe_fail("find application");
            return;
        };
        application.quit();
    }

    async fn run_media_profile_probe(self: &Rc<Self>) -> bool {
        probe_step("group sender opens profile without changing chat");
        self.clone().open_chat(4);
        if !poll_until(4000, || self.messages.contains(401) && !self.messages.is_loading()).await {
            probe_fail("sender profile source chat"); return false;
        }
        let count = self.chatlist.ordered().len();
        if !self.messages.probe_open_sender(401)
            || !poll_until(4000, || self.profile.info().is_some_and(|info| info.id == 4001)).await
            || self.open_chat.get() != Some(4) || self.chatlist.ordered().len() != count {
            probe_fail("sender profile changed conversation or failed to load"); return false;
        }
        if self.settings.get().ui.show_avatars
            && !poll_until(4000, || self.profile.photo_loaded()).await {
            probe_fail("sender avatar did not load"); return false;
        }
        self.probe_capture("sender-profile").await;
        probe_step("full profile photo loads and closes back to sender details");
        self.profile.probe_photo();
        if !poll_until(4000, || self.viewer.profile_peer() == Some(4001) && self.viewer.image_ready()).await {
            probe_fail("enlarged sender photo"); return false;
        }
        self.probe_capture("profile-photo").await;
        let launches = self.probe_media_launches.get();
        self.viewer.probe_open();
        if self.probe_media_launches.get() != launches + 1 {
            probe_fail("profile photo external action"); return false;
        }
        self.viewer.probe_close();
        if self.viewer.is_open() || !self.profile.is_open() || self.open_chat.get() != Some(4) {
            probe_fail("profile photo return navigation"); return false;
        }
        probe_step("profile message action opens private conversation");
        self.profile.probe_message();
        if !poll_until(4000, || self.open_chat.get() == Some(4001) && !self.messages.is_loading()).await
            || self.profile.is_open() {
            probe_fail("sender profile message action"); return false;
        }
        probe_step("profile generation rejects a superseded user");
        self.open_profile(4001, "Robin", None);
        self.open_profile(4002, "Ada", None);
        if !poll_until(4000, || self.profile.info().is_some_and(|info| info.id == 4002)).await {
            probe_fail("late sender details"); return false;
        }
        self.close_profile();
        self.clone().open_chat(4);
        if !self.ui_state.borrow().info_panel_open { self.toggle_info_panel(); }
        if !poll_until(4000, || self.info.is_bound(4) && !self.messages.is_loading()).await {
            probe_fail("group info binding"); return false;
        }
        probe_step("group info photo expands");
        self.info.probe_photo();
        if !poll_until(4000, || self.viewer.profile_peer() == Some(4) && self.viewer.image_ready()).await {
            probe_fail("group photo enlargement"); return false;
        }
        self.viewer.probe_key(gdk::Key::Escape);

        probe_step("photo viewer recovers from an unreadable cache file");
        self.clone().open_chat(1);
        if !poll_until(4000, || self.messages.contains(100) && self.messages.media_path(100).is_some()).await {
            probe_fail("photo recovery fixture"); return false;
        }
        self.open_viewer(100);
        if !poll_until(4000, || self.viewer.image_ready()).await { probe_fail("photo viewer decode"); return false; }
        let broken = std::env::temp_dir().join(format!("omg-probe-corrupt-media-{}.bin", std::process::id()));
        if std::fs::write(&broken, b"invalid media fixture").is_err() { probe_fail("write corrupt fixture"); return false; }
        self.viewer.set_path(100, self.viewer_generation.get(), broken.clone());
        if !poll_until(3000, || self.viewer.retry_visible()).await { probe_fail("photo failure has no retry"); return false; }
        self.viewer.probe_retry();
        if !poll_until(4000, || self.viewer.image_ready()).await { probe_fail("photo retry decode"); return false; }
        self.close_viewer();
        probe_step("voice playback error permits retry and resumes audio");
        self.clone().open_chat(3);
        if !poll_until(4000, || self.messages.contains(301) && !self.messages.is_loading()).await {
            probe_fail("voice recovery fixture"); return false;
        }
        let _ = self.scroll_into_view(301).await;
        self.messages.play_media(301, broken.clone(), player::OpenIntent::Manual);
        if !poll_until(3000, || self.messages.player_state(301) == player::PlayerState::Error && player::retry_available(301)).await {
            probe_fail("voice decode failure cannot retry"); return false;
        }
        // Drain the bus error's deferred teardown before the user's next click.
        glib::timeout_future(Duration::from_millis(100)).await;
        self.clone().media_action(301, player::OpenIntent::Manual);
        if !poll_until(5000, || self.messages.player_state(301) == player::PlayerState::Playing).await {
            probe_fail("voice retry did not resume playback"); return false;
        }
        if !poll_until(2000, || player::position_of(301) > 0.1).await {
            probe_fail("voice clock did not advance"); return false;
        }
        self.messages.reset_players();
        let _ = std::fs::remove_file(broken);

        probe_step("late voice download cannot interrupt newer music");
        self.clone().open_chat(8);
        if !poll_until(4000, || self.messages.contains(800) && self.messages.contains(801) && !self.messages.is_loading()).await {
            probe_fail("playback ordering fixture"); return false;
        }
        let Ok(Some(voice_path)) = self.tg.download_media(8, 800).await else {
            probe_fail("playback ordering voice file"); return false;
        };
        self.messages.reset_media(800);
        let Some((_, generation)) = self.messages.begin_media(800) else {
            probe_fail("queue old voice request"); return false;
        };
        self.clone().play_when_ready(800, player::OpenIntent::Manual);
        let _ = self.scroll_into_view(801).await;
        self.clone().media_action(801, player::OpenIntent::Manual);
        if !poll_until(4000, || self.messages.player_state(801) == player::PlayerState::Playing).await {
            probe_fail("newer music request did not play"); return false;
        }
        self.messages.finish_media_path(800, generation, voice_path);
        glib::timeout_future(Duration::from_millis(200)).await;
        if self.messages.player_state(801) != player::PlayerState::Playing
            || self.messages.player_state(800) == player::PlayerState::Playing
            || !player::play_control_enabled(800) {
            probe_fail("late voice stole playback or became unplayable"); return false;
        }
        self.messages.reset_players();

        probe_step("notification photos use the private user and group identity");
        for (chat_id, sender_id) in [(1, 1), (4, 4001)] {
            let before = self.probe_notifications.get();
            self.notify(&Msg { chat_id, sender_id: Some(sender_id), sender: "Fixture sender".into(),
                chat_title: "Fixture chat".into(), text: "Notification photo check".into(), ..Msg::default() });
            if !poll_until(3000, || self.probe_notifications.get() == before + 1).await {
                probe_fail("avatar notification not emitted"); return false;
            }
            let expected = self.tg.download_avatar(chat_id).await.ok().flatten();
            if self.probe_notification_avatar.borrow().as_ref().is_none_or(|(id, path)| *id != chat_id || Some(path) != expected.as_ref()) {
                probe_fail("notification used the wrong avatar"); return false;
            }
        }
        probe_step("opening a chat cancels its pending avatar notification");
        let before = self.probe_notifications.get();
        self.notify(&Msg { chat_id: 4, sender_id: Some(4001), text: "Already read".into(), ..Msg::default() });
        self.clone().open_chat(4);
        glib::timeout_future(Duration::from_millis(1000)).await;
        if self.probe_notifications.get() != before { probe_fail("late notification after chat activation"); return false; }
        self.close_info_panel();
        true
    }

    async fn probe_capture(&self, name: &str) {
        let Some(directory) = std::env::var_os("OMG_PROBE_ARTIFACTS") else { return };
        let directory = PathBuf::from(directory);
        if std::fs::create_dir_all(&directory).is_err() { probe_fail("create probe artifact directory"); return; }
        for _ in 0..5 {
            wait_for_frame(self.widget.upcast_ref()).await;
            let Some(window) = self.window() else { return };
            let Some(renderer) = window.renderer() else { return };
            let paintable = gtk::WidgetPaintable::new(Some(&self.widget));
            let snapshot = gtk::Snapshot::new();
            let (width, height) = (self.widget.width() as f64, self.widget.height() as f64);
            paintable.snapshot(&snapshot, width, height);
            if let Some(node) = snapshot.to_node() {
                let texture = renderer.render_texture(&node, Some(&gtk::graphene::Rect::new(0.0, 0.0, width as f32, height as f32)));
                if texture.save_to_png(directory.join(format!("{name}.png"))).is_ok() { return; }
            }
            glib::timeout_future(Duration::from_millis(100)).await;
        }
        probe_fail("capture media profile screenshot");
    }

    async fn run_wave5d_probe(
        self: &Rc<Self>,
        group: i64,
        marta: i64,
        deni: i64,
        mom: i64,
    ) -> bool {
        if self
            .probe_restored_ui_state
            .is_some_and(|restored| restored.info_panel_open)
        {
            probe_step("info desired state restored and bound");
            let Some(current) = self.open_chat.get() else {
                probe_fail("restored info selected chat");
                return false;
            };
            let expected_width = self
                .probe_restored_ui_state
                .map(|restored| restored.info_width)
                .unwrap_or(320);
            if !poll_until(1_500, || {
                self.info.is_bound(current)
                    && match self.info.layout() {
                        InfoLayout::Column => {
                            (self.content_paned.width() - self.content_paned.position()
                                - expected_width)
                                .abs()
                                <= 1
                        }
                        // The requested width includes the sheet's two borders;
                        // Widget::width() only reports its inner content width.
                        InfoLayout::Overlay => {
                            self.info.widget.parent().is_some_and(|parent| {
                                parent == self.overlay.clone().upcast::<gtk::Widget>()
                            }) && self.info.widget.compute_bounds(&self.overlay).is_some_and(|bounds| {
                                (bounds.width() - expected_width as f32).abs() <= 1.0
                                    && (bounds.x() + bounds.width() - self.overlay.width() as f32).abs() <= 1.0
                            })
                        }
                        InfoLayout::Hidden => false,
                    }
            })
            .await
            {
                eprintln!(
                    "probe info restore: bound={} layout={:?} content_width={} expected_outer_width={} visible={} desired={}",
                    self.info.is_bound(current), self.info.layout(), self.info.widget.width(),
                    expected_width, self.info.widget.is_visible(), self.ui_state.borrow().info_panel_open
                );
                probe_fail("restored info binding");
                return false;
            }
            // A27/A36: a restored-open panel is bound AND on screen, not just
            // flagged open in the state file.
            if !self.ui_state.borrow().info_panel_open || !self.info.widget.is_visible() {
                probe_fail("restored info panel visible");
                return false;
            }
        }

        probe_step("info panel Arch Linux ARM");
        self.clone().open_chat(group);
        if !self.ui_state.borrow().info_panel_open {
            self.toggle_info_panel();
        }
        if !poll_until(1_000, || self.info.is_bound(group)).await {
            probe_fail("info panel bind group");
            return false;
        }

        if environment_listed("OMG_MOCK_SLOW", "GetMembers")
            || environment_listed("OMG_MOCK_SLOW", "GetSharedMedia")
        {
            probe_step("info stale chat switch");
            self.clone().open_chat(marta);
            glib::timeout_future(Duration::from_millis(1_800)).await;
            if !self.info.is_bound(marta) || self.info.members_count() != 0 {
                probe_fail("info stale group fill");
                return false;
            }
            self.clone().open_chat(group);
            if !poll_until(1_000, || self.info.is_bound(group)).await {
                probe_fail("info rebind group");
                return false;
            }
        }

        if !poll_until(4_000, || {
            (self.info.members_count() == 42 || self.info.members_retry_visible())
                && (self.info.shared_retry_visible()
                    || !self.info.shared_state_text().starts_with("Loading"))
        })
        .await
        {
            probe_fail("info section settlement");
            return false;
        }
        if self.info.members_retry_visible() {
            probe_step("members error retry");
            if !self
                .info
                .members_state_text()
                .contains("mock: transient failure")
            {
                probe_fail("members section error");
                return false;
            }
            self.info.trigger_members_retry();
        }
        if self.info.shared_retry_visible() {
            probe_step("shared media error retry");
            if !self
                .info
                .shared_state_text()
                .contains("mock: transient failure")
            {
                probe_fail("shared media section error");
                return false;
            }
            self.info.trigger_shared_retry();
        }
        if !poll_until(4_000, || {
            self.info.members_count() == 42
                && !self.info.members_retry_visible()
                && !self.info.shared_retry_visible()
                && !self.info.shared_state_text().starts_with("Loading")
        })
        .await
        {
            probe_fail("info members/shared retry success");
            return false;
        }

        let Some(window) = self.window() else {
            probe_fail("info resize window");
            return false;
        };
        let dock_at = {
            let state = self.ui_state.borrow();
            state.sidebar_width + state.info_width.max(280) + 560
        };
        probe_step("info boundary preserves 560px conversation");
        self.resize_window_for_probe(&window, dock_at - 1).await;
        if !poll_until(1_000, || self.info.layout() == InfoLayout::Overlay).await {
            probe_fail("info overlay at 1199");
            return false;
        }
        probe_step("info boundary 1200 column");
        self.resize_window_for_probe(&window, dock_at).await;
        if !poll_until(1_000, || self.info.layout() == InfoLayout::Column).await {
            probe_fail("info column at 1200");
            return false;
        }

        // A27 stress: several overlay⇄column reparents inside a single
        // main-loop turn, with no frame in between.
        probe_step("info layout flip 1199/1200");
        for width in [dock_at - 1, dock_at, dock_at - 1, dock_at, dock_at - 1] {
            self.probe_window_width.set(Some(width));
            self.apply_sidebar_layout(width);
        }
        if self.info.layout() != InfoLayout::Overlay {
            probe_fail("info flip settled on overlay");
            return false;
        }
        if !poll_until(1_000, || self.info.widget.is_visible()).await {
            probe_fail("info flip left the panel unmapped");
            return false;
        }
        self.resize_window_for_probe(&window, 1280).await;
        if !poll_until(1_000, || {
            self.info.layout() == InfoLayout::Column && self.info.is_bound(group)
        })
        .await
        {
            probe_fail("info flip restores the column");
            return false;
        }

        if environment_listed("OMG_MOCK_SLOW", "GetSharedMedia") {
            probe_step("shared media stale tab switch");
            self.info.probe_select_shared(SharedKind::Files);
            self.info.probe_select_shared(SharedKind::Links);
            self.info.probe_select_shared(SharedKind::Voice);
            if !poll_until(4_000, || {
                self.info.shared_kind() == SharedKind::Voice
                    && !self.info.shared_state_text().starts_with("Loading")
            })
            .await
            {
                probe_fail("shared media stale tab result");
                return false;
            }
        }
        for (label, kind) in [
            ("Files", SharedKind::Files),
            ("Links", SharedKind::Links),
            ("Voice", SharedKind::Voice),
        ] {
            probe_step(&format!("shared media {label}"));
            self.info.probe_select_shared(kind);
            if !poll_until(4_000, || {
                self.info.shared_kind() == kind
                    && (self.info.shared_retry_visible()
                        || !self.info.shared_state_text().starts_with("Loading"))
            })
            .await
            {
                probe_fail("shared media tab");
                return false;
            }
            if self.info.shared_retry_visible() {
                self.info.trigger_shared_retry();
                if !poll_until(4_000, || {
                    !self.info.shared_retry_visible()
                        && !self.info.shared_state_text().starts_with("Loading")
                })
                .await
                {
                    probe_fail("shared media tab retry");
                    return false;
                }
            }
        }

        probe_step("info notifications switch");
        self.info.probe_toggle_notifications(false);
        if !poll_until(1_000, || !self.info.notifications_enabled()).await {
            probe_fail("info notifications mute");
            return false;
        }
        self.info.probe_toggle_notifications(true);
        if !poll_until(1_000, || self.info.notifications_enabled()).await {
            probe_fail("info notifications unmute");
            return false;
        }

        self.clone().open_chat(marta);
        if !poll_until(4_000, || {
            self.open_chat.get() == Some(marta) && !self.messages.is_loading()
        })
        .await
        {
            probe_fail("stickers open chat");
            return false;
        }
        probe_step("sticker popover");
        self.open_stickers();
        if !poll_until(4_000, || {
            self.stickers.stickers_ready() || self.stickers.retry_visible()
        })
        .await
        {
            probe_fail("sticker packs settlement");
            return false;
        }
        if self.stickers.retry_visible() {
            probe_step("sticker packs error retry");
            if !self
                .stickers
                .error_text()
                .contains("mock: transient failure")
            {
                probe_fail("sticker packs section error");
                return false;
            }
            self.stickers.trigger_retry();
            if !poll_until(4_000, || self.stickers.stickers_ready()).await {
                probe_fail("sticker packs retry");
                return false;
            }
        }

        let Some(omarchy_pack) = self.stickers.probe_pack_id_by_title("Omarchy") else {
            probe_fail("Omarchy sticker pack listed");
            return false;
        };
        if environment_listed("OMG_MOCK_SLOW", "GetStickers") {
            probe_step("sticker stale pack switch");
            self.stickers.probe_select_pack(&omarchy_pack);
            self.stickers.probe_select_pack("recent");
            if !poll_until(4_000, || {
                self.stickers.current_pack() == "recent" && self.stickers.stickers_ready()
            })
            .await
            {
                probe_fail("sticker stale pack result");
                return false;
            }
        }
        self.stickers.probe_select_pack(&omarchy_pack);
        if !poll_until(4_000, || {
            self.stickers.current_pack() == omarchy_pack && self.stickers.stickers_ready()
        })
        .await
        {
            probe_fail("Omarchy sticker pack");
            return false;
        }
        // Wave 6D: animated .tgs stickers ARE sendable now (the mock accepts
        // send_sticker(9104) and the cell renders through the Lottie thread).
        if !self.stickers.sticker_sendable(9104) {
            probe_fail("animated sticker sendability");
            return false;
        }
        if environment_listed("OMG_MOCK_SLOW", "DownloadSticker") {
            probe_step("sticker stale image fill");
            if !poll_until(4_000, || self.stickers.downloaded_ids().contains(&9101)).await {
                probe_fail("sticker image download");
                return false;
            }
            if self
                .stickers
                .downloaded_ids()
                .iter()
                .any(|id| (9000..9100).contains(id))
            {
                probe_fail("sticker stale image applied");
                return false;
            }
        }
        probe_step("GIF cards settle");
        // Since wave 6 the mock renders an mp4 fixture with ffmpeg: both cards
        // must either download (ffmpeg present) or say they are unavailable
        // (OMG_MOCK_NO_FFMPEG=1) — never look like they are still loading (A33).
        self.stickers.probe_select_pack("gifs");
        if !poll_until(8_000, || {
            self.stickers.current_pack() == "gifs"
                && (self.stickers.unavailable_ids() == vec![9201, 9202]
                    || self.stickers.downloaded_ids() == vec![9201, 9202])
        })
        .await
        {
            probe_fail("GIF cards settle");
            return false;
        }
        self.stickers.probe_select_pack("recent");
        if !poll_until(4_000, || self.stickers.sticker_sendable(9002)).await {
            probe_fail("recent sticker 9002");
            return false;
        }
        probe_step("send sticker 9002");
        if !self.stickers.probe_send_sticker(9002) {
            probe_fail("sticker send action");
            return false;
        }
        if !poll_until(4_000, || {
            self.stickers.retry_visible()
                || self.messages.last_message().is_some_and(|message| {
                    message.outgoing
                        && message.media == Some(MediaKind::Sticker)
                        && message.sticker_emoji.as_deref() == Some("👍")
                })
        })
        .await
        {
            probe_fail("sticker send settlement");
            return false;
        }
        if self.stickers.retry_visible() {
            probe_step("sticker send error retry");
            if !self
                .stickers
                .error_text()
                .contains("mock: transient failure")
            {
                probe_fail("sticker send error");
                return false;
            }
            self.stickers.trigger_retry();
        }
        if !poll_until(4_000, || {
            self.messages.last_message().is_some_and(|message| {
                message.outgoing
                    && message.media == Some(MediaKind::Sticker)
                    && message.sticker_emoji.as_deref() == Some("👍")
            })
        })
        .await
        {
            probe_fail("sticker retry success");
            return false;
        }
        self.close_stickers();

        // A8: Cancel while `record_start` is still in flight. The bar and the
        // C5 lock must survive until the start resolves, and nothing may be
        // sent. The delay hook makes the Starting window deterministic.
        probe_step("recording cancel during start");
        let voice_before = self.messages.last_message().map(|message| message.id);
        self.probe_record_start_delay.set(500);
        self.start_recording();
        if !poll_until(1_500, || {
            self.messages.recorder_status() == "Starting microphone…"
        })
        .await
        {
            self.probe_record_start_delay.set(0);
            probe_fail("recording starting state");
            return false;
        }
        self.messages.probe_recorder_cancel();
        if self.messages.recorder_status() != "Cancelling…"
            || !self.messages.recorder_visible()
            || !self.composer_operation.get()
        {
            self.probe_record_start_delay.set(0);
            probe_fail("deferred cancel holds the bar and the composer lock");
            return false;
        }
        if !poll_until(4_000, || {
            !self.messages.recorder_visible() && !self.composer_operation.get()
        })
        .await
        {
            self.probe_record_start_delay.set(0);
            probe_fail("deferred cancel settlement");
            return false;
        }
        self.probe_record_start_delay.set(0);
        if self.messages.last_message().map(|message| message.id) != voice_before {
            probe_fail("deferred cancel sent a message");
            return false;
        }

        probe_step("recording start cancel");
        self.start_recording();
        if !poll_until(4_000, || self.messages.recorder_status() == "Recording").await {
            probe_fail("recording start");
            return false;
        }
        self.messages.probe_recorder_cancel();
        // The composer stays locked until the backend recording slot is
        // released, so the next start cannot race the cancel.
        if !poll_until(2_000, || {
            !self.messages.recorder_visible() && !self.composer_operation.get()
        })
        .await
        {
            probe_fail("recording cancel");
            return false;
        }

        probe_step("recording stop send");
        self.start_recording();
        if !poll_until(4_000, || self.messages.recorder_status() == "Recording").await {
            probe_fail("recording restart");
            return false;
        }
        self.messages.probe_recorder_send();
        if !poll_until(4_000, || {
            self.messages
                .recorder_status()
                .contains("mock: transient failure")
                || self.messages.last_message().is_some_and(|message| {
                    message.outgoing
                        && message.media == Some(MediaKind::Voice)
                        && message.duration == Some(1)
                })
        })
        .await
        {
            probe_fail("voice send settlement");
            return false;
        }
        if self
            .messages
            .recorder_status()
            .contains("mock: transient failure")
        {
            probe_step("voice send error retry");
            self.messages.probe_recorder_retry();
        }
        if !poll_until(4_000, || {
            self.messages.last_message().is_some_and(|message| {
                message.outgoing
                    && message.media == Some(MediaKind::Voice)
                    && message.duration == Some(1)
            })
        })
        .await
        {
            probe_fail("voice retry success");
            return false;
        }

        if environment_listed("OMG_MOCK_SLOW", "GetContacts") {
            probe_step("contacts stale close");
            self.open_contacts();
            glib::timeout_future(Duration::from_millis(80)).await;
            self.close_contacts();
            glib::timeout_future(Duration::from_millis(1_700)).await;
            if self.contacts.is_open() || self.contacts.count() != 0 {
                probe_fail("contacts stale fill");
                return false;
            }
        }
        probe_step("contacts Sam Rivera");
        self.open_contacts();
        if !poll_until(4_000, || self.contacts.count() == 6 || self.contacts.retry_visible()).await {
            probe_fail("contacts settlement");
            return false;
        }
        if self.contacts.retry_visible() {
            probe_step("contacts error retry");
            if !self.contacts.error_text().contains("mock: transient failure") {
                probe_fail("contacts section error");
                return false;
            }
            self.contacts.trigger_retry();
            if !poll_until(4_000, || self.contacts.count() == 6).await {
                probe_fail("contacts retry");
                return false;
            }
        }
        if !self.contacts.probe_open("Sam Rivera")
            || !poll_until(4_000, || {
                self.messages.header_title() == "Sam Rivera" && !self.messages.is_loading()
            })
            .await
        {
            probe_fail("contacts open Sam Rivera");
            return false;
        }

        probe_step("new group Test");
        self.open_new_group();
        if !poll_until(4_000, || {
            self.new_group.contact_count() == 6 || self.new_group.retry_visible()
        })
        .await
        {
            probe_fail("new group contacts settlement");
            return false;
        }
        if self.new_group.retry_visible() {
            self.new_group.trigger_retry();
            if !poll_until(4_000, || self.new_group.contact_count() == 6).await {
                probe_fail("new group contacts retry");
                return false;
            }
        }
        self.new_group.probe_set_title("Test");
        if !self.new_group.probe_select("Sam Rivera") || !self.new_group.probe_select("Mom") {
            probe_fail("new group member selection");
            return false;
        }
        self.new_group.probe_submit();
        if !poll_until(4_000, || {
            !self.new_group.is_open()
                || self
                    .new_group
                    .error_text()
                    .contains("mock: transient failure")
        })
        .await
        {
            probe_fail("new group create settlement");
            return false;
        }
        if self.new_group.is_open() {
            probe_step("new group create error retry");
            if self.new_group.title_text() != "Test" || self.new_group.selected_count() != 2 {
                probe_fail("new group retained input");
                return false;
            }
            self.new_group.probe_submit();
        }
        if !poll_until(4_000, || {
            !self.new_group.is_open()
                && self.messages.header_title() == "Test"
                && !self.messages.is_loading()
        })
        .await
        {
            probe_fail("new group retry success");
            return false;
        }
        let Some(test_chat) = self.open_chat.get() else {
            probe_fail("new group selected chat");
            return false;
        };

        self.messages.set_composer_text("selection one");
        self.clone().submit_composer();
        if !poll_until(3_000, || {
            self.messages.find_outgoing_text("selection one").is_some()
        })
        .await
        {
            probe_fail("first selection message");
            return false;
        }
        self.messages.set_composer_text("selection two");
        self.clone().submit_composer();
        if !poll_until(3_000, || {
            self.messages.find_outgoing_text("selection two").is_some()
        })
        .await
        {
            probe_fail("second selection message");
            return false;
        }
        let first = self
            .messages
            .find_outgoing_text("selection one")
            .unwrap_or_default();
        let second = self
            .messages
            .find_outgoing_text("selection two")
            .unwrap_or_default();
        probe_step("multi-select copy forward cancel");
        if !self.messages.begin_selection(first) {
            probe_fail("selection mode start");
            return false;
        }
        self.messages.set_selected(second, true);
        if self.messages.selection_ids() != vec![first, second] {
            probe_fail("selection display order");
            return false;
        }
        self.clone()
            .handle_message_action(MessageAction::SelectionCopy);
        if self.probe_copied.borrow().as_str() != "selection one\nselection two" {
            probe_fail("selection copy text");
            return false;
        }
        self.clone()
            .handle_message_action(MessageAction::SelectionForward);
        if !poll_until(1_000, || self.forward.is_open()).await
            || !self.forward.probe_select("Mom")
        {
            probe_fail("selection forward Mom target");
            return false;
        }
        // Closing the picker must not forward anything — checked below, once
        // the submitted forward has put us in Mom.
        self.close_forward();
        self.clone()
            .handle_message_action(MessageAction::SelectionCancel);
        if self.messages.selection_mode() {
            probe_fail("selection cancel");
            return false;
        }

        probe_step("multi-select forward submit");
        if !self.messages.begin_selection(first) {
            probe_fail("forward selection start");
            return false;
        }
        self.messages.set_selected(second, true);
        self.clone()
            .handle_message_action(MessageAction::SelectionForward);
        if !poll_until(1_000, || self.forward.is_open()).await
            || !self.forward.probe_select("Mom")
        {
            probe_fail("forward submit Mom target");
            return false;
        }
        self.forward.probe_submit();
        if environment_listed("OMG_MOCK_FAIL_ONCE", "ForwardMessages") {
            // The one-shot failure may already have been consumed by the
            // earlier 5C forward step; accept either a transient error
            // (then retry) or a direct success.
            let outcome = poll_until(3_800, || {
                !self.forward.is_open()
                    || self
                        .forward
                        .probe_status()
                        .contains("mock: transient failure")
            })
            .await;
            if !outcome {
                probe_fail("selection forward transient error");
                return false;
            }
            if self.forward.is_open() {
                self.forward.probe_submit();
            }
        }
        let forwarded_copies = |text: &str| {
            self.messages
                .messages()
                .into_iter()
                .filter(|message| message.text == text && message.forwarded_from.is_some())
                .count()
        };
        if !poll_until(5_000, || {
            !self.forward.is_open()
                && self.open_chat.get() == Some(mom)
                && !self.messages.is_loading()
                && forwarded_copies("selection one") > 0
                && forwarded_copies("selection two") > 0
        })
        .await
        {
            probe_fail("selection forward delivered both messages");
            return false;
        }
        // Exactly one copy each: the picker that was closed without
        // submitting forwarded nothing.
        if forwarded_copies("selection one") != 1 || forwarded_copies("selection two") != 1 {
            probe_fail("cancelled forward picker sent messages");
            return false;
        }
        self.clone().open_chat(test_chat);
        if !poll_until(4_000, || {
            self.open_chat.get() == Some(test_chat)
                && !self.messages.is_loading()
                && self.messages.find_outgoing_text("selection one").is_some()
        })
        .await
        {
            probe_fail("restore group after forward submit");
            return false;
        }
        let first = self
            .messages
            .find_outgoing_text("selection one")
            .unwrap_or_default();

        probe_step("selection delete event bookkeeping");
        self.messages.set_composer_text("delete");
        self.clone().submit_composer();
        let trigger = if poll_until(3_000, || {
            self.messages.find_outgoing_text("delete").is_some()
        })
        .await
        {
            self.messages.find_outgoing_text("delete").unwrap_or_default()
        } else {
            probe_fail("delete trigger send");
            return false;
        };
        let reply = if poll_until(3_500, || {
            self.messages
                .find_incoming_text_after("(mock reply) got it", trigger)
                .is_some()
        })
        .await
        {
            self.messages
                .find_incoming_text_after("(mock reply) got it", trigger)
                .unwrap_or_default()
        } else {
            probe_fail("delete trigger reply");
            return false;
        };
        if !self.messages.begin_selection(first) {
            probe_fail("delete selection start");
            return false;
        }
        self.messages.set_selected(reply, true);
        if self.messages.selection_count() != 2 {
            probe_fail("delete selection initial count");
            return false;
        }
        if !poll_until(4_000, || {
            self.messages.selection_count() == 1
                && !self.messages.selection_ids().contains(&reply)
        })
        .await
        {
            probe_fail("deleted id left selection");
            return false;
        }
        self.messages.exit_selection_mode();

        probe_step("muted incoming suppresses notification");
        if self
            .chatlist
            .summary(deni)
            .is_none_or(|summary| !summary.muted)
        {
            probe_fail("muted chat fixture");
            return false;
        }
        self.clone().open_chat(deni);
        if !poll_until(3_500, || {
            self.open_chat.get() == Some(deni) && !self.messages.is_loading()
        })
        .await
        {
            probe_fail("open muted chat");
            return false;
        }
        self.messages.set_composer_text("mute notification probe");
        self.clone().submit_composer();
        if !poll_until(3_000, || {
            self.messages
                .find_outgoing_text("mute notification probe")
                .is_some()
        })
        .await
        {
            probe_fail("muted reply trigger");
            return false;
        }
        self.clone().open_chat(marta);
        let notifications_before = self.probe_notifications.get();
        glib::timeout_future(Duration::from_millis(2_800)).await;
        if self.probe_notifications.get() != notifications_before {
            probe_fail("muted chat notification emitted");
            return false;
        }

        probe_step("Saved messages action");
        self.clone().open_saved_messages();
        if !poll_until(3_500, || {
            self.messages.header_title() == "Saved Messages" && !self.messages.is_loading()
        })
        .await
        {
            probe_fail("Saved messages open");
            return false;
        }
        self.clone().open_chat(marta);
        if !poll_until(3_500, || {
            self.open_chat.get() == Some(marta) && !self.messages.is_loading()
        })
        .await
        {
            probe_fail("restore Marta after regular-use probe");
            return false;
        }
        if self.info.chat_id() != Some(marta) {
            probe_fail("info panel chat-switch rebind");
            return false;
        }
        if test_chat == marta || test_chat == group || test_chat == deni || test_chat == mom {
            probe_fail("new group identity");
            return false;
        }

        // ---- Package 6B: location / venue / contact / dice / polls ----
        let open_by_title = |this: &Rc<Self>, title: &str| -> Option<i64> {
            this.chatlist
                .ordered()
                .into_iter()
                .find_map(|(id, t)| (t == title).then_some(id))
        };

        probe_step("cards open Media Lab");
        let Some(media_lab) = open_by_title(self, "Media Lab") else {
            probe_fail("Media Lab fixture missing");
            return false;
        };
        self.clone().open_chat(media_lab);
        if !poll_until(
            3_500,
            || self.open_chat.get() == Some(media_lab) && !self.messages.is_loading(),
        )
        .await
        {
            probe_fail("cards open Media Lab");
            return false;
        }

        probe_step("card location");
        // The asciiload effect is opt-in (off by default in smoke), so the
        // placeholder path is proven by the row having a media state at all;
        // the effect source is only checked when the effect is enabled.
        if self.messages.media_state(805).is_none()
            || (self.settings.get().animation("asciiload")
                && !matches!(self.messages.media_state(805), Some(MediaState::Done(_)))
                && !self.messages.geo_loading_effect_active(805))
        {
            probe_fail("card location loading effect");
            return false;
        }
        if !poll_until(
            8_000,
            || {
                self.messages.geo_map_loaded(805)
                    && self
                        .messages
                        .card_text(805)
                        .map(|t| t.contains("13.40500"))
                        .unwrap_or(false)
            },
        )
        .await
        {
            probe_fail("card location");
            return false;
        }

        probe_step("card venue");
        if !poll_until(
            3_500,
            || {
                self.messages
                    .card_text(806)
                    .map(|t| t.contains("Café Einstein") && t.contains("Kurfürstenstraße"))
                    .unwrap_or(false)
            },
        )
        .await
        {
            probe_fail("card venue");
            return false;
        }

        probe_step("card live location");
        if !poll_until(
            3_500,
            || {
                self.messages
                    .card_text(807)
                    .map(|t| t.contains("Live") || t.contains("Sharing ended"))
                    .unwrap_or(false)
            },
        )
        .await
        {
            probe_fail("card live location");
            return false;
        }
        if !poll_until(3_500, || self.messages.geo_map_loaded(807)).await
            || !self.messages.live_timer_active(807)
        {
            probe_fail("card live location map/timer");
            return false;
        }

        // Move the live point through the backend, then offer the completed
        // row its previous map generation again. It must reject that stale
        // completion and retain the new point's map.
        probe_step("card live location stale map");
        let Some(old_generation) = self.messages.media_generation(807) else {
            probe_fail("card live location old map generation");
            return false;
        };
        let Some(MediaState::Done(old_path)) = self.messages.media_state(807) else {
            probe_fail("card live location old map path");
            return false;
        };
        if self
            .tg
            .update_live_location(
                media_lab,
                807,
                crate::tg::GeoPoint {
                    lat: 52.5185,
                    lon: 13.3777,
                },
            )
            .await
            .is_err()
        {
            probe_fail("card live location backend move");
            return false;
        }
        if !poll_until(4_000, || {
            matches!(
                self.messages.media_state(807),
                Some(MediaState::Done(path))
                    if path.file_name().is_some_and(|name| name.to_string_lossy().contains("52.5185_13.3777"))
            )
        })
        .await
        {
            probe_fail("card live location latest map");
            return false;
        }
        let Ok(old_texture) = gdk::Texture::from_filename(&old_path) else {
            probe_fail("card live location old map decode");
            return false;
        };
        if self
            .messages
            .finish_image(807, old_generation, old_path, &old_texture)
            || !matches!(
                self.messages.media_state(807),
                Some(MediaState::Done(path))
                    if path.file_name().is_some_and(|name| name.to_string_lossy().contains("52.5185_13.3777"))
            )
        {
            probe_fail("card live location stale map completion");
            return false;
        }

        // A point update while map tiles are disabled must rebuild the text
        // card without resetting media or issuing a replacement download.
        let map_generation = self.messages.media_generation(807);
        self.messages.set_map_tiles(false);
        if let Some(mut message) = self.messages.message(807) {
            if let Some(location) = &mut message.location {
                location.point.lat += 0.001;
            }
            self.messages.merge_event(message);
        }
        if self.messages.media_generation(807) != map_generation
            || self.messages.geo_map_loaded(807)
        {
            probe_fail("card live location map disabled download");
            return false;
        }
        self.messages.set_map_tiles(true);

        // Stopping at the same point updates the metadata widgets in place,
        // cancels their dedicated timer, and never leaves a loading map slot.
        let live_card = self.messages.card_widget(807);
        self.messages.expire_live_location(807);
        if self.messages.card_widget(807) != live_card
            || self.messages.live_timer_active(807)
            || !self
                .messages
                .card_text(807)
                .is_some_and(|text| text.contains("Sharing ended"))
        {
            probe_fail("card live location metadata update");
            return false;
        }
        let expired_generation = self.messages.media_generation(807);
        if let Some(mut message) = self.messages.message(807) {
            if let Some(location) = &mut message.location {
                location.point.lon += 0.001;
            }
            self.messages.merge_event(message);
        }
        if self.messages.media_generation(807) != expired_generation
            || self.messages.geo_map_loaded(807)
            || self.messages.live_timer_active(807)
        {
            probe_fail("card expired live location download");
            return false;
        }

        probe_step("card contact");
        if !poll_until(
            3_500,
            || {
                self.messages
                    .card_text(808)
                    .map(|t| t.contains("Marta") && t.contains("Open chat"))
                    .unwrap_or(false)
            },
        )
        .await
        {
            probe_fail("card contact");
            return false;
        }

        probe_step("card contact retry");
        let old_fail_once = std::env::var_os("OMG_MOCK_FAIL_ONCE");
        let restore_fail_once = || {
            if let Some(value) = &old_fail_once {
                unsafe { std::env::set_var("OMG_MOCK_FAIL_ONCE", value) };
            } else {
                unsafe { std::env::remove_var("OMG_MOCK_FAIL_ONCE") };
            }
        };
        let mut fail_once = old_fail_once
            .as_ref()
            .map(|value| value.to_string_lossy().into_owned())
            .unwrap_or_default();
        if !fail_once.is_empty() {
            fail_once.push(',');
        }
        fail_once.push_str("AddContact");
        unsafe {
            std::env::set_var("OMG_MOCK_FAIL_ONCE", fail_once);
        }
        let Some(add_contact) = self.messages.contact_add_button(808) else {
            restore_fail_once();
            probe_fail("card contact add button");
            return false;
        };
        add_contact.emit_clicked();
        if add_contact.is_sensitive() {
            restore_fail_once();
            probe_fail("card contact pending state");
            return false;
        }
        if !poll_until(4_000, || {
            self.messages.contact_error_visible(808)
                && self
                    .messages
                    .contact_add_button(808)
                    .is_some_and(|button| button.is_sensitive())
        })
        .await
        {
            restore_fail_once();
            probe_fail("card contact inline retry state");
            return false;
        }
        restore_fail_once();
        self.messages.contact_add_button(808).unwrap().emit_clicked();
        if !poll_until(4_000, || {
            self.messages
                .card_text(808)
                .is_some_and(|text| text.contains("Added"))
        })
        .await
        {
            probe_fail("card contact retry completion");
            return false;
        }

        probe_step("card contact unknown");
        if !poll_until(
            3_500,
            || {
                self.messages
                    .card_text(809)
                    .map(|t| t.contains("Unknown Caller") && !t.contains("Open chat"))
                    .unwrap_or(false)
            },
        )
        .await
        {
            probe_fail("card contact unknown");
            return false;
        }
        if let Some(mut same_contact) = self.messages.message(808) {
            same_contact.id = 814;
            self.messages.merge_event(same_contact);
        }
        if !self
            .messages
            .card_text(814)
            .is_some_and(|text| text.contains("Added"))
        {
            probe_fail("card contact user-scoped added state");
            return false;
        }

        probe_step("card dice");
        if !poll_until(
            3_500,
            || self.messages.card_text(810).map(|t| t.contains("Rolled 4")).unwrap_or(false),
        )
        .await
        {
            probe_fail("card dice");
            return false;
        }

        probe_step("card dice rolling");
        if !poll_until(
            3_500,
            || self.messages.card_text(812).map(|t| t.contains("Rolling")).unwrap_or(false),
        )
        .await
        {
            probe_fail("card dice rolling");
            return false;
        }

        probe_step("cards open Polls");
        let Some(polls) = open_by_title(self, "Polls") else {
            probe_fail("Polls fixture missing");
            return false;
        };
        self.clone().open_chat(polls);
        if !poll_until(
            3_500,
            || self.open_chat.get() == Some(polls) && !self.messages.is_loading(),
        )
        .await
        {
            probe_fail("cards open Polls");
            return false;
        }

        // A delayed completion from Polls must not mutate a colliding message
        // id in another chat.
        probe_step("poll stale completion");
        let Some(mut colliding_poll) = self.messages.message(904) else {
            probe_fail("poll stale completion fixture");
            return false;
        };
        let old_slow = std::env::var_os("OMG_MOCK_SLOW");
        let restore_slow = || {
            if let Some(value) = &old_slow {
                unsafe { std::env::set_var("OMG_MOCK_SLOW", value) };
            } else {
                unsafe { std::env::remove_var("OMG_MOCK_SLOW") };
            }
        };
        let mut slow = old_slow
            .as_ref()
            .map(|value| value.to_string_lossy().into_owned())
            .unwrap_or_default();
        if !slow.is_empty() {
            slow.push(',');
        }
        slow.push_str("SendVote");
        unsafe {
            std::env::set_var("OMG_MOCK_SLOW", slow);
        }
        self.clone().handle_message_action(MessageAction::Vote {
            msg_id: 904,
            options: vec![0],
        });
        self.clone().open_chat(media_lab);
        if !poll_until(3_500, || {
            self.open_chat.get() == Some(media_lab) && !self.messages.is_loading()
        })
        .await
        {
            restore_slow();
            probe_fail("poll stale completion target chat");
            return false;
        }
        colliding_poll.chat_id = media_lab;
        colliding_poll.chat_title = "Media Lab".into();
        self.messages.merge_event(colliding_poll);
        let Some(colliding_widget) = self.messages.card_widget(904) else {
            restore_slow();
            probe_fail("poll stale completion colliding row");
            return false;
        };
        colliding_widget.set_sensitive(false);
        glib::timeout_future(Duration::from_millis(150)).await;
        restore_slow();
        glib::timeout_future(Duration::from_millis(1_700)).await;
        if colliding_widget.is_sensitive() || self.messages.poll_error_visible(904) {
            probe_fail("poll stale completion changed colliding row");
            return false;
        }
        self.clone().open_chat(polls);
        if !poll_until(3_500, || {
            self.open_chat.get() == Some(polls) && !self.messages.is_loading()
        })
        .await
        {
            probe_fail("poll restore after stale completion");
            return false;
        }

        probe_step("poll vote");
        if !self.messages.poll_option_label_in_check(900, 0) {
            probe_fail("poll option label activation");
            return false;
        }
        if let Some(check) = self.messages.poll_option_check(900, 0) {
            check.set_active(true);
        } else {
            probe_fail("poll vote widget missing");
            return false;
        }
        if !poll_until(
            4_000,
            || self.messages.card_text(900).map(|t| t.contains("13 votes")).unwrap_or(false),
        )
        .await
        {
            probe_fail("poll vote");
            return false;
        }
        // Voting again (already voted) must surface the inline error, not crash.
        self.clone()
            .handle_message_action(MessageAction::Vote {
                msg_id: 900,
                options: vec![0],
            });
        if !poll_until(4_000, || self.messages.poll_error_visible(900)).await {
            probe_fail("poll vote twice error");
            return false;
        }
        if !self
            .messages
            .card_widget(900)
            .is_some_and(|widget| widget.is_sensitive())
        {
            probe_fail("poll vote error re-enable");
            return false;
        }

        probe_step("poll retract");
        self.clone()
            .handle_message_action(MessageAction::RetractVote(900));
        if !poll_until(
            4_000,
            || self.messages.card_text(900).map(|t| t.contains("12 votes")).unwrap_or(false),
        )
        .await
        {
            probe_fail("poll retract");
            return false;
        }

        probe_step("poll live update");
        if let Some(check) = self.messages.poll_option_check(900, 0) {
            check.set_active(true);
        } else {
            probe_fail("poll live update widget missing");
            return false;
        }
        if !poll_until(
            4_000,
            || self.messages.card_text(900).map(|t| t.contains("13 votes")).unwrap_or(false),
        )
        .await
        {
            probe_fail("poll live update");
            return false;
        }

        probe_step("poll multiple");
        if !poll_until(
            3_500,
            || {
                self.messages
                    .card_text(901)
                    .map(|t| t.contains("Multiple answers") && t.contains("Vote"))
                    .unwrap_or(false)
            },
        )
        .await
        {
            probe_fail("poll multiple");
            return false;
        }
        // Exercise the real widgets: toggle two options, click the Vote button.
        {
            let _ = self.messages.poll_option_check(901, 0).map(|c| c.set_active(true));
            let _ = self.messages.poll_option_check(901, 1).map(|c| c.set_active(true));
            if let Some(vote) = self.messages.poll_vote_button(901) {
                vote.emit_clicked();
            }
        }
        if !poll_until(
            4_000,
            || self.messages.card_text(901).map(|t| t.contains("9 votes")).unwrap_or(false),
        )
        .await
        {
            probe_fail("poll multiple vote");
            return false;
        }

        probe_step("poll quiz");
        if let Some(check) = self.messages.poll_option_check(902, 1) {
            check.set_active(true);
        } else {
            probe_fail("poll quiz widget missing");
            return false;
        }
        if !poll_until(
            4_000,
            || {
                self.messages
                    .card_text(902)
                    .map(|t| t.contains("It rotates through the installed themes"))
                    .unwrap_or(false)
            },
        )
        .await
        {
            probe_fail("poll quiz");
            return false;
        }

        probe_step("poll closed");
        if !poll_until(
            3_500,
            || {
                self.messages
                    .card_text(903)
                    .map(|t| t.contains("Closed") && !t.contains("Vote"))
                    .unwrap_or(false)
            },
        )
        .await
        {
            probe_fail("poll closed");
            return false;
        }

        // Leave Marta open for the subsequent sidebar/pagination probe steps,
        // which assume the regular-use chat is still selected.
        if let Some(marta) = open_by_title(self, "Marta") {
            self.clone().open_chat(marta);
            poll_until(
                3_500,
                || self.open_chat.get() == Some(marta) && !self.messages.is_loading(),
            )
            .await;
        }

        true
    }

    /// Wave 6D: animated (.tgs) stickers on the "Media Lab" fixture and in
    /// the sticker picker (specs/spec-wave6.md §1.11).
    async fn run_wave6d_probe(self: &Rc<Self>) -> bool {
        let media_lab = self
            .chatlist
            .ordered()
            .into_iter()
            .find_map(|(id, title)| (title == "Media Lab").then_some(id));
        let Some(media_lab) = media_lab else {
            probe_fail("find Media Lab");
            return false;
        };
        self.clone().open_chat(media_lab);
        if !poll_until(4_000, || {
            self.open_chat.get() == Some(media_lab)
                && !self.messages.is_loading()
                && self.messages.contains(813)
        })
        .await
        {
            probe_fail("open Media Lab for animated stickers");
            return false;
        }

        probe_step("lottie sticker renders");
        // The .tgs row is the newest message, so it is on screen and plays.
        if !poll_until(6_000, || {
            self.messages
                .lottie(813)
                .is_some_and(|sticker| sticker.is_ready())
        })
        .await
        {
            probe_fail("lottie sticker first frame");
            return false;
        }
        let Some(sticker) = self.messages.lottie(813) else {
            probe_fail("lottie sticker widget");
            return false;
        };
        if let Some(error) = sticker.error() {
            probe_fail(&format!("lottie sticker error: {error}"));
            return false;
        }
        if sticker.logical_size() != lottie::BUBBLE_SIZE
            || sticker.raster_size()
                != (lottie::BUBBLE_SIZE * sticker.scale_factor()) as u32
        {
            probe_fail("lottie logical size / DPI raster size");
            return false;
        }
        // The row is the newest message, but a settling layout can leave it
        // below the viewport — an off-screen sticker is paused by design.
        if !self.messages.row_visible(813) {
            self.messages.scroll_to_message(813);
        }
        if !poll_until(3_000, || self.messages.row_visible(813)).await {
            probe_fail("animated sticker row on screen");
            return false;
        }
        let (index, frames) = (sticker.frame_index(), sticker.frames_shown());
        glib::timeout_future(Duration::from_millis(300)).await;
        if !poll_until(3_000, || {
            sticker.is_animating()
                && sticker.frames_shown() > frames
                && sticker.frame_index() != index
        })
        .await
        {
            probe_fail(&format!(
                "lottie sticker frame advance (animating {}, visible {}, slots {}, frames {} -> {})",
                sticker.is_animating(),
                self.messages.row_visible(813),
                lottie::animating_count(),
                frames,
                sticker.frames_shown()
            ));
            return false;
        }

        probe_step("lottie sticker offscreen pause");
        if !self.messages.scroll_to_message(800) {
            probe_fail("scroll away from the animated sticker");
            return false;
        }
        if !poll_until(3_000, || {
            !self.messages.row_visible(813)
                && !sticker.is_animating()
                && sticker.frame_index() == 0
                && lottie::animating_count() == 0
        })
        .await
        {
            probe_fail("lottie sticker offscreen pause");
            return false;
        }
        let paused = sticker.frames_shown();
        glib::timeout_future(Duration::from_millis(300)).await;
        if sticker.frames_shown() != paused {
            probe_fail("off-screen lottie sticker kept rendering");
            return false;
        }
        if !self.messages.scroll_to_message(813) {
            probe_fail("scroll back to the animated sticker");
            return false;
        }
        if !poll_until(3_000, || {
            self.messages.row_visible(813) && sticker.frames_shown() > paused
        })
        .await
        {
            probe_fail("lottie sticker resume on screen");
            return false;
        }

        probe_step("lottie master toggle");
        self.settings
            .update(|settings| settings.media.animated_stickers = false);
        if !poll_until(2_000, || !sticker.is_animating() && sticker.frame_index() == 0).await {
            probe_fail("animated_stickers off pauses playback");
            return false;
        }
        let frozen = sticker.frames_shown();
        glib::timeout_future(Duration::from_millis(300)).await;
        if sticker.frames_shown() != frozen {
            probe_fail("animated_stickers off kept rendering");
            return false;
        }
        self.settings
            .update(|settings| settings.media.animated_stickers = true);
        if !poll_until(3_000, || {
            sticker.is_animating() && sticker.frames_shown() > frozen
        })
        .await
        {
            probe_fail("animated_stickers on resumes playback");
            return false;
        }
        // The animations master switch pauses stickers as well (§1.8).
        if let Some(gtk_settings) = gtk::Settings::default() {
            gtk_settings.set_gtk_enable_animations(false);
            if !poll_until(2_000, || !sticker.is_animating() && sticker.frame_index() == 0).await {
                gtk_settings.set_gtk_enable_animations(true);
                probe_fail("animations master off pauses stickers");
                return false;
            }
            let stopped = sticker.frames_shown();
            glib::timeout_future(Duration::from_millis(300)).await;
            let advanced = sticker.frames_shown() != stopped;
            gtk_settings.set_gtk_enable_animations(true);
            if advanced {
                probe_fail("animations master off kept rendering");
                return false;
            }
            if !poll_until(3_000, || sticker.is_animating()).await {
                probe_fail("animations master on resumes stickers");
                return false;
            }
        }

        probe_step("lottie picker hover");
        self.open_stickers();
        if !poll_until(4_000, || {
            self.stickers.stickers_ready() || self.stickers.retry_visible()
        })
        .await
        {
            probe_fail("lottie picker packs settlement");
            return false;
        }
        if self.stickers.retry_visible() {
            self.stickers.trigger_retry();
            if !poll_until(4_000, || self.stickers.stickers_ready()).await {
                probe_fail("lottie picker packs retry");
                return false;
            }
        }
        let Some(pack) = self.stickers.probe_pack_id_by_title("Omarchy") else {
            probe_fail("lottie picker Omarchy pack");
            return false;
        };
        self.stickers.probe_select_pack(&pack);
        if !poll_until(4_000, || {
            self.stickers.current_pack() == pack && self.stickers.sticker_sendable(9104)
        })
        .await
        {
            probe_fail("animated sticker cell listed and sendable");
            return false;
        }
        if !poll_until(8_000, || {
            self.stickers
                .lottie(9104)
                .is_some_and(|cell| cell.is_ready())
        })
        .await
        {
            probe_fail("animated sticker cell first frame");
            return false;
        }
        let Some(cell) = self.stickers.lottie(9104) else {
            probe_fail("animated sticker cell widget");
            return false;
        };
        if self.stickers.sticker_cell_size(9104)
            != Some(((lottie::CELL_SIZE, lottie::CELL_SIZE), (lottie::CELL_SIZE, lottie::CELL_SIZE)))
            || cell.logical_size() != lottie::CELL_SIZE
        {
            probe_fail("animated sticker picker cell size");
            return false;
        }
        if cell.is_animating() {
            probe_fail("animated sticker cell animates without hover");
            return false;
        }
        let idle = cell.frames_shown();
        if !self.stickers.probe_hover_sticker(9104, true) {
            probe_fail("animated sticker cell hover enter");
            return false;
        }
        if !poll_until(3_000, || cell.is_animating() && cell.frames_shown() > idle).await {
            probe_fail("animated sticker cell plays on hover");
            return false;
        }
        if !self.stickers.probe_hover_sticker(9104, false) {
            probe_fail("animated sticker cell hover leave");
            return false;
        }
        if !poll_until(2_000, || !cell.is_animating() && cell.frame_index() == 0).await {
            probe_fail("animated sticker cell pauses on leave");
            return false;
        }
        let left = cell.frames_shown();
        glib::timeout_future(Duration::from_millis(300)).await;
        if cell.frames_shown() != left {
            probe_fail("unhovered animated sticker cell kept rendering");
            return false;
        }

        probe_step("lottie picker first-frame cache");
        self.stickers.probe_select_pack("recent");
        if !poll_until(4_000, || self.stickers.current_pack() == "recent").await {
            probe_fail("switch away from cached sticker pack");
            return false;
        }
        self.stickers.probe_select_pack(&pack);
        if !poll_until(8_000, || {
            self.stickers.current_pack() == pack
                && self
                    .stickers
                    .lottie(9104)
                    .is_some_and(|sticker| sticker.is_ready() && sticker.seeded_from_cache())
        })
        .await
        {
            probe_fail("rebuilt picker cell did not use first-frame cache");
            return false;
        }
        self.close_stickers();

        // Wave 6F probe steps (specs/spec-wave6.md §1.11, §7)
        let marta = self
            .chatlist
            .ordered()
            .into_iter()
            .find_map(|(id, title)| (title == "Marta").then_some(id))
            .unwrap_or(1);
        self.clone().open_chat(marta);
        if !poll_until(4_000, || !self.messages.is_loading()).await {
            probe_fail("open Marta for wave 6F");
            return false;
        }

        // Camera failure path when OMG_MOCK_CAMERA=none
        unsafe {
            std::env::set_var("OMG_MOCK_CAMERA", "none");
        }
        self.clone().start_video_note();
        if !poll_until(4_000, || {
            self.messages.video_recorder_visible()
                && self.messages.video_recorder_status().contains("no camera found")
        })
        .await
        {
            unsafe {
                std::env::remove_var("OMG_MOCK_CAMERA");
            }
            probe_fail("video note record: camera failure not shown");
            return false;
        }
        self.messages.probe_video_recorder_close();
        if !poll_until(2_000, || !self.messages.video_recorder_visible()).await {
            unsafe {
                std::env::remove_var("OMG_MOCK_CAMERA");
            }
            probe_fail("video note record: error close failed");
            return false;
        }
        unsafe {
            std::env::remove_var("OMG_MOCK_CAMERA");
        }

        // A start that resolves after cancel/restart must clean up its own
        // process before the replacement start attaches a receiver.
        probe_step("video note stale start");
        self.probe_video_start_attach_delay.set(500);
        self.clone().start_video_note();
        glib::timeout_future(Duration::from_millis(50)).await;
        self.messages.probe_video_recorder_cancel();
        self.probe_video_start_attach_delay.set(0);
        self.clone().start_video_note();
        if !poll_until(12_000, || {
            self.messages.video_recorder_visible()
                && self.messages.video_recorder_status().contains("Recording")
                && self.messages.video_recorder_has_frame()
        })
        .await
        {
            probe_fail("video note stale start: replacement did not own the receiver");
            return false;
        }
        self.messages.probe_video_recorder_cancel();
        if !poll_until(4_000, || {
            !self.messages.video_recorder_visible()
                && self.video_recorder.borrow().phase == VideoRecorderPhase::Idle
        })
        .await
        {
            probe_fail("video note stale start: serialized cleanup did not finish");
            return false;
        }

        // 1. video note record
        probe_step("video note record");
        self.clone().start_video_note();
        if !poll_until(4_000, || {
            self.messages.video_recorder_visible()
                && self.messages.video_recorder_status().contains("Recording")
        })
        .await
        {
            probe_fail("video note record: bar not recording");
            return false;
        }
        if !poll_until(12_000, || self.messages.video_recorder_has_frame()).await {
            probe_fail("video note record: no frame received");
            return false;
        }

        // 2. video note cancel
        probe_step("video note cancel");
        self.messages.probe_video_recorder_cancel();
        if !poll_until(3_000, || !self.messages.video_recorder_visible()).await {
            probe_fail("video note cancel: bar stayed visible");
            return false;
        }

        // 3. video note send
        probe_step("video note send");
        let notes_before = self
            .messages
            .messages()
            .iter()
            .filter(|m| m.media == Some(MediaKind::VideoNote))
            .count();
        self.clone().start_video_note();
        if !poll_until(12_000, || self.messages.video_recorder_has_frame()).await {
            probe_fail("video note send: no frame received");
            return false;
        }
        // Record a realistic second before sending: a sub-second clip has no
        // finalized mp4 yet and the recorder rejects it as empty.
        glib::timeout_future(Duration::from_millis(1_200)).await;
        self.messages.probe_video_recorder_send();
        if !poll_until(6_000, || {
            !self.messages.video_recorder_visible()
                && self
                    .messages
                    .messages()
                    .iter()
                    .filter(|m| m.media == Some(MediaKind::VideoNote))
                    .count()
                    > notes_before
        })
        .await
        {
            probe_fail("video note send: message not sent");
            return false;
        }

        // 4. live location send
        probe_step("live location send");
        let live_before = self
            .messages
            .messages()
            .iter()
            .filter(|m| m.location.as_ref().is_some_and(|l| l.live.is_some()))
            .count();
        self.clone().open_location_dialog();
        if !poll_until(3_000, || self.location_dialog.is_open()).await {
            probe_fail("live location send: dialog failed to open");
            return false;
        }
        self.location_dialog.probe_set_point(52.52, 13.405);
        self.location_dialog.probe_set_live(1); // 15 min
        self.location_dialog.probe_send();
        if !poll_until(5_000, || {
            !self.location_dialog.is_open()
                && self
                    .messages
                    .messages()
                    .iter()
                    .filter(|m| m.location.as_ref().is_some_and(|l| l.live.is_some()))
                    .count()
                    > live_before
        })
        .await
        {
            probe_fail("live location send: message not created");
            return false;
        }
        let live_msg = self
            .messages
            .messages()
            .into_iter()
            .rev()
            .find(|m| m.location.as_ref().is_some_and(|l| l.live.is_some()))
            .map(|m| m.id)
            .unwrap();
        if !poll_until(3_000, || {
            self.messages
                .card_text(live_msg)
                .is_some_and(|t| t.contains("Live"))
        })
        .await
        {
            probe_fail("live location send: card text does not contain Live");
            return false;
        }

        // 5. live location update
        probe_step("live location update");
        if !self
            .messages
            .probe_click_card_button(live_msg, "Update position")
        {
            probe_fail("live location update: button missing");
            return false;
        }
        if !poll_until(3_000, || self.location_dialog.is_open()).await {
            probe_fail("live location update: dialog not opened");
            return false;
        }
        self.location_dialog.probe_set_point(52.53, 13.41);
        let old_fail_once = std::env::var_os("OMG_MOCK_FAIL_ONCE");
        let mut fail_once = old_fail_once
            .as_ref()
            .map(|value| value.to_string_lossy().into_owned())
            .unwrap_or_default();
        if !fail_once
            .split(',')
            .any(|name| name.trim() == "UpdateLiveLocation")
        {
            if !fail_once.is_empty() {
                fail_once.push(',');
            }
            fail_once.push_str("UpdateLiveLocation");
        }
        unsafe {
            std::env::set_var("OMG_MOCK_FAIL_ONCE", fail_once);
        }
        self.location_dialog.probe_send();
        let retry_ready = poll_until(4_000, || {
            self.location_dialog.is_open()
                && self.location_dialog.probe_retry_visible()
                && self
                    .location_dialog
                    .probe_error()
                    .contains("mock: transient failure")
        })
        .await;
        unsafe {
            match old_fail_once {
                Some(value) => std::env::set_var("OMG_MOCK_FAIL_ONCE", value),
                None => std::env::remove_var("OMG_MOCK_FAIL_ONCE"),
            }
        }
        if !retry_ready {
            probe_fail("live location update: retry affordance missing after failure");
            return false;
        }
        // Retrying the same dialog must retain the update message identity.
        self.location_dialog.probe_send();
        if !poll_until(4_000, || !self.location_dialog.is_open()).await {
            probe_fail("live location update: retry failed");
            return false;
        }
        if !poll_until(4_000, || {
            self.messages
                .message(live_msg)
                .and_then(|m| m.location)
                .is_some_and(|l| (l.point.lat - 52.53).abs() < 0.001)
        })
        .await
        {
            probe_fail("live location update: point coordinates not updated");
            return false;
        }
        if !self.messages.probe_live_timer_active(live_msg) {
            probe_fail("live location update: rebuilt card lost its sole live timer");
            return false;
        }

        // 6. live location stop
        probe_step("live location stop");
        if !self
            .messages
            .probe_click_card_button(live_msg, "Stop sharing")
        {
            probe_fail("live location stop: button missing");
            return false;
        }
        if !poll_until(4_000, || {
            self.messages
                .card_text(live_msg)
                .is_some_and(|t| t.contains("Sharing ended"))
        })
        .await
        {
            probe_fail("live location stop: card not flipped to Sharing ended");
            return false;
        }

        // 7. stories strip
        probe_step("stories strip");
        if !poll_until(4_000, || {
            self.stories_strip.is_visible() && self.stories_strip.peer_count() >= 2
        })
        .await
        {
            probe_fail("stories strip: strip not visible or peers missing");
            return false;
        }
        if self.stories_strip.peer_unread(1) != Some(true) {
            probe_fail("stories strip: Marta unread ring not present");
            return false;
        }
        if !self.stories_strip.probe_focus_peer(1) {
            probe_fail("stories strip: could not focus Marta button");
            return false;
        }
        self.reload_stories();
        if !poll_until(4_000, || !self.stories_strip.probe_focus_within()).await {
            probe_fail("stories strip: focused button removed without moving focus");
            return false;
        }

        // 8. stories viewer
        probe_step("stories viewer");
        self.stories_strip.probe_click_peer(1);
        if !poll_until(4_000, || {
            self.stories_viewer.is_open()
                && self.stories_viewer.current_peer_name() == "Marta"
                && self.stories_viewer.story_count() >= 2
        })
        .await
        {
            probe_fail("stories viewer: viewer not open for Marta");
            return false;
        }
        if self.stories_viewer.current_story_index() != 0 {
            probe_fail("stories viewer: expected story index 0");
            return false;
        }
        if !self.stories_viewer.probe_has_keyboard_focus() {
            probe_fail("stories viewer: viewer did not take keyboard focus");
            return false;
        }
        self.stories_viewer.probe_middle_click();
        glib::timeout_future(Duration::from_millis(150)).await;
        if self.stories_viewer.current_story_index() != 0 {
            probe_fail("stories viewer: middle-third click navigated");
            return false;
        }
        if !poll_until(3_000, || self.stories_viewer.probe_progress() > 0.0).await {
            probe_fail("stories viewer: progress did not start");
            return false;
        }
        self.stories_viewer.probe_toggle_pause();
        let paused_progress = self.stories_viewer.probe_progress();
        glib::timeout_future(Duration::from_millis(350)).await;
        if (self.stories_viewer.probe_progress() - paused_progress).abs() > 0.001 {
            probe_fail("stories viewer: progress advanced while paused");
            return false;
        }
        self.stories_viewer.probe_toggle_pause();

        // 9. stories viewer advance
        probe_step("stories viewer advance");
        self.stories_viewer.probe_advance();
        if !poll_until(4_000, || self.stories_viewer.current_story_index() == 1).await {
            probe_fail("stories viewer advance: did not advance to second story");
            return false;
        }

        // 10. stories seen
        probe_step("stories seen");
        self.stories_viewer.probe_close();
        if !poll_until(2_000, || !self.stories_viewer.is_open()).await {
            probe_fail("stories seen: viewer failed to close");
            return false;
        }
        if !poll_until(4_000, || {
            self.chatlist.probe_chat_story_ring(1) == Some(StoryRing::Read)
                && self.stories_strip.peer_unread(1) == Some(false)
        })
        .await
        {
            probe_fail("stories seen: Marta story ring not marked Read");
            return false;
        }

        true
    }
}

impl Drop for ShellInner {
    fn drop(&mut self) {
        if let Some(source) = self.clock_source.borrow_mut().take() {
            source.remove();
        }
        if let Some(source) = self.theme_switch_timeout.borrow_mut().take() {
            source.remove();
        }
        if let Some(source) = self.search_timeout.borrow_mut().take() {
            source.remove();
        }
        if let Some(source) = self.dialogs_reload_timeout.borrow_mut().take() {
            source.remove();
        }
        if let Some(source) = self.ui_save_timeout.borrow_mut().take() {
            source.remove();
            if let Err(error) = self.ui_state.borrow().save() {
                eprintln!("ui-state: {error}");
            }
        }
        if let Some(tick) = self.layout_tick.borrow_mut().take() {
            tick.remove();
        }
        if let Some(source) = self.live_expiry_timer.borrow_mut().take()
            && let Some(source) = glib::MainContext::default().find_source_by_id(&source) {
                source.destroy();
            }
        self.main_menu.dismiss();
        self.chatlist.dismiss_popovers();
        self.messages.dismiss_owned_popovers();
        // §2.3: nothing keeps playing once the window is gone.
        self.messages.reset_players();
    }
}

fn clock_text() -> String {
    Local::now().format("%H:%M:%S").to_string()
}

/// Wave 6D: `.tgs` stickers are gzipped Lottie, rendered by `ui::lottie`.
fn is_lottie(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("tgs"))
}

/// `main.rs` gives smoke runs a private state file. The probe contract also
/// supports an explicitly pre-seeded state file, so retain just the
/// shell-owned fields from the process's initial environment when both are
/// present. `/proc/self/environ` is the initial environment on Linux and is
/// unaffected by the smoke override performed before GTK starts.
fn load_shell_ui_state(probe: bool) -> (UiState, Option<RestoredUiState>) {
    let mut state = UiState::load();
    if !probe {
        return (state, None);
    }
    let Some(path) = initial_environment_path("OMG_UISTATE_PATH") else {
        return (state, None);
    };
    let external = std::fs::read_to_string(path)
        .ok()
        .and_then(|text| toml::from_str::<UiState>(&text).ok());
    let Some(mut external) = external else {
        return (state, None);
    };
    external.sidebar_width = external.sidebar_width.clamp(220, 2000);
    state.sidebar_width = external.sidebar_width;
    state.sidebar_collapsed = external.sidebar_collapsed;
    state.folder_id = external.folder_id;
    state.info_panel_open = external.info_panel_open;
    state.info_width = external.info_width.max(280);
    let restored = Some(RestoredUiState {
        sidebar_width: state.sidebar_width,
        sidebar_collapsed: state.sidebar_collapsed,
        folder_id: state.folder_id,
        info_panel_open: state.info_panel_open,
        info_width: state.info_width,
    });
    (state, restored)
}

fn initial_environment_path(name: &str) -> Option<PathBuf> {
    let environment = std::fs::read("/proc/self/environ").ok()?;
    let prefix = format!("{name}=").into_bytes();
    environment
        .split(|byte| *byte == 0)
        .find_map(|entry| entry.strip_prefix(prefix.as_slice()))
        .and_then(|value| std::str::from_utf8(value).ok())
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn environment_listed(variable: &str, value: &str) -> bool {
    std::env::var(variable)
        .is_ok_and(|list| list.split(',').any(|candidate| candidate.trim() == value))
}

pub fn clamp_sidebar_width(saved_width: i32, window_width: i32) -> i32 {
    let maximum = (((window_width.max(0) as f64) * 0.45).floor() as i32).max(220);
    saved_width.clamp(220, maximum)
}

fn effective_mute(pending_intent: Option<bool>, cached_summary: Option<bool>) -> bool {
    pending_intent.or(cached_summary).unwrap_or(false)
}

/// Matches `\d\d:\d\d:\d\d` (with an optional " edited" suffix).
fn time_has_seconds(text: &str) -> bool {
    let text = text.strip_suffix(" edited").unwrap_or(text);
    let bytes = text.as_bytes();
    bytes.len() == 8
        && bytes[2] == b':'
        && bytes[5] == b':'
        && [0usize, 1, 3, 4, 6, 7]
            .into_iter()
            .all(|index| bytes[index].is_ascii_digit())
}

/// The chat-list identity of a chat id: a forum topic belongs to its forum
/// row (spec §6.3 — the sidebar keeps showing the forum, never a topic).
fn dialog_id(chat_id: i64) -> i64 {
    split_topic_chat_id(chat_id)
        .map(|(forum_id, _)| forum_id)
        .unwrap_or(chat_id)
}

fn chat_title(message: &Msg) -> String {
    message.chat_title.clone()
}

fn message_preview(message: &Msg) -> String {
    if !message.text.is_empty() {
        return message.text.clone();
    }
    match message.media {
        Some(MediaKind::Photo) => "[photo]".to_string(),
        Some(MediaKind::Sticker) => "[sticker]".to_string(),
        Some(MediaKind::Voice) => "[voice message]".to_string(),
        Some(MediaKind::Document) => "[file]".to_string(),
        Some(MediaKind::Video) => "[video]".to_string(),
        Some(MediaKind::Gif) => "[GIF]".to_string(),
        Some(MediaKind::Audio) => "[audio]".to_string(),
        Some(MediaKind::VideoNote) => "[video message]".to_string(),
        Some(MediaKind::Location) => "[location]".to_string(),
        Some(MediaKind::Venue) => "[venue]".to_string(),
        Some(MediaKind::Contact) => "[contact]".to_string(),
        Some(MediaKind::Dice) => "[dice]".to_string(),
        Some(MediaKind::Poll) => "[poll]".to_string(),
        Some(MediaKind::Unsupported) => "[unsupported]".to_string(),
        None => String::new(),
    }
}

fn message_content(message: &Msg) -> String {
    if message.text.is_empty() {
        message_preview(message)
    } else {
        message.text.clone()
    }
}

fn recording_error(error: Option<&str>) -> String {
    let error = error.unwrap_or("could not record a voice message");
    if error.contains("ffmpeg") {
        "ffmpeg is not installed — run: sudo pacman -S ffmpeg".to_string()
    } else {
        error.to_string()
    }
}

fn ai_prefs(settings: &Settings) -> Prefs {
    Prefs {
        chat_provider: settings.ai.chat_provider.clone(),
        transcribe_provider: settings.ai.transcribe_provider.clone(),
        chat_model: settings.ai.chat_model.clone(),
        ollama_url: settings.ai.ollama_url.clone(),
    }
}

fn transcript(messages: &[Msg], limit: usize, search: bool) -> String {
    let start = messages.len().saturating_sub(limit);
    messages[start..]
        .iter()
        .map(|message| {
            let sender = if message.outgoing {
                "You"
            } else if message.sender.trim().is_empty() {
                "Unknown"
            } else {
                &message.sender
            };
            if search {
                format!(
                    "{} {}: {}",
                    message.ts.format("%H:%M"),
                    sender,
                    message_content(message)
                )
            } else {
                format!(
                    "[{}] {}: {}",
                    message.ts.format("%H:%M"),
                    sender,
                    message_content(message)
                )
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn search_transcript(title: &str, messages: &[Msg]) -> String {
    let title = clean_remote_text(title, 80);
    transcript(messages, 50, true)
        .lines()
        .map(|line| format!("[{title}] {line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn clean_remote_text(text: &str, max_chars: usize) -> String {
    text.chars()
        .filter(|character| !character.is_control())
        .take(max_chars)
        .collect()
}

fn done_text(state: Option<&ReqState<String>>) -> Option<String> {
    match state {
        Some(ReqState::Done(text)) => Some(text.clone()),
        _ => None,
    }
}

fn last_line(text: &str) -> String {
    text.lines().last().unwrap_or_default().to_string()
}

fn virtual_last_id(stores: &RefCell<HashMap<i64, VirtualStore>>, chat_id: i64) -> i32 {
    stores
        .borrow()
        .get(&chat_id)
        .and_then(|store| store.msgs.last())
        .map(|message| message.id)
        .unwrap_or(i32::MIN)
}

struct FileSendContext { chat_id: i64, epoch: u64, session_epoch: u64, token: u64 }

struct ProbeResize {
    pane_width: i32,
}

fn sidebar_position_fits(window_width: i32, position: i32) -> bool {
    position <= (f64::from(window_width) * 0.45).floor() as i32
}

fn probe_bubble_limit(pane_width: i32) -> i32 {
    ((f64::from(pane_width) * 0.66).floor() as i32).clamp(1, 520)
}

async fn wait_for_frame(widget: &gtk::Widget) {
    let (sender, receiver) = async_channel::bounded(1);
    widget.add_tick_callback(move |_, _| {
        let _ = sender.try_send(());
        glib::ControlFlow::Break
    });
    let _ = receiver.recv().await;
}

async fn poll_until<F>(timeout_ms: u64, condition: F) -> bool
where
    F: Fn() -> bool,
{
    let steps = timeout_ms / 25;
    for _ in 0..steps {
        if condition() {
            return true;
        }
        glib::timeout_future(Duration::from_millis(25)).await;
    }
    condition()
}

/// `OMG_PROBE_TRACE=1` prints each traversal step before it runs — bisects
/// crashes that abort the process without a Rust frame.
fn probe_step(step: &str) {
    if std::env::var_os("OMG_PROBE_TRACE").is_some() {
        shell_log!("[probe] {step}");
    }
}

fn probe_fail(step: &str) {
    shell_log!("probe failed: {step}");
    std::process::exit(1);
}

fn should_report_online(visible: bool, focused: bool, inactive_for: Duration) -> bool {
    visible && focused && inactive_for < Duration::from_secs(5 * 60)
}

#[cfg(test)]
mod wave5_tests {
    #[test]
    fn presence_requires_visible_focused_recent_activity() {
        use super::should_report_online;
        use std::time::Duration;
        assert!(should_report_online(true, true, Duration::ZERO));
        assert!(should_report_online(true, true, Duration::from_secs(299)));
        assert!(!should_report_online(true, true, Duration::from_secs(300)));
        assert!(!should_report_online(true, false, Duration::ZERO));
        assert!(!should_report_online(false, true, Duration::ZERO));
        assert!(!should_report_online(false, false, Duration::from_secs(600)));
    }
    use super::{clamp_sidebar_width, effective_mute};

    #[test]
    fn sidebar_width_is_clamped_to_minimum_and_window_fraction() {
        assert_eq!(clamp_sidebar_width(300, 1100), 300);
        assert_eq!(clamp_sidebar_width(700, 1000), 450);
        assert_eq!(clamp_sidebar_width(100, 1000), 220);
        assert_eq!(clamp_sidebar_width(300, 400), 220);
    }

    #[test]
    fn pending_mute_intent_precedes_stale_cached_summary() {
        assert!(effective_mute(Some(true), Some(false)));
        assert!(!effective_mute(Some(false), Some(true)));
        assert!(effective_mute(None, Some(true)));
        assert!(!effective_mute(None, None));
    }
}
