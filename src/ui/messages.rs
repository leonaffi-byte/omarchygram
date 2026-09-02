use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;

use chrono::{DateTime, Datelike, Local};
use gtk::gdk;
use gtk::glib;
use gtk::glib::subclass::prelude::ObjectSubclassIsExt;
use gtk::prelude::*;
use gtk4 as gtk;

use crate::tg::{
    ChatInfo, ChatKind, ChatSummary, MediaKind, Msg, MsgVersion, Presence, SpanKind, Tg,
};

use super::anim::Effects;
use super::avatar::Avatar;
use super::icons;
use super::markup;
use super::menus::{self, ChatAction, PopoverSlot};
use super::recorder::{RecorderBar, RecorderUiAction};
use super::virtual_chat::is_virtual;

mod bubble_clamp_imp {
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    use gtk::glib;
    use gtk::prelude::*;
    use gtk::subclass::prelude::*;
    use gtk4 as gtk;

    #[derive(Default)]
    pub struct BubbleClamp {
        pub child: RefCell<Option<gtk::Widget>>,
        pub pane_width: RefCell<Option<Rc<Cell<i32>>>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for BubbleClamp {
        const NAME: &'static str = "OmgBubbleClamp";
        type Type = super::BubbleClamp;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for BubbleClamp {
        fn dispose(&self) {
            if let Some(child) = self.child.borrow_mut().take() {
                child.unparent();
            }
        }
    }

    impl BubbleClamp {
        /// Width the child really gets: its natural width capped by the
        /// bubble limit and the available width, never below its minimum.
        fn child_width(&self, child: &gtk::Widget, available: i32, target: i32) -> i32 {
            let (minimum, natural, _, _) = child.measure(gtk::Orientation::Horizontal, -1);
            natural.min(target).min(available).max(minimum).max(1)
        }
    }

    impl WidgetImpl for BubbleClamp {
        fn request_mode(&self) -> gtk::SizeRequestMode {
            self.child
                .borrow()
                .as_ref()
                .map(gtk::prelude::WidgetExt::request_mode)
                .unwrap_or(gtk::SizeRequestMode::ConstantSize)
        }

        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            let Some(child) = self.child.borrow().as_ref().cloned() else {
                return (0, 0, -1, -1);
            };
            let target = super::bubble_width_limit(
                self.pane_width.borrow().as_ref().map(|width| width.get()),
            );
            if orientation == gtk::Orientation::Horizontal {
                let (minimum, natural, min_baseline, nat_baseline) =
                    child.measure(orientation, for_size);
                (
                    minimum,
                    natural.min(target).max(minimum),
                    min_baseline,
                    nat_baseline,
                )
            } else {
                // Height must be measured at the width the child will really
                // get (the clamped one), or wrapped text is cut off.
                let width = if for_size > 0 {
                    self.child_width(&child, for_size, target)
                } else {
                    for_size
                };
                child.measure(orientation, width)
            }
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            let Some(child) = self.child.borrow().as_ref().cloned() else {
                return;
            };
            // Allocate the child at the clamped width (not the full row) and
            // keep it at the start or end per its own alignment, so the
            // visible bubble never exceeds the limit.
            let target = super::bubble_width_limit(
                self.pane_width.borrow().as_ref().map(|width| width.get()),
            );
            let child_width = self.child_width(&child, width, target);
            let x = if child.halign() == gtk::Align::End {
                width - child_width
            } else {
                0
            };
            let transform = (x > 0).then(|| {
                gtk::gsk::Transform::new().translate(&gtk::graphene::Point::new(x as f32, 0.0))
            });
            child.allocate(child_width, height, baseline, transform);
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            if let Some(child) = self.child.borrow().as_ref() {
                self.obj().snapshot_child(child, snapshot);
            }
        }
    }
}

glib::wrapper! {
    pub struct BubbleClamp(ObjectSubclass<bubble_clamp_imp::BubbleClamp>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl BubbleClamp {
    fn new(child: &impl IsA<gtk::Widget>, pane_width: Rc<Cell<i32>>) -> Self {
        let widget: Self = glib::Object::builder().build();
        child.set_parent(&widget);
        *widget.imp().child.borrow_mut() = Some(child.clone().upcast());
        *widget.imp().pane_width.borrow_mut() = Some(pane_width);
        widget
    }
}

mod photo_clamp_imp {
    use std::cell::RefCell;

    use gtk::glib;
    use gtk::prelude::*;
    use gtk::subclass::prelude::*;
    use gtk4 as gtk;

    #[derive(Default)]
    pub struct PhotoClamp {
        pub child: RefCell<Option<gtk::Widget>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for PhotoClamp {
        const NAME: &'static str = "OmgPhotoClamp";
        type Type = super::PhotoClamp;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for PhotoClamp {
        fn dispose(&self) {
            if let Some(child) = self.child.borrow_mut().take() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for PhotoClamp {
        fn request_mode(&self) -> gtk::SizeRequestMode {
            self.child
                .borrow()
                .as_ref()
                .map(gtk::prelude::WidgetExt::request_mode)
                .unwrap_or(gtk::SizeRequestMode::ConstantSize)
        }

        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            let Some(child) = self.child.borrow().as_ref().cloned() else {
                return (0, 0, -1, -1);
            };
            let (minimum, natural, min_baseline, nat_baseline) =
                child.measure(orientation, for_size);
            if orientation == gtk::Orientation::Horizontal {
                (
                    minimum,
                    natural.min(320).max(minimum),
                    min_baseline,
                    nat_baseline,
                )
            } else {
                (minimum, natural, min_baseline, nat_baseline)
            }
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            if let Some(child) = self.child.borrow().as_ref() {
                child.allocate(width, height, baseline, None);
            }
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            if let Some(child) = self.child.borrow().as_ref() {
                self.obj().snapshot_child(child, snapshot);
            }
        }
    }
}

glib::wrapper! {
    pub struct PhotoClamp(ObjectSubclass<photo_clamp_imp::PhotoClamp>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl PhotoClamp {
    fn new(child: &impl IsA<gtk::Widget>) -> Self {
        let widget: Self = glib::Object::builder().build();
        child.set_parent(&widget);
        *widget.imp().child.borrow_mut() = Some(child.clone().upcast());
        widget
    }
}

fn bubble_width_limit(parent_width: Option<i32>) -> i32 {
    parent_width
        .filter(|width| *width > 0)
        .map(|width| ((f64::from(width) * 0.66).floor() as i32).min(520))
        .unwrap_or(520)
        .max(1)
}

#[derive(Clone)]
pub enum MessageAction {
    Submit,
    Mic,
    Stickers,
    RecorderCancel,
    RecorderSend,
    RecorderRetry,
    Attach,
    DraftChanged,
    DraftRetry,
    RetryReaction,
    RetryAvailableReactions,
    SearchChanged(String),
    SearchPrevious,
    SearchNext,
    SearchOlder,
    SearchRetry,
    SearchClose,
    Header(ChatAction),
    DropFile(gtk::gio::File),
    Reply(i32),
    Edit(i32),
    EditHistory(i32),
    Forward(i32),
    Select(i32),
    SelectionForward,
    SelectionDelete,
    SelectionCopy,
    SelectionCancel,
    Reaction {
        msg_id: i32,
        emoji: Option<String>,
    },
    RevealSpoiler {
        msg_id: i32,
        start: usize,
        end: usize,
    },
    OpenLink(String),
    OpenMention(i64),
    UnpinMessage(i32),
    RetryPinned,
    Delete(i32),
    Media(i32),
    Paginate,
    CancelMode,
    CopyMessageId(i32),
    CopyUserId(i32),
    JumpToMessage(i32),
    JumpToDate(DateTime<Local>),
    JumpToLatest,
    DraftReply(i32),
    Translate(i32),
    Summarize(i32),
    Transcribe(i32),
}

#[derive(Clone, Debug)]
pub enum MediaState {
    NotStarted,
    InFlight,
    Done(PathBuf),
    Failed,
}

#[derive(Clone)]
struct MessageRow {
    widget: BubbleClamp,
    selection: gtk::CheckButton,
    forwarded: gtk::Label,
    sender: gtk::Label,
    quote: gtk::Label,
    text_block: gtk::Box,
    text: Rc<RefCell<Option<gtk::Label>>>,
    revealed_spoilers: Rc<RefCell<HashSet<(usize, usize)>>>,
    web_preview: gtk::Box,
    time: gtk::Label,
    deleted_tag: gtk::Label,
    receipt: gtk::Label,
    reactions: gtk::Box,
    reaction_buttons: Rc<RefCell<Vec<gtk::Button>>>,
    reaction_anchor: gtk::Button,
    media_slot: gtk::Box,
    media_button: Option<gtk::Button>,
    transcribe_button: Option<gtk::Button>,
    aux_slot: gtk::Box,
    animation_sources: Rc<RefCell<Vec<glib::SourceId>>>,
    /// The asciiload placeholder timer only — drained when the media
    /// finishes, without touching receipt/cascade/particle timers.
    media_loading_source: Rc<RefCell<Option<glib::SourceId>>>,
}

struct MessageEntry {
    msg: Msg,
    row: MessageRow,
    media_state: MediaState,
    media_retryable: bool,
}

#[derive(Default)]
struct MessageStore {
    chat_id: Option<i64>,
    order: Vec<i32>,
    entries: HashMap<i32, MessageEntry>,
}

struct EditMode {
    msg_id: i32,
    draft: String,
}

#[derive(Default)]
struct SelectionBook {
    ids: HashSet<i32>,
}

impl SelectionBook {
    fn set(&mut self, msg_id: i32, selected: bool) {
        if selected {
            self.ids.insert(msg_id);
        } else {
            self.ids.remove(&msg_id);
        }
    }

    fn remove_deleted(&mut self, ids: &[i32]) -> bool {
        let before = self.ids.len();
        self.ids.retain(|id| !ids.contains(id));
        before != self.ids.len()
    }

    fn ordered(&self, order: &[i32]) -> Vec<i32> {
        order
            .iter()
            .filter(|id| self.ids.contains(id))
            .copied()
            .collect()
    }
}

type MediaReady = Box<dyn FnOnce(PathBuf)>;

struct MessagesInner {
    header_avatar: Avatar,
    header_title: gtk::Label,
    online_dot: gtk::Box,
    typing: gtk::Label,
    base_status: RefCell<String>,
    header_summary: RefCell<Option<ChatSummary>>,
    chat_kind: Cell<ChatKind>,
    read_outbox: Cell<i32>,
    ghost: gtk::Label,
    clock: gtk::Label,
    header_search: gtk::Button,
    header_info: gtk::Button,
    header_more: gtk::Button,
    header_actions: gtk::Box,
    search_bar: gtk::Box,
    search_entry: gtk::SearchEntry,
    search_position: gtk::Label,
    search_previous: gtk::Button,
    search_next: gtk::Button,
    search_older: gtk::Button,
    search_retry: gtk::Button,
    pinned_bar: gtk::Box,
    pinned_text: gtk::Label,
    pinned_message: RefCell<Option<Msg>>,
    pinned_more: gtk::Button,
    pinned_retry: gtk::Button,
    bottom_button: gtk::Button,
    bottom_badge: gtk::Label,
    bottom_unread: Cell<u32>,
    empty_effects: gtk::Overlay,
    date_chip: gtk::Label,
    scroll: gtk::ScrolledWindow,
    list: gtk::Box,
    pane_width: Rc<Cell<i32>>,
    probe_pane_width: Cell<Option<i32>>,
    loading: gtk::Label,
    paging_spinner: gtk::Spinner,
    error: gtk::Label,
    draft_retry: gtk::Button,
    operation_retry: gtk::Button,
    reply_bar: gtk::Box,
    reply_label: gtk::Label,
    edit_bar: gtk::Box,
    edit_label: gtk::Label,
    composer: gtk::TextView,
    composer_placeholder: gtk::Label,
    composer_cursor: gtk::Label,
    equalizer: gtk::Box,
    composer_box: gtk::Box,
    sticker: gtk::Button,
    send: gtk::Button,
    send_label: gtk::Label,
    attach: gtk::Button,
    emoji: gtk::Button,
    drop_target: gtk::DropTarget,
    selection_bar: gtk::Box,
    selection_count: gtk::Label,
    selection_copy: gtk::Button,
    selection_forward: gtk::Button,
    selection_delete: gtk::Button,
    selection: RefCell<SelectionBook>,
    selection_mode: Cell<bool>,
    recorder: RecorderBar,
    store: RefCell<MessageStore>,
    action: Rc<RefCell<Option<Rc<dyn Fn(MessageAction)>>>>,
    time_format: RefCell<String>,
    edit_history: Cell<bool>,
    detached: Cell<bool>,
    busy: Cell<bool>,
    composer_signal_blocked: Cell<bool>,
    paging: Cell<bool>,
    exhausted: Cell<bool>,
    suppress_paging: Cell<bool>,
    stick_to_bottom: Cell<bool>,
    reply_to: Cell<Option<i32>>,
    edit: RefCell<Option<EditMode>>,
    ai_draft: Cell<bool>,
    ai_enabled: Cell<bool>,
    virtual_mode: Cell<bool>,
    typing_generation: Cell<u64>,
    typing_animation: RefCell<Option<glib::SourceId>>,
    presence_timeout: RefCell<Option<glib::SourceId>>,
    presence_recent: Cell<bool>,
    unread_divider_shown: Cell<bool>,
    send_spin: RefCell<Option<glib::SourceId>>,
    date_timeout: RefCell<Option<glib::SourceId>>,
    scroll_epoch: Cell<u64>,
    upper_handler: RefCell<Option<glib::SignalHandlerId>>,
    upper_tick: RefCell<Option<gtk::TickCallbackId>>,
    context_popover: RefCell<Option<gtk::Popover>>,
    history_popover: RefCell<Option<gtk::Popover>>,
    reaction_popover: PopoverSlot,
    probe_reaction_chooser: RefCell<Option<gtk::EmojiChooser>>,
    header_popover: PopoverSlot,
    pinned_popover: PopoverSlot,
    composer_popover: PopoverSlot,
    initial_render_count: Cell<u64>,
    history_version_count: Cell<usize>,
    history_current_text: RefCell<Option<String>>,
    media_ready: RefCell<HashMap<i32, Vec<MediaReady>>>,
    day_separators: RefCell<HashMap<chrono::NaiveDate, gtk::Label>>,
    quote_cache: RefCell<HashMap<i32, (String, String)>>,
    pending_messages: RefCell<HashSet<i32>>,
    available_reactions: RefCell<Vec<String>>,
    available_reactions_loading: Cell<bool>,
    available_reactions_error: RefCell<Option<String>>,
    darker_background: RefCell<String>,
    effects: Rc<Effects>,
}

pub struct MessagesView {
    pub widget: gtk::Box,
    inner: Rc<MessagesInner>,
}

impl Clone for MessagesView {
    fn clone(&self) -> Self {
        Self {
            widget: self.widget.clone(),
            inner: self.inner.clone(),
        }
    }
}

impl MessagesView {
    pub fn new(effects: Rc<Effects>) -> Self {
        let widget = gtk::Box::new(gtk::Orientation::Vertical, 0);
        widget.set_hexpand(true);
        widget.set_vexpand(true);

        let header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        header.set_homogeneous(false);
        header.add_css_class("omg-chat-header");
        let header_avatar = Avatar::new(36);
        header.append(&header_avatar.widget);
        let identity = gtk::Box::new(gtk::Orientation::Vertical, 0);
        identity.set_hexpand(true);
        let header_title = gtk::Label::new(Some("Select a chat"));
        header_title.add_css_class("omg-chat-title");
        header_title.set_halign(gtk::Align::Start);
        header_title.set_hexpand(true);
        header_title.set_width_chars(-1);
        header_title.set_max_width_chars(-1);
        header_title.set_ellipsize(gtk::pango::EllipsizeMode::End);
        identity.append(&header_title);
        let typing = gtk::Label::new(None);
        typing.add_css_class("omg-header-status");
        typing.set_halign(gtk::Align::Start);
        typing.set_ellipsize(gtk::pango::EllipsizeMode::End);
        typing.set_visible(true);
        identity.append(&typing);
        header.append(&identity);
        let online_dot = gtk::Box::new(gtk::Orientation::Vertical, 0);
        online_dot.add_css_class("omg-online-dot");
        online_dot.set_size_request(6, 6);
        online_dot.set_valign(gtk::Align::Center);
        online_dot.set_visible(false);
        header.append(&online_dot);

        let ghost = gtk::Label::new(Some("ghost"));
        ghost.add_css_class("omg-ghost");
        ghost.set_visible(false);
        ghost.set_valign(gtk::Align::Center);
        header.append(&ghost);

        let clock = gtk::Label::new(None);
        clock.add_css_class("omg-clock");
        clock.set_visible(false);
        clock.set_valign(gtk::Align::Center);
        // Tabular digits so the ticking clock doesn't jitter the header.
        let clock_attrs = gtk::pango::AttrList::new();
        clock_attrs.insert(gtk::pango::AttrFontFeatures::new("tnum"));
        clock.set_attributes(Some(&clock_attrs));
        header.append(&clock);

        let header_actions = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        header_actions.set_homogeneous(false);
        header_actions.set_hexpand(false);
        header_actions.set_visible(false);
        let header_search = gtk::Button::with_label(icons::SEARCH);
        header_search.add_css_class("omg-icon-button");
        header_search.set_tooltip_text(Some("Search in chat"));
        header_actions.append(&header_search);
        let header_info = gtk::Button::with_label(icons::INFO);
        header_info.add_css_class("omg-icon-button");
        header_info.set_tooltip_text(Some("Chat info"));
        header_actions.append(&header_info);
        let header_more = gtk::Button::with_label(icons::MORE);
        header_more.add_css_class("omg-icon-button");
        header_more.set_tooltip_text(Some("More actions"));
        header_actions.append(&header_more);
        header.append(&header_actions);

        // Jump back to the latest page; always visible while detached.
        let bottom_button = gtk::Button::new();
        bottom_button.add_css_class("omg-scroll-bottom");
        bottom_button.add_css_class("omg-icon-button");
        bottom_button.set_halign(gtk::Align::End);
        bottom_button.set_valign(gtk::Align::End);
        bottom_button.set_visible(false);
        let bottom_contents = gtk::Overlay::new();
        bottom_contents.set_child(Some(&gtk::Label::new(Some(icons::DOWN))));
        let bottom_badge = gtk::Label::new(None);
        bottom_badge.add_css_class("omg-unread");
        bottom_badge.set_halign(gtk::Align::End);
        bottom_badge.set_valign(gtk::Align::Start);
        bottom_badge.set_visible(false);
        bottom_contents.add_overlay(&bottom_badge);
        bottom_button.set_child(Some(&bottom_contents));

        widget.append(&header);

        let search_bar = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        search_bar.add_css_class("omg-in-chat-search");
        search_bar.set_visible(false);
        let search_entry = gtk::SearchEntry::new();
        search_entry.set_placeholder_text(Some("Search in chat"));
        search_entry.set_hexpand(true);
        search_bar.append(&search_entry);
        let search_position = gtk::Label::new(Some("No results"));
        search_position.add_css_class("omg-small");
        search_bar.append(&search_position);
        let search_previous = gtk::Button::with_label(icons::LEFT);
        search_previous.add_css_class("omg-icon-button");
        search_previous.set_tooltip_text(Some("Previous result"));
        search_bar.append(&search_previous);
        let search_next = gtk::Button::with_label(icons::RIGHT);
        search_next.add_css_class("omg-icon-button");
        search_next.set_tooltip_text(Some("Next result"));
        search_bar.append(&search_next);
        let search_older = gtk::Button::with_label("Load older");
        search_older.add_css_class("omg-menu-item");
        search_older.set_visible(false);
        search_bar.append(&search_older);
        let search_retry = gtk::Button::with_label("Retry");
        search_retry.add_css_class("omg-primary");
        search_retry.set_visible(false);
        search_bar.append(&search_retry);
        let search_close = gtk::Button::with_label(icons::CLOSE);
        search_close.add_css_class("omg-icon-button");
        search_close.set_tooltip_text(Some("Close search"));
        search_bar.append(&search_close);
        widget.append(&search_bar);

        let pinned_bar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        pinned_bar.add_css_class("omg-pinned-bar");
        pinned_bar.set_visible(false);
        let pinned_icon = gtk::Label::new(Some(icons::PIN));
        pinned_icon.add_css_class("omg-muted");
        pinned_bar.append(&pinned_icon);
        let pinned_copy = gtk::Box::new(gtk::Orientation::Vertical, 0);
        pinned_copy.set_hexpand(true);
        let pinned_title = gtk::Label::new(Some("Pinned message"));
        pinned_title.set_halign(gtk::Align::Start);
        pinned_title.add_css_class("omg-small");
        pinned_copy.append(&pinned_title);
        let pinned_text = gtk::Label::new(None);
        pinned_text.set_halign(gtk::Align::Start);
        pinned_text.set_ellipsize(gtk::pango::EllipsizeMode::End);
        pinned_text.add_css_class("omg-muted");
        pinned_copy.append(&pinned_text);
        pinned_bar.append(&pinned_copy);
        let pinned_more = gtk::Button::with_label(icons::MORE);
        pinned_more.add_css_class("omg-icon-button");
        pinned_more.set_tooltip_text(Some("Pinned message actions"));
        pinned_bar.append(&pinned_more);
        let pinned_retry = gtk::Button::with_label("Retry");
        pinned_retry.add_css_class("omg-primary");
        pinned_retry.set_visible(false);
        pinned_bar.append(&pinned_retry);
        widget.append(&pinned_bar);

        // Spacing comes from the rows themselves (4px same sender / 12px
        // otherwise), so the list adds none.
        let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
        list.add_css_class("omg-messages");
        list.set_margin_start(16);
        list.set_margin_end(16);
        list.set_margin_top(8);
        list.set_margin_bottom(8);

        let scroll = gtk::ScrolledWindow::new();
        scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        scroll.set_child(Some(&list));
        scroll.set_hexpand(true);
        scroll.set_vexpand(true);

        let pane_width = Rc::new(Cell::new(0));

        let middle = gtk::Overlay::new();
        let empty_effects = gtk::Overlay::new();
        empty_effects.set_child(Some(&scroll));
        empty_effects.set_hexpand(true);
        empty_effects.set_vexpand(true);
        middle.set_child(Some(&empty_effects));
        middle.set_hexpand(true);
        middle.set_vexpand(true);
        let loading = gtk::Label::new(Some("Select a chat"));
        loading.add_css_class("omg-empty-state");
        loading.set_halign(gtk::Align::Center);
        loading.set_valign(gtk::Align::Center);
        middle.add_overlay(&loading);
        let paging_spinner = gtk::Spinner::new();
        paging_spinner.set_halign(gtk::Align::Center);
        paging_spinner.set_valign(gtk::Align::Start);
        paging_spinner.set_margin_top(8);
        paging_spinner.set_visible(false);
        middle.add_overlay(&paging_spinner);
        let date_chip = gtk::Label::new(None);
        date_chip.add_css_class("omg-date-chip");
        date_chip.set_halign(gtk::Align::Center);
        date_chip.set_valign(gtk::Align::Start);
        date_chip.set_margin_top(8);
        date_chip.set_can_target(false);
        date_chip.set_visible(false);
        middle.add_overlay(&date_chip);
        middle.add_overlay(&bottom_button);
        widget.append(&middle);

        let error = gtk::Label::new(None);
        error.add_css_class("omg-error");
        error.set_halign(gtk::Align::Start);
        error.set_margin_start(8);
        error.set_margin_end(8);
        error.set_wrap(true);
        error.set_visible(false);
        widget.append(&error);
        let draft_retry = gtk::Button::with_label("Retry draft");
        draft_retry.add_css_class("omg-primary");
        draft_retry.set_halign(gtk::Align::Start);
        draft_retry.set_margin_start(8);
        draft_retry.set_visible(false);
        widget.append(&draft_retry);
        let operation_retry = gtk::Button::with_label("Retry");
        operation_retry.add_css_class("omg-primary");
        operation_retry.set_halign(gtk::Align::Start);
        operation_retry.set_margin_start(8);
        operation_retry.set_visible(false);
        widget.append(&operation_retry);

        let reply_bar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        reply_bar.add_css_class("omg-reply-bar");
        reply_bar.set_visible(false);
        let reply_label = gtk::Label::new(None);
        reply_label.set_halign(gtk::Align::Start);
        reply_label.set_hexpand(true);
        reply_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        reply_bar.append(&reply_label);
        let reply_close = gtk::Button::with_label(icons::CLOSE);
        reply_close.add_css_class("omg-bar-close");
        reply_bar.append(&reply_close);
        widget.append(&reply_bar);

        let edit_bar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        edit_bar.add_css_class("omg-edit-bar");
        edit_bar.set_visible(false);
        let edit_label = gtk::Label::new(Some("Editing message"));
        edit_label.set_halign(gtk::Align::Start);
        edit_label.set_hexpand(true);
        edit_bar.append(&edit_label);
        let edit_close = gtk::Button::with_label(icons::CLOSE);
        edit_close.add_css_class("omg-bar-close");
        edit_bar.append(&edit_close);
        widget.append(&edit_bar);

        let selection_bar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        selection_bar.add_css_class("omg-message-selection-bar");
        selection_bar.set_visible(false);
        let selection_count = gtk::Label::new(Some("0 selected"));
        selection_count.set_halign(gtk::Align::Start);
        selection_count.set_hexpand(true);
        selection_bar.append(&selection_count);
        let selection_copy = gtk::Button::with_label(icons::COPY);
        selection_copy.add_css_class("omg-icon-button");
        selection_copy.set_tooltip_text(Some("Copy selected messages"));
        selection_bar.append(&selection_copy);
        let selection_forward = gtk::Button::with_label(icons::FORWARD);
        selection_forward.add_css_class("omg-icon-button");
        selection_forward.set_tooltip_text(Some("Forward selected messages"));
        selection_bar.append(&selection_forward);
        let selection_delete = gtk::Button::with_label(icons::TRASH);
        selection_delete.add_css_class("omg-icon-button");
        selection_delete.add_css_class("omg-danger");
        selection_delete.set_tooltip_text(Some("Delete selected messages"));
        selection_bar.append(&selection_delete);
        let selection_cancel = gtk::Button::with_label(icons::CLOSE);
        selection_cancel.add_css_class("omg-icon-button");
        selection_cancel.set_tooltip_text(Some("Cancel selection"));
        selection_bar.append(&selection_cancel);
        widget.append(&selection_bar);

        let recorder = RecorderBar::new();
        widget.append(&recorder.widget);

        let composer_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        composer_box.add_css_class("omg-composer");
        let attach = gtk::Button::with_label(icons::ATTACH);
        attach.add_css_class("omg-attach");
        attach.set_tooltip_text(Some("Attach file"));
        attach.set_valign(gtk::Align::End);
        // Inert until a chat is open (reset_chat enables both).
        attach.set_sensitive(false);
        composer_box.append(&attach);
        let emoji = gtk::Button::with_label(icons::EMOJI);
        emoji.add_css_class("omg-attach");
        emoji.set_tooltip_text(Some("Emoji"));
        emoji.set_sensitive(false);
        composer_box.append(&emoji);
        let sticker = gtk::Button::with_label(icons::STICKER);
        sticker.add_css_class("omg-attach");
        sticker.set_tooltip_text(Some("Stickers and GIFs"));
        sticker.set_sensitive(false);
        composer_box.append(&sticker);

        let composer = gtk::TextView::new();
        composer.set_sensitive(false);
        composer.set_wrap_mode(gtk::WrapMode::WordChar);
        composer.set_accepts_tab(false);
        composer.set_top_margin(4);
        composer.set_bottom_margin(4);
        composer.set_left_margin(4);
        composer.set_right_margin(4);

        let composer_scroll = gtk::ScrolledWindow::new();
        composer_scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        composer_scroll.set_min_content_height(36);
        composer_scroll.set_max_content_height(160);
        composer_scroll.set_propagate_natural_height(true);
        composer_scroll.set_hexpand(true);
        composer_scroll.set_child(Some(&composer));

        let composer_cursor = gtk::Label::new(Some(icons::COMPOSER_CURSOR));
        composer_cursor.add_css_class("omg-composer-cursor");
        composer_cursor.set_halign(gtk::Align::Start);
        composer_cursor.set_valign(gtk::Align::Start);
        composer_cursor.set_margin_start(4);
        composer_cursor.set_margin_top(4);
        composer_cursor.set_can_target(false);
        composer_cursor.set_visible(false);
        let composer_layer = gtk::Overlay::new();
        composer_layer.set_hexpand(true);
        composer_layer.set_child(Some(&composer_scroll));
        let composer_placeholder = gtk::Label::new(Some("Message"));
        composer_placeholder.add_css_class("omg-composer-placeholder");
        composer_placeholder.set_halign(gtk::Align::Start);
        composer_placeholder.set_valign(gtk::Align::Start);
        composer_placeholder.set_can_target(false);
        composer_layer.add_overlay(&composer_placeholder);
        composer_layer.add_overlay(&composer_cursor);
        composer_box.append(&composer_layer);

        let equalizer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        equalizer.add_css_class("omg-equalizer");
        equalizer.set_valign(gtk::Align::Center);
        equalizer.set_visible(false);
        for _ in 0..5 {
            let bar = gtk::Box::new(gtk::Orientation::Vertical, 0);
            bar.add_css_class("omg-equalizer-bar");
            bar.set_size_request(2, 4);
            equalizer.append(&bar);
        }
        composer_box.append(&equalizer);

        let send = gtk::Button::new();
        send.add_css_class("omg-icon-button");
        send.set_valign(gtk::Align::End);
        send.set_sensitive(false);
        // The label is the measured child; the charge fill is an unmeasured
        // overlay. Expand flags on the fill would propagate up to the button
        // and make the composer compete with the message pane for space.
        let send_contents = gtk::Overlay::new();
        let send_label = gtk::Label::new(Some(icons::MIC));
        send.set_tooltip_text(Some("Voice message"));
        send_label.set_margin_start(8);
        send_label.set_margin_end(8);
        send_contents.set_child(Some(&send_label));
        let send_fill = gtk::Box::new(gtk::Orientation::Vertical, 0);
        send_fill.add_css_class("omg-charge-fill");
        send_fill.set_can_target(false);
        send_contents.add_overlay(&send_fill);
        send.set_child(Some(&send_contents));
        composer_box.append(&send);
        widget.append(&composer_box);

        let action: Rc<RefCell<Option<Rc<dyn Fn(MessageAction)>>>> = Rc::new(RefCell::new(None));
        let drop_target = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
        composer_box.add_controller(drop_target.clone());

        {
            let action = action.clone();
            let effects = effects.clone();
            let animated_button = bottom_button.clone();
            let bottom_badge = bottom_badge.clone();
            bottom_button.connect_clicked(move |_| {
                effects.scroll_to_bottom_pressed(animated_button.upcast_ref());
                bottom_badge.set_visible(false);
                if let Some(callback) = action.borrow().as_ref().cloned() {
                    callback(MessageAction::JumpToLatest);
                }
            });
        }

        let inner = Rc::new(MessagesInner {
            header_avatar,
            header_title,
            online_dot,
            typing,
            base_status: RefCell::new(String::new()),
            header_summary: RefCell::new(None),
            chat_kind: Cell::new(ChatKind::User),
            read_outbox: Cell::new(0),
            ghost,
            clock,
            header_search,
            header_info,
            header_more,
            header_actions,
            search_bar,
            search_entry,
            search_position,
            search_previous,
            search_next,
            search_older,
            search_retry,
            pinned_bar,
            pinned_text,
            pinned_message: RefCell::new(None),
            pinned_more,
            pinned_retry,
            bottom_button,
            bottom_badge,
            bottom_unread: Cell::new(0),
            empty_effects,
            date_chip,
            scroll,
            list,
            pane_width: pane_width.clone(),
            probe_pane_width: Cell::new(None),
            loading,
            paging_spinner,
            error,
            draft_retry,
            operation_retry,
            reply_bar,
            reply_label,
            edit_bar,
            edit_label,
            composer,
            composer_placeholder,
            composer_cursor,
            equalizer,
            composer_box,
            sticker,
            send,
            send_label,
            attach,
            emoji,
            drop_target,
            selection_bar,
            selection_count,
            selection_copy: selection_copy.clone(),
            selection_forward: selection_forward.clone(),
            selection_delete,
            selection: RefCell::new(SelectionBook::default()),
            selection_mode: Cell::new(false),
            recorder,
            store: RefCell::new(MessageStore::default()),
            action,
            time_format: RefCell::new("%H:%M".to_string()),
            edit_history: Cell::new(false),
            detached: Cell::new(false),
            busy: Cell::new(false),
            composer_signal_blocked: Cell::new(false),
            paging: Cell::new(false),
            exhausted: Cell::new(false),
            suppress_paging: Cell::new(false),
            stick_to_bottom: Cell::new(true),
            reply_to: Cell::new(None),
            edit: RefCell::new(None),
            ai_draft: Cell::new(false),
            ai_enabled: Cell::new(false),
            virtual_mode: Cell::new(false),
            typing_generation: Cell::new(0),
            typing_animation: RefCell::new(None),
            presence_timeout: RefCell::new(None),
            presence_recent: Cell::new(false),
            unread_divider_shown: Cell::new(false),
            send_spin: RefCell::new(None),
            date_timeout: RefCell::new(None),
            scroll_epoch: Cell::new(0),
            upper_handler: RefCell::new(None),
            upper_tick: RefCell::new(None),
            context_popover: RefCell::new(None),
            history_popover: RefCell::new(None),
            reaction_popover: PopoverSlot::default(),
            probe_reaction_chooser: RefCell::new(None),
            header_popover: PopoverSlot::default(),
            pinned_popover: PopoverSlot::default(),
            composer_popover: PopoverSlot::default(),
            initial_render_count: Cell::new(0),
            history_version_count: Cell::new(0),
            history_current_text: RefCell::new(None),
            media_ready: RefCell::new(HashMap::new()),
            day_separators: RefCell::new(HashMap::new()),
            quote_cache: RefCell::new(HashMap::new()),
            pending_messages: RefCell::new(HashSet::new()),
            available_reactions: RefCell::new(Vec::new()),
            available_reactions_loading: Cell::new(true),
            available_reactions_error: RefCell::new(None),
            darker_background: RefCell::new(
                crate::theme::load_colors()
                    .get("darker_background")
                    .cloned()
                    .unwrap_or_default(),
            ),
            effects,
        });
        let view = Self { widget, inner };
        {
            let inner = Rc::downgrade(&view.inner);
            view.widget.add_tick_callback(move |pane, _| {
                let Some(inner) = inner.upgrade() else {
                    return glib::ControlFlow::Break;
                };
                let width = inner.probe_pane_width.get().unwrap_or_else(|| pane.width());
                apply_pane_width(&inner, width);
                glib::ControlFlow::Continue
            });
        }
        view.connect_controls(
            reply_close,
            edit_close,
            search_close,
            selection_copy,
            selection_forward,
            selection_cancel,
        );
        view
    }

    fn connect_controls(
        &self,
        reply_close: gtk::Button,
        edit_close: gtk::Button,
        search_close: gtk::Button,
        selection_copy: gtk::Button,
        selection_forward: gtk::Button,
        selection_cancel: gtk::Button,
    ) {
        {
            let action = self.inner.action.clone();
            self.inner.attach.connect_clicked(move |_| {
                if let Some(callback) = action.borrow().as_ref().cloned() {
                    callback(MessageAction::Attach);
                }
            });
        }
        {
            let inner = self.inner.clone();
            self.inner.emoji.connect_clicked(move |button| {
                inner.composer_popover.dismiss();
                let chooser = gtk::EmojiChooser::new();
                let buffer = inner.composer.buffer();
                chooser.connect_emoji_picked(move |_, emoji| {
                    buffer.insert_at_cursor(emoji);
                });
                inner
                    .composer_popover
                    .show(button, chooser.upcast::<gtk::Popover>());
            });
        }
        {
            let action = self.inner.action.clone();
            self.inner.sticker.connect_clicked(move |_| {
                if let Some(callback) = action.borrow().as_ref().cloned() {
                    callback(MessageAction::Stickers);
                }
            });
        }
        for (button, event) in [
            (selection_copy, MessageAction::SelectionCopy),
            (selection_forward, MessageAction::SelectionForward),
            (
                self.inner.selection_delete.clone(),
                MessageAction::SelectionDelete,
            ),
            (selection_cancel, MessageAction::SelectionCancel),
        ] {
            let action = self.inner.action.clone();
            button.connect_clicked(move |_| {
                if let Some(callback) = action.borrow().as_ref().cloned() {
                    callback(event.clone());
                }
            });
        }
        {
            let action = self.inner.action.clone();
            self.inner.recorder.set_action(Rc::new(move |event| {
                let event = match event {
                    RecorderUiAction::Cancel => MessageAction::RecorderCancel,
                    RecorderUiAction::Send => MessageAction::RecorderSend,
                    RecorderUiAction::Retry => MessageAction::RecorderRetry,
                };
                if let Some(callback) = action.borrow().as_ref().cloned() {
                    callback(event);
                }
            }));
        }
        for button in [reply_close, edit_close] {
            let action = self.inner.action.clone();
            button.connect_clicked(move |_| {
                if let Some(callback) = action.borrow().as_ref().cloned() {
                    callback(MessageAction::CancelMode);
                }
            });
        }
        {
            let action = self.inner.action.clone();
            let composer = self.inner.composer.clone();
            self.inner.send.connect_clicked(move |_| {
                if let Some(callback) = action.borrow().as_ref().cloned() {
                    let buffer = composer.buffer();
                    let empty = buffer
                        .text(&buffer.start_iter(), &buffer.end_iter(), true)
                        .is_empty();
                    callback(if empty {
                        MessageAction::Mic
                    } else {
                        MessageAction::Submit
                    });
                }
            });
        }

        {
            let action = self.inner.action.clone();
            self.inner.header_search.connect_clicked(move |_| {
                if let Some(callback) = action.borrow().as_ref().cloned() {
                    callback(MessageAction::Header(ChatAction::Search));
                }
            });
        }
        {
            let action = self.inner.action.clone();
            self.inner
                .search_entry
                .connect_search_changed(move |entry| {
                    if let Some(callback) = action.borrow().as_ref().cloned() {
                        callback(MessageAction::SearchChanged(entry.text().to_string()));
                    }
                });
        }
        for (button, message_action) in [
            (
                self.inner.search_previous.clone(),
                MessageAction::SearchPrevious,
            ),
            (self.inner.search_next.clone(), MessageAction::SearchNext),
            (self.inner.search_older.clone(), MessageAction::SearchOlder),
            (self.inner.search_retry.clone(), MessageAction::SearchRetry),
            (search_close, MessageAction::SearchClose),
        ] {
            let action = self.inner.action.clone();
            button.connect_clicked(move |_| {
                if let Some(callback) = action.borrow().as_ref().cloned() {
                    callback(message_action.clone());
                }
            });
        }
        {
            let action = self.inner.action.clone();
            self.inner.header_info.connect_clicked(move |_| {
                if let Some(callback) = action.borrow().as_ref().cloned() {
                    callback(MessageAction::Header(ChatAction::Info));
                }
            });
        }
        {
            let action = self.inner.action.clone();
            self.inner.draft_retry.connect_clicked(move |_| {
                if let Some(callback) = action.borrow().as_ref().cloned() {
                    callback(MessageAction::DraftRetry);
                }
            });
        }
        {
            let action = self.inner.action.clone();
            self.inner.operation_retry.connect_clicked(move |_| {
                if let Some(callback) = action.borrow().as_ref().cloned() {
                    callback(MessageAction::RetryReaction);
                }
            });
        }
        {
            let inner = self.inner.clone();
            self.inner.header_more.connect_clicked(move |button| {
                MessagesView::show_header_menu(&inner, button);
            });
        }
        {
            let inner = self.inner.clone();
            let click = gtk::GestureClick::new();
            click.connect_released(move |_, _, _, _| {
                if let Some(message) = inner.pinned_message.borrow().as_ref() {
                    if let Some(callback) = inner.action.borrow().as_ref().cloned() {
                        callback(MessageAction::JumpToMessage(message.id));
                    }
                }
            });
            self.inner.pinned_bar.add_controller(click);
        }
        {
            let inner = self.inner.clone();
            self.inner.pinned_more.connect_clicked(move |button| {
                let Some(msg_id) = inner
                    .pinned_message
                    .borrow()
                    .as_ref()
                    .map(|message| message.id)
                else {
                    return;
                };
                let (popover, contents) = menus::popover();
                let unpin = menus::button("Unpin", false);
                let action = inner.action.clone();
                let popover_weak = popover.downgrade();
                unpin.connect_clicked(move |_| {
                    if let Some(popover) = popover_weak.upgrade() {
                        popover.popdown();
                    }
                    if let Some(callback) = action.borrow().as_ref().cloned() {
                        callback(MessageAction::UnpinMessage(msg_id));
                    }
                });
                contents.append(&unpin);
                inner.pinned_popover.show(button, popover);
            });
        }
        {
            let action = self.inner.action.clone();
            self.inner.pinned_retry.connect_clicked(move |_| {
                if let Some(callback) = action.borrow().as_ref().cloned() {
                    callback(MessageAction::RetryPinned);
                }
            });
        }

        {
            let inner = self.inner.clone();
            let effects = self.inner.effects.clone();
            let cursor = self.inner.composer_cursor.clone();
            let equalizer = self.inner.equalizer.clone();
            self.inner.composer.buffer().connect_changed(move |buffer| {
                let empty = buffer
                    .text(&buffer.start_iter(), &buffer.end_iter(), true)
                    .is_empty();
                effects.composer_idle(cursor.upcast_ref(), empty);
                effects.composer_typing(&equalizer, !empty);
                inner.composer_placeholder.set_visible(empty);
                inner
                    .send_label
                    .set_label(if empty { icons::MIC } else { icons::SEND });
                inner
                    .send
                    .set_tooltip_text(Some(if empty { "Voice message" } else { "Send" }));
                if empty {
                    inner.send.remove_css_class("omg-primary");
                    inner.send.add_css_class("omg-icon-button");
                } else {
                    inner.send.remove_css_class("omg-icon-button");
                    inner.send.add_css_class("omg-primary");
                }
                if !inner.composer_signal_blocked.get() {
                    if let Some(callback) = inner.action.borrow().as_ref().cloned() {
                        callback(MessageAction::DraftChanged);
                    }
                }
            });
        }

        {
            let inner = self.inner.clone();
            self.inner.drop_target.connect_drop(move |_, value, _, _| {
                if inner.busy.get() {
                    return false;
                }
                let Ok(files) = value.get::<gdk::FileList>() else {
                    return false;
                };
                let Some(file) = files.files().into_iter().next() else {
                    return false;
                };
                if let Some(callback) = inner.action.borrow().as_ref().cloned() {
                    callback(MessageAction::DropFile(file));
                    true
                } else {
                    false
                }
            });
        }

        {
            let inner = self.inner.clone();
            let adjustment = self.inner.scroll.vadjustment();
            adjustment.connect_value_changed(move |adjustment| {
                if inner.suppress_paging.get() {
                    return;
                }
                inner
                    .stick_to_bottom
                    .set(adjustment.value() >= adjustment.upper() - adjustment.page_size() - 4.0);
                if inner.stick_to_bottom.get() {
                    inner.bottom_unread.set(0);
                    inner.bottom_badge.set_visible(false);
                }
                inner.bottom_button.set_visible(
                    inner.detached.get()
                        || adjustment.upper() - adjustment.page_size() - adjustment.value()
                            > adjustment.page_size(),
                );
                update_date_chip_label(&inner, adjustment);
                inner.effects.date_chip(inner.date_chip.upcast_ref(), true);
                if let Some(source) = inner.date_timeout.borrow_mut().take() {
                    remove_source_if_present(source);
                }
                let weak = Rc::downgrade(&inner);
                let source = glib::timeout_add_local_once(
                    std::time::Duration::from_millis(900),
                    move || {
                        if let Some(inner) = weak.upgrade() {
                            inner.date_timeout.borrow_mut().take();
                            inner.effects.date_chip(inner.date_chip.upcast_ref(), false);
                        }
                    },
                );
                *inner.date_timeout.borrow_mut() = Some(source);
                if adjustment.value() <= 4.0 && !inner.paging.get() && !inner.exhausted.get() {
                    if let Some(callback) = inner.action.borrow().as_ref().cloned() {
                        callback(MessageAction::Paginate);
                    }
                }
            });
        }
    }

    pub fn set_action(&self, callback: Rc<dyn Fn(MessageAction)>) {
        *self.inner.action.borrow_mut() = Some(callback);
    }

    pub fn open_search(&self) {
        self.inner.search_bar.set_visible(true);
        self.inner.search_entry.grab_focus();
    }

    pub fn close_search(&self) {
        self.move_focus_before_removal(&self.inner.search_bar);
        self.inner.search_entry.set_text("");
        self.inner.search_bar.set_visible(false);
        self.clear_search_highlights();
        self.inner.search_retry.set_visible(false);
    }

    pub fn search_is_open(&self) -> bool {
        self.inner.search_bar.is_visible()
    }

    pub fn search_text(&self) -> String {
        self.inner.search_entry.text().to_string()
    }

    pub fn search_position_text(&self) -> String {
        self.inner.search_position.label().to_string()
    }

    pub fn search_retry_visible(&self) -> bool {
        self.inner.search_retry.is_visible()
    }

    pub fn trigger_search_retry(&self) {
        self.inner.search_retry.emit_clicked();
    }

    pub fn set_search_text(&self, text: &str) {
        self.inner.search_entry.set_text(text);
    }

    pub fn set_search_loading(&self, loading: bool) {
        self.inner.search_position.remove_css_class("omg-error");
        self.inner.search_position.set_label(if loading {
            "Searching…"
        } else {
            "No results"
        });
        self.inner.search_retry.set_visible(false);
        self.inner.search_older.set_sensitive(!loading);
    }

    pub fn set_search_results(&self, ids: &[i32], index: Option<usize>, has_older: bool) {
        self.inner.search_entry.set_sensitive(true);
        self.inner.search_retry.set_visible(false);
        self.inner
            .search_position
            .set_label(&search_position(index, ids.len()));
        self.inner
            .search_previous
            .set_sensitive(index.is_some_and(|index| index > 0));
        self.inner
            .search_next
            .set_sensitive(index.is_some_and(|index| index + 1 < ids.len()));
        self.inner.search_older.set_visible(has_older);
        self.inner.search_older.set_sensitive(true);
        self.clear_search_highlights();
        let store = self.inner.store.borrow();
        for (position, id) in ids.iter().enumerate() {
            if let Some(entry) = store.entries.get(id) {
                entry.row.widget.add_css_class("omg-hit");
                if Some(position) == index {
                    entry.row.widget.add_css_class("omg-hit-active");
                }
            }
        }
    }

    pub fn fail_search(&self, message: &str, has_results: bool) {
        if !has_results {
            self.clear_search_highlights();
        }
        self.inner.search_position.set_label(message);
        self.inner.search_position.add_css_class("omg-error");
        self.inner.search_retry.set_visible(true);
        self.inner.search_older.set_sensitive(false);
    }

    fn clear_search_highlights(&self) {
        for entry in self.inner.store.borrow().entries.values() {
            entry.row.widget.remove_css_class("omg-hit");
            entry.row.widget.remove_css_class("omg-hit-active");
        }
        self.inner.search_position.remove_css_class("omg-error");
    }

    pub fn set_pinned_message(&self, message: Option<Msg>) {
        self.move_focus_before_removal(&self.inner.pinned_bar);
        self.inner.pinned_popover.dismiss();
        self.inner.pinned_retry.set_visible(false);
        self.inner.pinned_more.set_visible(message.is_some());
        self.inner.pinned_text.remove_css_class("omg-error");
        *self.inner.pinned_message.borrow_mut() = message.clone();
        if let Some(message) = message {
            let preview = if message.text.is_empty() {
                media_title(&message)
            } else {
                message.text.clone()
            };
            self.inner.pinned_text.set_label(&snippet(&preview, 90));
            self.inner.pinned_bar.set_visible(true);
        } else {
            self.inner.pinned_text.set_label("");
            self.inner.pinned_bar.set_visible(false);
        }
    }

    pub fn begin_pinned(&self) {
        self.move_focus_before_removal(&self.inner.pinned_bar);
        self.inner.pinned_popover.dismiss();
        self.inner.pinned_message.borrow_mut().take();
        self.inner.pinned_text.remove_css_class("omg-error");
        self.inner.pinned_text.set_label("Loading…");
        self.inner.pinned_more.set_visible(false);
        self.inner.pinned_retry.set_visible(false);
        self.inner.pinned_bar.set_visible(true);
    }

    pub fn fail_pinned(&self, error: &str) {
        self.inner.pinned_message.borrow_mut().take();
        self.inner.pinned_text.add_css_class("omg-error");
        self.inner.pinned_text.set_label(error);
        self.inner.pinned_more.set_visible(false);
        self.inner.pinned_retry.set_visible(true);
        self.inner.pinned_bar.set_visible(true);
    }

    pub fn pinned_id(&self) -> Option<i32> {
        self.inner
            .pinned_message
            .borrow()
            .as_ref()
            .map(|message| message.id)
    }

    pub fn pinned_bar_visible(&self) -> bool {
        self.inner.pinned_bar.is_visible()
    }

    pub fn pinned_retry_visible(&self) -> bool {
        self.inner.pinned_retry.is_visible()
    }

    pub fn trigger_pinned_retry(&self) {
        self.inner.pinned_retry.emit_clicked();
    }

    pub fn trigger_pinned(&self) {
        if let Some(message) = self.inner.pinned_message.borrow().as_ref() {
            if let Some(callback) = self.inner.action.borrow().as_ref().cloned() {
                callback(MessageAction::JumpToMessage(message.id));
            }
        }
    }

    pub fn set_available_reactions(&self, reactions: Vec<String>) {
        *self.inner.available_reactions.borrow_mut() = reactions;
        self.inner.available_reactions_loading.set(false);
        self.inner.available_reactions_error.borrow_mut().take();
    }

    pub fn begin_available_reactions(&self) {
        self.inner.available_reactions_loading.set(true);
        self.inner.available_reactions_error.borrow_mut().take();
    }

    pub fn fail_available_reactions(&self, error: String) {
        self.inner.available_reactions_loading.set(false);
        *self.inner.available_reactions_error.borrow_mut() = Some(error);
    }

    pub fn optimistic_reaction(&self, msg_id: i32, emoji: &str) -> bool {
        let message = {
            let store = self.inner.store.borrow();
            let Some(entry) = store.entries.get(&msg_id) else {
                return false;
            };
            let mut message = entry.msg.clone();
            let remove = message
                .reactions
                .iter()
                .any(|reaction| reaction.emoji == emoji && reaction.chosen);
            for reaction in message
                .reactions
                .iter_mut()
                .filter(|reaction| reaction.chosen)
            {
                reaction.chosen = false;
                reaction.count -= 1;
            }
            message.reactions.retain(|reaction| reaction.count > 0);
            if !remove {
                match message
                    .reactions
                    .iter_mut()
                    .find(|reaction| reaction.emoji == emoji)
                {
                    Some(reaction) => {
                        reaction.count += 1;
                        reaction.chosen = true;
                    }
                    None => message.reactions.push(crate::tg::Reaction {
                        emoji: emoji.to_string(),
                        count: 1,
                        chosen: true,
                    }),
                }
            }
            message
        };
        self.update_existing(message, true);
        true
    }

    pub fn reveal_spoiler(&self, msg_id: i32, start: usize, end: usize) -> bool {
        let (message, label, revealed) = {
            let store = self.inner.store.borrow();
            let Some(entry) = store.entries.get(&msg_id) else {
                return false;
            };
            let Some(label) = entry.row.text.borrow().as_ref().cloned() else {
                return false;
            };
            (
                entry.msg.clone(),
                label,
                entry.row.revealed_spoilers.clone(),
            )
        };
        revealed.borrow_mut().insert((start, end));
        apply_message_markup(
            &label,
            &message,
            &revealed,
            &self.inner.darker_background.borrow(),
        );
        true
    }

    fn cancel_pending_scroll(&self) {
        let adjustment = self.inner.scroll.vadjustment();
        if let Some(handler) = self.inner.upper_handler.borrow_mut().take() {
            adjustment.disconnect(handler);
        }
        if let Some(tick) = self.inner.upper_tick.borrow_mut().take() {
            tick.remove();
        }
    }

    fn clear_rows(&self) {
        self.move_focus_before_removal(&self.inner.list);
        let rows: Vec<MessageRow> = self
            .inner
            .store
            .borrow()
            .entries
            .values()
            .map(|entry| entry.row.clone())
            .collect();
        for row in rows {
            for source in row.animation_sources.borrow_mut().drain(..) {
                remove_source_if_present(source);
            }
            if let Some(source) = row.media_loading_source.borrow_mut().take() {
                remove_source_if_present(source);
            }
        }
        while let Some(child) = self.inner.list.first_child() {
            self.inner.list.remove(&child);
        }
        self.inner.day_separators.borrow_mut().clear();
    }

    fn after_upper_change<F>(&self, saved_upper: f64, callback: F)
    where
        F: FnOnce(&gtk::Adjustment, bool) + 'static,
    {
        self.cancel_pending_scroll();
        let adjustment = self.inner.scroll.vadjustment();
        let epoch = self.inner.scroll_epoch.get();
        if (adjustment.upper() - saved_upper).abs() > f64::EPSILON {
            callback(&adjustment, true);
            return;
        }

        let callback = Rc::new(RefCell::new(Some(callback)));
        let inner = self.inner.clone();
        let callback_for_signal = callback.clone();
        let handler = adjustment.connect_upper_notify(move |adjustment| {
            if inner.scroll_epoch.get() != epoch {
                return;
            }
            if (adjustment.upper() - saved_upper).abs() <= f64::EPSILON {
                return;
            }
            if let Some(handler) = inner.upper_handler.borrow_mut().take() {
                adjustment.disconnect(handler);
            }
            if let Some(tick) = inner.upper_tick.borrow_mut().take() {
                tick.remove();
            }
            if let Some(callback) = callback_for_signal.borrow_mut().take() {
                callback(adjustment, true);
            }
        });
        *self.inner.upper_handler.borrow_mut() = Some(handler);

        let adjustment_for_tick = adjustment.clone();
        let inner = self.inner.clone();
        let callback_for_tick = callback;
        let frames = Cell::new(0);
        let tick = self.inner.scroll.add_tick_callback(move |_, _| {
            if inner.scroll_epoch.get() != epoch {
                if let Some(handler) = inner.upper_handler.borrow_mut().take() {
                    adjustment_for_tick.disconnect(handler);
                }
                inner.upper_tick.borrow_mut().take();
                callback_for_tick.borrow_mut().take();
                return glib::ControlFlow::Break;
            }
            let changed = (adjustment_for_tick.upper() - saved_upper).abs() > f64::EPSILON;
            frames.set(frames.get() + 1);
            if !changed && frames.get() < 3 {
                return glib::ControlFlow::Continue;
            }
            if let Some(handler) = inner.upper_handler.borrow_mut().take() {
                adjustment_for_tick.disconnect(handler);
            }
            inner.upper_tick.borrow_mut().take();
            if let Some(callback) = callback_for_tick.borrow_mut().take() {
                callback(&adjustment_for_tick, changed);
            }
            glib::ControlFlow::Break
        });
        *self.inner.upper_tick.borrow_mut() = Some(tick);
    }

    fn after_next_upper_or_tick<F>(&self, callback: F)
    where
        F: FnOnce(&gtk::Adjustment) + 'static,
    {
        self.cancel_pending_scroll();
        let adjustment = self.inner.scroll.vadjustment();
        let epoch = self.inner.scroll_epoch.get();
        let callback = Rc::new(RefCell::new(Some(callback)));
        let inner = self.inner.clone();
        let callback_for_signal = callback.clone();
        let handler = adjustment.connect_upper_notify(move |adjustment| {
            if inner.scroll_epoch.get() != epoch {
                return;
            }
            if let Some(handler) = inner.upper_handler.borrow_mut().take() {
                adjustment.disconnect(handler);
            }
            if let Some(tick) = inner.upper_tick.borrow_mut().take() {
                tick.remove();
            }
            if let Some(callback) = callback_for_signal.borrow_mut().take() {
                callback(adjustment);
            }
        });
        *self.inner.upper_handler.borrow_mut() = Some(handler);

        let adjustment_for_tick = adjustment.clone();
        let inner = self.inner.clone();
        let tick = self.inner.scroll.add_tick_callback(move |_, _| {
            if let Some(handler) = inner.upper_handler.borrow_mut().take() {
                adjustment_for_tick.disconnect(handler);
            }
            inner.upper_tick.borrow_mut().take();
            if inner.scroll_epoch.get() == epoch {
                if let Some(callback) = callback.borrow_mut().take() {
                    callback(&adjustment_for_tick);
                }
            } else {
                callback.borrow_mut().take();
            }
            glib::ControlFlow::Break
        });
        *self.inner.upper_tick.borrow_mut() = Some(tick);
    }

    pub fn reset_chat(&self, chat_id: i64, title: &str, epoch: u64) {
        self.exit_selection_mode();
        self.hide_recorder();
        self.move_focus_before_removal(&self.widget);
        self.clear_recent_presence();
        self.inner.virtual_mode.set(is_virtual(chat_id));
        let composer_enabled = self.inner.virtual_mode.get() || !self.inner.busy.get();
        self.inner.composer.set_sensitive(composer_enabled);
        self.inner.attach.set_sensitive(composer_enabled);
        self.inner.emoji.set_sensitive(composer_enabled);
        self.inner.sticker.set_sensitive(composer_enabled);
        self.inner.send.set_sensitive(composer_enabled);
        self.cancel_pending_scroll();
        self.inner.scroll_epoch.set(epoch);
        self.inner.edit.borrow_mut().take();
        self.inner.ai_draft.set(false);
        self.inner.edit_bar.set_visible(false);
        self.inner.edit_label.set_label("Editing message");
        self.cancel_reply();
        self.set_composer_text("");
        self.clear_error();
        self.clear_typing();
        self.dismiss_row_popovers();
        self.inner.header_popover.dismiss();
        self.inner.composer_popover.dismiss();
        self.inner.reaction_popover.dismiss();
        self.set_pinned_message(None);
        self.inner.media_ready.borrow_mut().clear();
        self.inner.quote_cache.borrow_mut().clear();
        self.inner.pending_messages.borrow_mut().clear();
        self.inner.read_outbox.set(0);
        self.clear_rows();
        *self.inner.store.borrow_mut() = MessageStore {
            chat_id: Some(chat_id),
            ..MessageStore::default()
        };
        self.inner.header_title.set_label(title);
        self.inner.header_actions.set_visible(true);
        self.inner.header_summary.borrow_mut().take();
        self.inner.base_status.borrow_mut().clear();
        self.inner.typing.set_label("");
        self.inner.loading.set_label("Loading…");
        self.inner.loading.set_visible(true);
        self.inner.paging_spinner.start();
        self.inner.paging_spinner.set_visible(true);
        self.set_detached(false);
        self.inner.paging.set(false);
        self.inner.exhausted.set(false);
        self.inner.suppress_paging.set(true);
        self.inner.stick_to_bottom.set(true);
        self.inner.unread_divider_shown.set(false);
        self.inner
            .effects
            .empty_state(&self.inner.empty_effects, false);
    }

    pub fn clear_selection(&self, epoch: u64) {
        self.exit_selection_mode();
        self.hide_recorder();
        self.move_focus_before_removal(&self.widget);
        self.clear_recent_presence();
        self.cancel_pending_scroll();
        self.inner.scroll_epoch.set(epoch);
        self.inner.virtual_mode.set(false);
        self.inner.edit.borrow_mut().take();
        self.inner.ai_draft.set(false);
        self.inner.edit_bar.set_visible(false);
        self.cancel_reply();
        self.set_composer_text("");
        self.clear_error();
        self.clear_typing();
        self.dismiss_row_popovers();
        self.inner.header_popover.dismiss();
        self.inner.composer_popover.dismiss();
        self.inner.reaction_popover.dismiss();
        self.set_pinned_message(None);
        self.inner.media_ready.borrow_mut().clear();
        self.inner.quote_cache.borrow_mut().clear();
        self.inner.pending_messages.borrow_mut().clear();
        self.inner.read_outbox.set(0);
        self.clear_rows();
        *self.inner.store.borrow_mut() = MessageStore::default();
        self.inner.header_title.set_label("Select a chat");
        self.inner.header_actions.set_visible(false);
        self.inner.header_summary.borrow_mut().take();
        self.inner.base_status.borrow_mut().clear();
        self.inner.loading.set_label("Select a chat");
        self.inner.loading.set_visible(true);
        self.inner.paging_spinner.stop();
        self.inner.paging_spinner.set_visible(false);
        self.inner.composer.set_sensitive(false);
        self.inner.attach.set_sensitive(false);
        self.inner.emoji.set_sensitive(false);
        self.inner.sticker.set_sensitive(false);
        self.inner.send.set_sensitive(false);
        self.set_detached(false);
        self.inner.paging.set(false);
        self.inner.exhausted.set(false);
        self.inner.suppress_paging.set(true);
        self.inner.unread_divider_shown.set(false);
        self.inner
            .effects
            .empty_state(&self.inner.empty_effects, true);
    }

    /// History-only reset for jump-to-date / jump-to-latest on the SAME chat:
    /// clears the store/paging/scroll state and shows the loading label, but
    /// preserves the composer draft, reply/edit mode, and busy sensitivity
    /// (C5/C13). `detached` is left to the caller.
    pub fn reset_history(&self, chat_id: i64, epoch: u64) {
        self.move_focus_before_removal(&self.widget);
        self.cancel_pending_scroll();
        self.inner.scroll_epoch.set(epoch);
        self.clear_error();
        self.clear_typing();
        self.dismiss_row_popovers();
        self.inner.header_popover.dismiss();
        self.inner.media_ready.borrow_mut().clear();
        self.inner.quote_cache.borrow_mut().clear();
        self.inner.pending_messages.borrow_mut().clear();
        self.clear_rows();
        *self.inner.store.borrow_mut() = MessageStore {
            chat_id: Some(chat_id),
            ..MessageStore::default()
        };
        self.inner.loading.set_label("Loading…");
        self.inner.loading.set_visible(true);
        self.inner.paging_spinner.start();
        self.inner.paging_spinner.set_visible(true);
        self.inner.paging.set(false);
        self.inner.exhausted.set(false);
        self.inner.suppress_paging.set(true);
        self.inner.stick_to_bottom.set(true);
        self.inner.unread_divider_shown.set(false);
        self.inner
            .effects
            .empty_state(&self.inner.empty_effects, false);
    }

    pub fn finish_initial(&self, messages: Vec<Msg>) -> Vec<i32> {
        self.inner
            .initial_render_count
            .set(self.inner.initial_render_count.get().wrapping_add(1));
        let adjustment = self.inner.scroll.vadjustment();
        let inserted = self.merge(messages, false, false);
        self.refresh_reply_bar();
        self.inner.paging_spinner.stop();
        self.inner.paging_spinner.set_visible(false);
        if self.inner.store.borrow().order.is_empty() {
            self.inner.loading.set_label("No messages yet");
            self.inner.loading.set_visible(true);
        } else {
            self.inner.loading.set_visible(false);
        }
        self.inner
            .effects
            .empty_state(&self.inner.empty_effects, false);
        adjustment.set_value((adjustment.upper() - adjustment.page_size()).max(0.0));
        let inner = self.inner.clone();
        self.after_next_upper_or_tick(move |adjustment| {
            adjustment.set_value((adjustment.upper() - adjustment.page_size()).max(0.0));
            inner.stick_to_bottom.set(true);
            inner.suppress_paging.set(false);
        });
        inserted
    }

    pub fn merge_event(&self, message: Msg) -> Vec<i32> {
        // Detached (jumped-to historical page): merge into the store but never
        // auto-scroll; the ▼ button stays visible (C4).
        let should_stick = self.inner.stick_to_bottom.get() && !self.inner.detached.get();
        let adjustment = self.inner.scroll.vadjustment();
        let saved_upper = adjustment.upper();
        if should_stick {
            self.inner.suppress_paging.set(true);
        }
        let incoming = !message.outgoing;
        let show_unread_divider = incoming
            && !should_stick
            && self.inner.effects.on("unreaddivider")
            && !self.inner.unread_divider_shown.replace(true);
        let inserted = self.merge(vec![message], true, show_unread_divider);
        if incoming && !should_stick && !inserted.is_empty() {
            let unread = self.inner.bottom_unread.get().saturating_add(1);
            self.inner.bottom_unread.set(unread);
            self.inner.bottom_badge.set_label(&unread.to_string());
            self.inner.bottom_badge.set_visible(true);
        }
        if should_stick {
            let inner = self.inner.clone();
            self.after_upper_change(saved_upper, move |adjustment, changed| {
                if changed {
                    adjustment.set_value((adjustment.upper() - adjustment.page_size()).max(0.0));
                }
                inner.stick_to_bottom.set(true);
                inner.suppress_paging.set(false);
            });
        }
        inserted
    }

    pub fn merge_pending(&self, message: Msg) -> Vec<i32> {
        self.inner.pending_messages.borrow_mut().insert(message.id);
        self.merge_event(message)
    }

    pub fn begin_page(&self) -> Option<i32> {
        if self.inner.loading.is_visible()
            || self.inner.paging.get()
            || self.inner.exhausted.get()
            || self.inner.suppress_paging.get()
        {
            return None;
        }
        let oldest = self.inner.store.borrow().order.first().copied()?;
        self.inner.paging.set(true);
        self.inner.paging_spinner.start();
        self.inner.paging_spinner.set_visible(true);
        Some(oldest)
    }

    pub fn finish_page(&self, messages: Vec<Msg>) -> Vec<i32> {
        let adjustment = self.inner.scroll.vadjustment();
        let saved_upper = adjustment.upper();
        let saved_value = adjustment.value();
        self.inner.suppress_paging.set(true);
        let inserted = self.merge(messages, false, false);
        self.inner.paging.set(false);
        self.inner.paging_spinner.stop();
        self.inner.paging_spinner.set_visible(false);
        if inserted.is_empty() {
            self.inner.exhausted.set(true);
            self.inner.suppress_paging.set(false);
            return inserted;
        }
        let inner = self.inner.clone();
        self.after_upper_change(saved_upper, move |adjustment, changed| {
            if changed {
                let restored = saved_value + adjustment.upper() - saved_upper;
                adjustment.set_value(restored);
            }
            inner.suppress_paging.set(false);
        });
        inserted
    }

    pub fn fail_page(&self, message: &str) {
        self.inner.paging.set(false);
        self.inner.paging_spinner.stop();
        self.inner.paging_spinner.set_visible(false);
        self.show_error(message);
    }

    pub fn fail_initial(&self, message: &str) {
        self.inner.loading.set_visible(false);
        self.inner.paging_spinner.stop();
        self.inner.paging_spinner.set_visible(false);
        self.inner.suppress_paging.set(false);
        self.show_error(message);
    }

    pub fn trigger_pagination(&self) {
        if let Some(callback) = self.inner.action.borrow().as_ref().cloned() {
            callback(MessageAction::Paginate);
        }
    }

    fn merge(&self, messages: Vec<Msg>, is_live: bool, show_unread_divider: bool) -> Vec<i32> {
        let mut inserted = Vec::new();
        for message in messages {
            if message.deleted {
                self.drop_selection_ids(&[message.id]);
            }
            let current_chat = self.inner.store.borrow().chat_id;
            if current_chat != Some(message.chat_id) {
                continue;
            }
            let existing = self.inner.store.borrow().entries.contains_key(&message.id);
            if existing {
                self.update_existing(message, is_live);
                continue;
            }
            let row = self.build_row(
                &message,
                is_live,
                show_unread_divider && inserted.is_empty(),
            );
            let media_state = MediaState::NotStarted;
            let id = message.id;
            self.inner.store.borrow_mut().entries.insert(
                id,
                MessageEntry {
                    msg: message,
                    row,
                    media_state,
                    media_retryable: false,
                },
            );
            self.inner.store.borrow_mut().order.push(id);
            inserted.push(id);
        }
        self.inner.store.borrow_mut().order.sort_unstable();
        self.reorder_rows();
        self.refresh_quotes();
        inserted
    }

    fn build_row(&self, message: &Msg, is_live: bool, show_unread_divider: bool) -> MessageRow {
        let row_layout = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        // Same alignment as the clamp, so the bubble sits at the end even if
        // a parent ever hands the clamp the full row width.
        row_layout.set_halign(if message.outgoing {
            gtk::Align::End
        } else {
            gtk::Align::Start
        });
        let widget = BubbleClamp::new(&row_layout, self.inner.pane_width.clone());
        widget.add_css_class("omg-msg");
        // The viewer returns keyboard focus to its originating message.  The
        // bubble wrapper therefore needs to be an explicit focus target.
        widget.set_focusable(true);
        widget.set_hexpand(false);
        widget.set_halign(if message.outgoing {
            gtk::Align::End
        } else {
            gtk::Align::Start
        });
        widget.add_css_class(if message.outgoing {
            "omg-msg-out"
        } else {
            "omg-msg-in"
        });

        let selection = gtk::CheckButton::new();
        selection.add_css_class("omg-message-check");
        selection.set_valign(gtk::Align::Center);
        selection.set_visible(self.inner.selection_mode.get() && !message.deleted);
        selection.set_sensitive(!message.deleted);
        selection.set_active(self.inner.selection.borrow().ids.contains(&message.id));
        row_layout.append(&selection);

        let bubble_column = gtk::Box::new(gtk::Orientation::Vertical, 4);
        bubble_column.set_halign(gtk::Align::Fill);
        bubble_column.set_hexpand(true);
        row_layout.append(&bubble_column);

        if show_unread_divider {
            let divider = gtk::Revealer::new();
            divider.set_transition_type(gtk::RevealerTransitionType::SlideRight);
            divider.set_transition_duration(400);
            let line = gtk::Label::new(Some("unread"));
            line.add_css_class("omg-unread-divider");
            divider.set_child(Some(&line));
            bubble_column.append(&divider);
            self.inner
                .effects
                .unread_divider_added(divider.upcast_ref());
            glib::idle_add_local_once(move || divider.set_reveal_child(true));
        }

        let content = gtk::Box::new(gtk::Orientation::Vertical, 4);
        content.add_css_class("omg-msg-content");
        bubble_column.append(&content);

        let forwarded = gtk::Label::new(
            message
                .forwarded_from
                .as_ref()
                .map(|name| format!("{} Forwarded from {name}", icons::FORWARD))
                .as_deref(),
        );
        forwarded.add_css_class("omg-forwarded");
        forwarded.set_halign(gtk::Align::Start);
        forwarded.set_visible(message.forwarded_from.is_some());
        content.append(&forwarded);

        let show_sender = self.inner.chat_kind.get() == ChatKind::Group
            && !message.outgoing
            && !message.sender.is_empty()
            && message.sender != "You";
        let sender = gtk::Label::new(Some(&message.sender));
        sender.add_css_class("omg-msg-sender");
        sender.add_css_class(&format!("omg-c{}", sender_color_index(message)));
        sender.set_halign(gtk::Align::Start);
        sender.set_visible(show_sender);
        sender.set_ellipsize(gtk::pango::EllipsizeMode::End);
        sender.set_max_width_chars(40);
        content.append(&sender);

        let quote = gtk::Label::new(None);
        quote.add_css_class("omg-msg-quote");
        quote.set_halign(gtk::Align::Start);
        quote.set_wrap(true);
        quote.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        quote.set_max_width_chars(60);
        quote.set_visible(message.reply_to.is_some());
        if let Some(reply_to) = message.reply_to {
            quote.set_cursor_from_name(Some("pointer"));
            let action = self.inner.action.clone();
            let click = gtk::GestureClick::new();
            click.connect_released(move |_, _, _, _| {
                if let Some(callback) = action.borrow().as_ref().cloned() {
                    callback(MessageAction::JumpToMessage(reply_to));
                }
            });
            quote.add_controller(click);
        }
        content.append(&quote);

        let media_slot = gtk::Box::new(gtk::Orientation::Vertical, 0);
        media_slot.set_halign(if message.outgoing {
            gtk::Align::End
        } else {
            gtk::Align::Start
        });
        content.append(&media_slot);

        let animation_sources = Rc::new(RefCell::new(Vec::new()));
        let media_loading_source: Rc<RefCell<Option<glib::SourceId>>> = Rc::new(RefCell::new(None));

        let mut media_button = None;
        let mut transcribe_button = None;
        match message.media {
            Some(MediaKind::Photo | MediaKind::Sticker) => {
                let placeholder = gtk::Label::new(Some("loading image…"));
                placeholder.add_css_class("omg-media-placeholder");
                media_slot.append(&placeholder);
                if let Some(source) = self.inner.effects.image_loading(&placeholder) {
                    *media_loading_source.borrow_mut() = Some(source);
                }
            }
            Some(
                MediaKind::Document
                | MediaKind::Voice
                | MediaKind::Video
                | MediaKind::Gif
                | MediaKind::Audio
                | MediaKind::VideoNote
                | MediaKind::Unsupported,
            ) => {
                let button = media_card(message);
                let action = self.inner.action.clone();
                let msg_id = message.id;
                button.connect_clicked(move |_| {
                    if let Some(callback) = action.borrow().as_ref().cloned() {
                        callback(MessageAction::Media(msg_id));
                    }
                });
                media_slot.append(&button);
                media_button = Some(button);
                if message.media == Some(MediaKind::Voice) {
                    let transcribe = gtk::Button::with_label("transcribe");
                    transcribe.add_css_class("omg-attach");
                    transcribe.set_halign(gtk::Align::Start);
                    transcribe.set_visible(self.inner.ai_enabled.get());
                    let action = self.inner.action.clone();
                    let msg_id = message.id;
                    transcribe.connect_clicked(move |_| {
                        if let Some(callback) = action.borrow().as_ref().cloned() {
                            callback(MessageAction::Transcribe(msg_id));
                        }
                    });
                    media_slot.append(&transcribe);
                    transcribe_button = Some(transcribe);
                }
            }
            None => {}
        }

        let revealed_spoilers = Rc::new(RefCell::new(HashSet::new()));
        let text_block = gtk::Box::new(gtk::Orientation::Vertical, 0);
        text_block.set_halign(gtk::Align::Fill);
        set_text_block_style(&text_block, message);
        let text = if message.text.is_empty() {
            None
        } else {
            let text = formatted_message_label(
                message,
                &revealed_spoilers,
                &self.inner.action,
                &self.inner.darker_background.borrow(),
            );
            text_block.append(&text);
            Some(text)
        };
        text_block.set_visible(text.is_some());
        content.append(&text_block);

        let web_preview = gtk::Box::new(gtk::Orientation::Vertical, 0);
        content.append(&web_preview);
        render_web_preview(&web_preview, message, &self.inner.action);

        let time = gtk::Label::new(None);
        time.add_css_class("omg-msg-time");
        set_time_label(&time, message, &self.inner.time_format.borrow());
        let deleted_tag = gtk::Label::new(Some("deleted"));
        deleted_tag.add_css_class("omg-msg-time");
        let meta = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        meta.set_halign(gtk::Align::End);
        meta.append(&time);
        if let Some(views) = message.views {
            let views = gtk::Label::new(Some(&format!("{} {views}", icons::EYE)));
            views.add_css_class("omg-msg-time");
            meta.append(&views);
        }
        let receipt = gtk::Label::new(Some(receipt_glyph(
            self.inner.pending_messages.borrow().contains(&message.id),
            message.id,
            self.inner.read_outbox.get(),
        )));
        receipt.add_css_class("omg-msg-time");
        receipt.set_visible(message.outgoing);
        meta.append(&receipt);
        if message.outgoing && is_live && self.inner.effects.on("receiptdraw") {
            if let Some(source) = self.inner.effects.receipt_drawn(receipt.upcast_ref()) {
                animation_sources.borrow_mut().push(source);
            }
        }
        meta.append(&deleted_tag);
        meta.add_css_class("omg-meta");
        content.append(&meta);

        let reactions = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        reactions.set_halign(gtk::Align::Start);
        content.append(&reactions);
        let reaction_buttons = Rc::new(RefCell::new(Vec::new()));
        let reaction_anchor = gtk::Button::with_label(icons::EMOJI);
        reaction_anchor.add_css_class("omg-reaction-anchor");
        reaction_anchor.set_tooltip_text(Some("React"));
        {
            let inner = Rc::downgrade(&self.inner);
            let anchor = reaction_anchor.clone();
            let msg_id = message.id;
            reaction_anchor.connect_clicked(move |_| {
                let Some(inner) = inner.upgrade() else { return };
                // Context-menu "…" activates this stable row-owned button.
                // Focus it before the old popover is unparented (A26).
                anchor.grab_focus();
                MessagesView::dismiss_popover(&inner.context_popover);
                let chooser = gtk::EmojiChooser::new();
                let action = inner.action.clone();
                chooser.connect_emoji_picked(move |_, emoji| {
                    if let Some(callback) = action.borrow().as_ref().cloned() {
                        callback(MessageAction::Reaction {
                            msg_id,
                            emoji: Some(emoji.to_string()),
                        });
                    }
                });
                *inner.probe_reaction_chooser.borrow_mut() = Some(chooser.clone());
                inner.reaction_popover.show(&anchor, chooser.upcast());
            });
        }
        reactions.append(&reaction_anchor);

        let aux_slot = gtk::Box::new(gtk::Orientation::Vertical, 4);
        content.append(&aux_slot);

        let gesture = gtk::GestureClick::new();
        gesture.set_button(3);
        {
            let inner = Rc::downgrade(&self.inner);
            let msg_id = message.id;
            gesture.connect_pressed(move |_, _, x, y| {
                if let Some(inner) = inner.upgrade() {
                    MessagesView::show_context_menu(&inner, msg_id, x, y);
                }
            });
        }
        widget.add_controller(gesture);

        {
            let inner = Rc::downgrade(&self.inner);
            let msg_id = message.id;
            selection.connect_toggled(move |check| {
                if let Some(inner) = inner.upgrade() {
                    MessagesView::set_selected_inner(&inner, msg_id, check.is_active());
                }
            });
        }
        {
            let inner = Rc::downgrade(&self.inner);
            let check = selection.clone();
            let click = gtk::GestureClick::new();
            click.set_button(1);
            click.set_propagation_phase(gtk::PropagationPhase::Capture);
            click.connect_pressed(move |gesture, _, _, _| {
                let Some(inner) = inner.upgrade() else { return };
                if !inner.selection_mode.get() || !check.is_sensitive() {
                    return;
                }
                check.set_active(!check.is_active());
                if gesture.current_sequence().is_some() {
                    gesture.set_state(gtk::EventSequenceState::Claimed);
                }
            });
            widget.add_controller(click);
        }

        let row = MessageRow {
            widget,
            selection,
            forwarded,
            sender,
            quote,
            text_block,
            text: Rc::new(RefCell::new(text)),
            revealed_spoilers,
            web_preview,
            time,
            deleted_tag,
            receipt,
            reactions,
            reaction_buttons,
            reaction_anchor,
            media_slot,
            media_button,
            transcribe_button,
            aux_slot,
            animation_sources,
            media_loading_source,
        };
        set_deleted_rendering(&row, message.deleted);
        row.selection.set_sensitive(!message.deleted);
        row.selection
            .set_visible(self.inner.selection_mode.get() && !message.deleted);
        update_reactions(
            &row,
            message,
            &self.inner.effects,
            &self.inner.action,
            is_live,
        );
        self.inner
            .effects
            .message_added(row.widget.upcast_ref(), message, is_live);
        if is_live && message.outgoing {
            self.inner.effects.message_sent(row.widget.upcast_ref());
        }
        row
    }

    fn update_existing(&self, message: Msg, is_live: bool) {
        let (row, was_edited, old_text, old_spans, old_webpage) = {
            let mut store = self.inner.store.borrow_mut();
            let Some(entry) = store.entries.get_mut(&message.id) else {
                return;
            };
            let was_edited = entry.msg.edited;
            let old_text = entry.msg.text.clone();
            let old_spans = entry.msg.spans.clone();
            let old_webpage = entry.msg.webpage.clone();
            entry.msg = message.clone();
            (
                entry.row.clone(),
                was_edited,
                old_text,
                old_spans,
                old_webpage,
            )
        };
        row.forwarded.set_label(
            &message
                .forwarded_from
                .as_ref()
                .map(|name| format!("{} Forwarded from {name}", icons::FORWARD))
                .unwrap_or_default(),
        );
        row.forwarded.set_visible(message.forwarded_from.is_some());
        row.sender.set_label(&message.sender);
        row.sender.set_visible(
            self.inner.chat_kind.get() == ChatKind::Group
                && !message.outgoing
                && !message.sender.is_empty()
                && message.sender != "You",
        );
        for index in 0..7 {
            row.sender.remove_css_class(&format!("omg-c{index}"));
        }
        row.sender
            .add_css_class(&format!("omg-c{}", sender_color_index(&message)));
        if old_text != message.text || old_spans != message.spans {
            row.revealed_spoilers.borrow_mut().clear();
        }
        let mut text_label = row.text.borrow_mut();
        if let Some(label) = text_label.as_ref() {
            label.remove_css_class("omg-code-animation");
        }
        match (text_label.as_ref(), message.text.is_empty()) {
            (Some(label), false) => apply_message_markup(
                label,
                &message,
                &row.revealed_spoilers,
                &self.inner.darker_background.borrow(),
            ),
            (Some(label), true) => {
                self.move_focus_before_removal(label);
                row.text_block.remove(label);
                row.text_block.set_visible(false);
                *text_label = None;
            }
            (None, false) => {
                let label = formatted_message_label(
                    &message,
                    &row.revealed_spoilers,
                    &self.inner.action,
                    &self.inner.darker_background.borrow(),
                );
                row.text_block.append(&label);
                row.text_block.set_visible(true);
                *text_label = Some(label);
            }
            (None, true) => {}
        }
        set_text_block_style(&row.text_block, &message);
        drop(text_label);
        if old_webpage != message.webpage {
            self.move_focus_before_removal(&row.web_preview);
        }
        render_web_preview(&row.web_preview, &message, &self.inner.action);
        set_time_label(&row.time, &message, &self.inner.time_format.borrow());
        row.receipt.set_visible(message.outgoing);
        if message.outgoing {
            row.receipt.set_label(receipt_glyph(
                self.inner.pending_messages.borrow().contains(&message.id),
                message.id,
                self.inner.read_outbox.get(),
            ));
        }
        set_deleted_rendering(&row, message.deleted);
        row.selection.set_sensitive(!message.deleted);
        row.selection
            .set_visible(self.inner.selection_mode.get() && !message.deleted);
        update_reactions(
            &row,
            &message,
            &self.inner.effects,
            &self.inner.action,
            is_live,
        );
        if message.edited && (!was_edited || old_text != message.text) {
            self.inner.effects.message_edited(row.widget.upcast_ref());
        }
    }

    fn reorder_rows(&self) {
        let rows: Vec<(Msg, BubbleClamp, gtk::Label)> = {
            let store = self.inner.store.borrow();
            store
                .order
                .iter()
                .filter_map(|id| {
                    store.entries.get(id).map(|entry| {
                        (
                            entry.msg.clone(),
                            entry.row.widget.clone(),
                            entry.row.sender.clone(),
                        )
                    })
                })
                .collect()
        };
        let used_dates: std::collections::HashSet<chrono::NaiveDate> = rows
            .iter()
            .map(|(message, _, _)| message.ts.date_naive())
            .collect();
        let unused = self
            .inner
            .day_separators
            .borrow()
            .iter()
            .filter_map(|(date, label)| {
                (!used_dates.contains(date)).then_some((*date, label.clone()))
            })
            .collect::<Vec<_>>();
        for (date, label) in unused {
            if label.parent().is_some() {
                self.inner.list.remove(&label);
            }
            self.inner.day_separators.borrow_mut().remove(&date);
        }
        let mut previous: Option<gtk::Widget> = None;
        let mut previous_message: Option<Msg> = None;
        for (message, row, sender) in rows {
            let date = message.ts.date_naive();
            let new_day = previous_message
                .as_ref()
                .is_none_or(|previous| previous.ts.date_naive() != date);
            if new_day {
                let separator = self
                    .inner
                    .day_separators
                    .borrow_mut()
                    .entry(date)
                    .or_insert_with(|| {
                        let label = gtk::Label::new(Some(&day_separator_label_at(
                            date,
                            Local::now().date_naive(),
                        )));
                        label.add_css_class("omg-date-separator");
                        label.set_halign(gtk::Align::Center);
                        label
                    })
                    .clone();
                if separator.parent().is_none() {
                    self.inner.list.append(&separator);
                }
                self.inner
                    .list
                    .reorder_child_after(&separator, previous.as_ref());
                previous = Some(separator.upcast());
            }
            let same_sender = previous_message.as_ref().is_some_and(|previous| {
                previous.ts.date_naive() == date
                    && previous.outgoing == message.outgoing
                    && sender_identity(previous) == sender_identity(&message)
                    && message
                        .ts
                        .signed_duration_since(previous.ts)
                        .num_minutes()
                        .abs()
                        <= 5
            });
            let can_show_sender = self.inner.chat_kind.get() == ChatKind::Group
                && !message.outgoing
                && message.sender != "You"
                && !message.sender.is_empty();
            sender.set_visible(can_show_sender && !same_sender);
            row.set_margin_top(if previous_message.is_some() && same_sender {
                4
            } else if previous_message.is_some() {
                12
            } else {
                0
            });
            if row.parent().is_none() {
                self.inner.list.append(&row);
            }
            self.inner.list.reorder_child_after(&row, previous.as_ref());
            previous = Some(row.clone().upcast());
            previous_message = Some(message);
        }
    }

    fn refresh_quotes(&self) {
        let (quoted, rows) = {
            let store = self.inner.store.borrow();
            let mut quoted = self.inner.quote_cache.borrow().clone();
            quoted.extend(
                store
                    .entries
                    .iter()
                    .map(|(&id, entry)| (id, (entry.msg.sender.clone(), entry.msg.text.clone()))),
            );
            let rows: Vec<(Option<i32>, gtk::Label)> = store
                .entries
                .values()
                .map(|entry| (entry.msg.reply_to, entry.row.quote.clone()))
                .collect();
            (quoted, rows)
        };
        for (reply_to, quote) in rows {
            let Some(reply_to) = reply_to else {
                quote.set_visible(false);
                continue;
            };
            let label = quoted
                .get(&reply_to)
                .map(|(sender, text)| {
                    let sender = if sender.is_empty() { "Unknown" } else { sender };
                    format!("{}: {}", sender, snippet(text, 60))
                })
                .unwrap_or_else(|| "replied message".to_string());
            quote.set_label(&label);
            quote.set_visible(true);
        }
    }

    pub fn missing_reply_ids(&self) -> Vec<i32> {
        let store = self.inner.store.borrow();
        let cache = self.inner.quote_cache.borrow();
        let mut missing = store
            .entries
            .values()
            .filter_map(|entry| entry.msg.reply_to)
            .filter(|id| !store.entries.contains_key(id) && !cache.contains_key(id))
            .collect::<Vec<_>>();
        missing.sort_unstable();
        missing.dedup();
        missing
    }

    pub fn fill_quote_messages(&self, messages: Vec<Msg>) {
        let mut cache = self.inner.quote_cache.borrow_mut();
        for message in messages {
            cache.insert(message.id, (message.sender, message.text));
        }
        drop(cache);
        self.refresh_quotes();
        self.refresh_reply_bar();
    }

    fn refresh_reply_bar(&self) {
        let Some(reply_to) = self.inner.reply_to.get() else {
            return;
        };
        let reply = self
            .message(reply_to)
            .map(|message| (message.sender, message.text))
            .or_else(|| self.inner.quote_cache.borrow().get(&reply_to).cloned());
        let Some((sender, text)) = reply else {
            return;
        };
        let sender = if sender.is_empty() {
            "Unknown"
        } else {
            &sender
        };
        self.inner
            .reply_label
            .set_label(&format!("Reply to {sender}: {}", snippet(&text, 60)));
        self.inner.reply_bar.set_visible(true);
    }

    fn show_context_menu(inner: &Rc<MessagesInner>, msg_id: i32, x: f64, y: f64) {
        if inner.pending_messages.borrow().contains(&msg_id) {
            return;
        }
        let snapshot = inner
            .store
            .borrow()
            .entries
            .get(&msg_id)
            .map(|entry| entry.msg.clone());
        let Some(message) = snapshot else { return };
        let row = inner
            .store
            .borrow()
            .entries
            .get(&msg_id)
            .map(|entry| entry.row.widget.clone());
        let Some(row) = row else { return };

        row.grab_focus();
        Self::dismiss_popover(&inner.context_popover);
        let popover = gtk::Popover::new();
        popover.add_css_class("omg-menu");
        popover.set_has_arrow(false);
        popover.set_parent(&row);
        popover.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        let menu = gtk::Box::new(gtk::Orientation::Vertical, 0);
        popover.set_child(Some(&menu));
        popover.connect_closed(|popover| {
            if popover.parent().is_some() {
                popover.unparent();
            }
        });
        *inner.context_popover.borrow_mut() = Some(popover.clone());

        let quick = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        quick.add_css_class("omg-quick-reactions");
        if inner.available_reactions_loading.get() {
            let state = gtk::Label::new(Some("Loading reactions…"));
            state.add_css_class("omg-muted");
            quick.append(&state);
        } else if let Some(error) = inner.available_reactions_error.borrow().as_ref() {
            let state = gtk::Label::new(Some(error));
            state.add_css_class("omg-error");
            state.set_ellipsize(gtk::pango::EllipsizeMode::End);
            state.set_max_width_chars(24);
            quick.append(&state);
            let retry = gtk::Button::with_label("Retry");
            retry.add_css_class("omg-primary");
            let action = inner.action.clone();
            let popover_weak = popover.downgrade();
            retry.connect_clicked(move |_| {
                if let Some(popover) = popover_weak.upgrade() {
                    popover.popdown();
                }
                if let Some(callback) = action.borrow().as_ref().cloned() {
                    callback(MessageAction::RetryAvailableReactions);
                }
            });
            quick.append(&retry);
        } else {
            let reactions = inner.available_reactions.borrow();
            if reactions.is_empty() {
                let state = gtk::Label::new(Some("No reactions"));
                state.add_css_class("omg-muted");
                quick.append(&state);
            }
            for emoji in reactions.iter().take(7).cloned() {
                let button = gtk::Button::with_label(&emoji);
                button.add_css_class("omg-reaction");
                let action = inner.action.clone();
                let popover_weak = popover.downgrade();
                button.connect_clicked(move |_| {
                    if let Some(popover) = popover_weak.upgrade() {
                        popover.popdown();
                    }
                    if let Some(callback) = action.borrow().as_ref().cloned() {
                        callback(MessageAction::Reaction {
                            msg_id,
                            emoji: Some(emoji.clone()),
                        });
                    }
                });
                quick.append(&button);
            }
        }
        let more_reactions = gtk::Button::with_label("…");
        more_reactions.add_css_class("omg-reaction");
        {
            let anchor = inner
                .store
                .borrow()
                .entries
                .get(&msg_id)
                .map(|entry| entry.row.reaction_anchor.clone());
            more_reactions.connect_clicked(move |_| {
                if let Some(anchor) = anchor.as_ref() {
                    anchor.emit_clicked();
                }
            });
        }
        quick.append(&more_reactions);
        menu.append(&quick);

        let copy = menu_button("Copy", false);
        {
            let clipboard = row.clipboard();
            let text = message.text.clone();
            let popover = popover.downgrade();
            copy.connect_clicked(move |_| {
                clipboard.set_text(&text);
                if let Some(popover) = popover.upgrade() {
                    popover.popdown();
                }
            });
        }
        menu.append(&copy);

        if is_virtual(message.chat_id) {
            menu.remove(&quick);
            popover.popup();
            return;
        }

        // Deleted rows are archive evidence, not actionable Telegram rows.
        if message.deleted {
            menu.remove(&quick);
            popover.popup();
            return;
        }

        let copy_message_id = menu_button("Copy message id", false);
        Self::connect_menu_action(
            inner,
            &copy_message_id,
            &popover,
            MessageAction::CopyMessageId(msg_id),
        );
        menu.append(&copy_message_id);

        if message.sender_id.is_some() {
            let copy_user_id = menu_button("Copy user id", false);
            Self::connect_menu_action(
                inner,
                &copy_user_id,
                &popover,
                MessageAction::CopyUserId(msg_id),
            );
            menu.append(&copy_user_id);
        }

        if message.edited && inner.edit_history.get() {
            let history = menu_button("Edit history", false);
            Self::connect_menu_action(
                inner,
                &history,
                &popover,
                MessageAction::EditHistory(msg_id),
            );
            menu.append(&history);
        }

        if inner.ai_enabled.get() {
            let draft = menu_button("Draft reply with AI", false);
            Self::connect_menu_action(inner, &draft, &popover, MessageAction::DraftReply(msg_id));
            menu.append(&draft);
            if !message.text.is_empty() {
                let translate = menu_button("Translate", false);
                Self::connect_menu_action(
                    inner,
                    &translate,
                    &popover,
                    MessageAction::Translate(msg_id),
                );
                menu.append(&translate);
                if message.text.chars().count() > 300 {
                    let summarize = menu_button("Summarize", false);
                    Self::connect_menu_action(
                        inner,
                        &summarize,
                        &popover,
                        MessageAction::Summarize(msg_id),
                    );
                    menu.append(&summarize);
                }
            }
        }

        let reply = menu_button("Reply", false);
        Self::connect_menu_action(inner, &reply, &popover, MessageAction::Reply(msg_id));
        menu.append(&reply);

        let forward = menu_button("Forward", false);
        Self::connect_menu_action(inner, &forward, &popover, MessageAction::Forward(msg_id));
        menu.append(&forward);

        let select = menu_button("Select", false);
        Self::connect_menu_action(inner, &select, &popover, MessageAction::Select(msg_id));
        menu.append(&select);

        if message.outgoing && message.media.is_none() && !message.text.is_empty() {
            let edit = menu_button("Edit", false);
            Self::connect_menu_action(inner, &edit, &popover, MessageAction::Edit(msg_id));
            menu.append(&edit);
        }
        if message.outgoing {
            let delete = menu_button("Delete", true);
            Self::connect_menu_action(inner, &delete, &popover, MessageAction::Delete(msg_id));
            menu.append(&delete);
        }
        popover.popup();
    }

    fn show_header_menu(inner: &Rc<MessagesInner>, button: &gtk::Button) {
        let Some(summary) = inner.header_summary.borrow().clone() else {
            return;
        };
        let (popover, contents) = menus::popover();
        let actions = [
            ("Search", ChatAction::Search, false),
            (
                if summary.muted { "Unmute" } else { "Mute" },
                ChatAction::Mute(if summary.muted {
                    crate::tg::MuteMode::Unmute
                } else {
                    crate::tg::MuteMode::Forever
                }),
                false,
            ),
            (
                if summary.pinned { "Unpin" } else { "Pin" },
                ChatAction::Pin(!summary.pinned),
                false,
            ),
            ("Mark as unread", ChatAction::MarkUnread(true), false),
            ("Jump to date", ChatAction::JumpToDate, false),
            ("Clear history", ChatAction::ClearHistory, true),
            ("Delete chat", ChatAction::Delete, true),
            ("Chat info", ChatAction::Info, false),
        ];
        for (label, action, danger) in actions {
            let menu_button = menus::button(label, danger);
            let inner_weak = Rc::downgrade(inner);
            let popover_weak = popover.downgrade();
            menu_button.connect_clicked(move |_| {
                if let Some(popover) = popover_weak.upgrade() {
                    popover.popdown();
                }
                let Some(inner) = inner_weak.upgrade() else {
                    return;
                };
                if matches!(action, ChatAction::JumpToDate) {
                    inner.header_more.grab_focus();
                    Self::show_jump_calendar(&inner);
                } else if let Some(callback) = inner.action.borrow().as_ref().cloned() {
                    callback(MessageAction::Header(action));
                }
            });
            contents.append(&menu_button);
        }
        inner.header_popover.show(button, popover);
    }

    fn show_jump_calendar(inner: &Rc<MessagesInner>) {
        let calendar_popover = gtk::Popover::new();
        calendar_popover.add_css_class("omg-menu");
        calendar_popover.set_has_arrow(false);
        let calendar = gtk::Calendar::new();
        let action_callback = inner.action.clone();
        let popover_for_day = calendar_popover.downgrade();
        calendar.connect_day_selected(move |calendar| {
            if let Some(popover) = popover_for_day.upgrade() {
                popover.popdown();
            }
            if let Some(date) = calendar_day_end(calendar) {
                if let Some(callback) = action_callback.borrow().as_ref().cloned() {
                    callback(MessageAction::JumpToDate(date));
                }
            }
        });
        calendar_popover.set_child(Some(&calendar));
        inner
            .header_popover
            .show(&inner.header_more, calendar_popover);
    }

    fn connect_menu_action(
        inner: &Rc<MessagesInner>,
        button: &gtk::Button,
        popover: &gtk::Popover,
        message_action: MessageAction,
    ) {
        let action = inner.action.clone();
        let popover = popover.downgrade();
        button.connect_clicked(move |_| {
            if let Some(popover) = popover.upgrade() {
                popover.popdown();
            }
            if let Some(callback) = action.borrow().as_ref().cloned() {
                callback(message_action.clone());
            }
        });
    }

    pub fn begin_reply(&self, msg_id: i32) {
        if self.inner.edit.borrow().is_some() {
            self.cancel_edit();
        }
        let Some(message) = self.message(msg_id) else {
            return;
        };
        let sender = if message.sender.is_empty() {
            "Unknown"
        } else {
            &message.sender
        };
        self.inner.reply_to.set(Some(msg_id));
        self.inner.reply_label.set_label(&format!(
            "Reply to {}: {}",
            sender,
            snippet(&message.text, 60)
        ));
        self.inner.reply_bar.set_visible(true);
        self.focus_composer();
    }

    pub fn begin_edit(&self, msg_id: i32) {
        let Some(message) = self.message(msg_id) else {
            return;
        };
        if !message.outgoing || message.media.is_some() || message.text.is_empty() {
            return;
        }
        self.cancel_reply();
        if self.inner.edit.borrow().is_some() {
            self.cancel_edit();
        }
        let draft = self.composer_text();
        *self.inner.edit.borrow_mut() = Some(EditMode { msg_id, draft });
        self.inner.ai_draft.set(false);
        self.inner.edit_label.set_label("Editing message");
        self.set_composer_text(if message.markdown.is_empty() {
            &message.text
        } else {
            &message.markdown
        });
        self.inner.edit_bar.set_visible(true);
        self.focus_composer();
    }

    pub fn cancel_mode(&self) -> bool {
        if self.inner.selection_mode.get() {
            self.exit_selection_mode();
            true
        } else if self.inner.edit.borrow().is_some() {
            self.cancel_edit();
            true
        } else if self.inner.ai_draft.replace(false) {
            self.set_composer_text("");
            self.inner.edit_bar.set_visible(false);
            self.inner.edit_label.set_label("Editing message");
            true
        } else if self.inner.reply_to.get().is_some() {
            self.cancel_reply();
            true
        } else {
            false
        }
    }

    pub fn cancel_all_modes(&self) {
        self.exit_selection_mode();
        self.cancel_edit();
        if self.inner.ai_draft.replace(false) {
            self.set_composer_text("");
        }
        self.inner.edit_label.set_label("Editing message");
        self.cancel_reply();
    }

    /// The composer TextView is about to be hidden (selection or recording
    /// mode); drop keyboard focus first so GTK delivers its focus-out instead
    /// of warning at unmap time.
    fn release_composer_focus(&self) {
        let composer: gtk::Widget = self.inner.composer.clone().upcast();
        let focused = composer.is_focus() || composer.focus_child().is_some();
        if focused {
            if let Some(window) = composer.root().and_then(|r| r.downcast::<gtk::Window>().ok()) {
                gtk::prelude::GtkWindowExt::set_focus(&window, None::<&gtk::Widget>);
            }
        }
    }

    pub fn begin_selection(&self, msg_id: i32) -> bool {
        // Selection cannot merely hide an active recorder: that would leave
        // the recorder machine running with no Cancel/Send controls. Ask the
        // shell to transition the machine first and let the user enter
        // selection on a subsequent action.
        if self.inner.recorder.is_visible() {
            if let Some(callback) = self.inner.action.borrow().as_ref().cloned() {
                callback(MessageAction::RecorderCancel);
            }
            return false;
        }
        let selectable = self
            .inner
            .store
            .borrow()
            .entries
            .get(&msg_id)
            .is_some_and(|entry| !entry.msg.deleted);
        if !selectable {
            return false;
        }
        self.cancel_edit();
        self.cancel_reply();
        self.inner.ai_draft.set(false);
        self.inner.edit_bar.set_visible(false);
        self.inner.selection_mode.set(true);
        self.inner.selection.borrow_mut().set(msg_id, true);
        let rows = self
            .inner
            .store
            .borrow()
            .entries
            .values()
            .map(|entry| {
                (
                    entry.msg.id,
                    entry.msg.deleted,
                    entry.row.selection.clone(),
                )
            })
            .collect::<Vec<_>>();
        for (id, deleted, check) in rows {
            check.set_sensitive(!deleted);
            check.set_visible(!deleted);
            check.set_active(id == msg_id);
        }
        self.release_composer_focus();
        self.inner.composer_box.set_visible(false);
        self.inner.selection_bar.set_visible(true);
        Self::refresh_selection_inner(&self.inner);
        true
    }

    pub fn exit_selection_mode(&self) {
        if !self.inner.selection_mode.replace(false) {
            return;
        }
        let checks = self
            .inner
            .store
            .borrow()
            .entries
            .values()
            .map(|entry| entry.row.selection.clone())
            .collect::<Vec<_>>();
        // D6: remove every checkbox before any caller can tear rows down.
        for check in checks {
            check.set_visible(false);
            check.set_active(false);
        }
        self.inner.selection.borrow_mut().ids.clear();
        self.inner.selection_bar.set_visible(false);
        if !self.inner.recorder.is_visible() {
            self.inner.composer_box.set_visible(true);
        }
    }

    pub fn set_selected(&self, msg_id: i32, selected: bool) {
        Self::set_selected_inner(&self.inner, msg_id, selected);
    }

    pub fn drop_selection_ids(&self, ids: &[i32]) -> bool {
        let changed = self.inner.selection.borrow_mut().remove_deleted(ids);
        if !changed {
            return false;
        }
        let checks = {
            let store = self.inner.store.borrow();
            ids.iter()
                .filter_map(|id| {
                    store
                        .entries
                        .get(id)
                        .map(|entry| entry.row.selection.clone())
                })
                .collect::<Vec<_>>()
        };
        for check in checks {
            check.set_active(false);
            check.set_sensitive(false);
            check.set_visible(false);
        }
        Self::refresh_selection_inner(&self.inner);
        true
    }

    pub fn selection_ids(&self) -> Vec<i32> {
        let store = self.inner.store.borrow();
        self.inner.selection.borrow().ordered(&store.order)
    }

    pub fn selection_count(&self) -> usize {
        self.inner.selection.borrow().ids.len()
    }

    pub fn selection_mode(&self) -> bool {
        self.inner.selection_mode.get()
    }

    pub fn selection_all_outgoing(&self) -> bool {
        let ids = self.selection_ids();
        !ids.is_empty()
            && ids.iter().all(|id| {
                self.inner
                    .store
                    .borrow()
                    .entries
                    .get(id)
                    .is_some_and(|entry| entry.msg.outgoing && !entry.msg.deleted)
            })
    }

    fn set_selected_inner(inner: &Rc<MessagesInner>, msg_id: i32, selected: bool) {
        if !inner.selection_mode.get() {
            return;
        }
        let selectable = inner
            .store
            .borrow()
            .entries
            .get(&msg_id)
            .is_some_and(|entry| !entry.msg.deleted);
        if !selectable {
            return;
        }
        inner.selection.borrow_mut().set(msg_id, selected);
        Self::refresh_selection_inner(inner);
    }

    fn refresh_selection_inner(inner: &Rc<MessagesInner>) {
        let ids = {
            let store = inner.store.borrow();
            inner.selection.borrow().ordered(&store.order)
        };
        let count = ids.len();
        inner
            .selection_count
            .set_label(&format!("{count} selected"));
        inner.selection_copy.set_sensitive(count > 0);
        inner.selection_forward.set_sensitive(count > 0);
        let all_outgoing = count > 0
            && ids.iter().all(|id| {
                inner
                    .store
                    .borrow()
                    .entries
                    .get(id)
                    .is_some_and(|entry| entry.msg.outgoing && !entry.msg.deleted)
            });
        inner.selection_delete.set_sensitive(all_outgoing);
    }

    pub fn show_sticker_popover(&self, popover: gtk::Popover) {
        self.inner.composer_popover.show(&self.inner.sticker, popover);
    }

    pub fn dismiss_composer_popover(&self) {
        self.inner.composer_popover.dismiss();
    }

    pub fn show_recorder_starting(&self) {
        self.exit_selection_mode();
        self.release_composer_focus();
        self.inner.composer_box.set_visible(false);
        self.inner.recorder.show_starting();
    }

    pub fn show_recorder_recording(&self) {
        self.release_composer_focus();
        self.inner.composer_box.set_visible(false);
        self.inner.recorder.show_recording();
    }

    pub fn show_recorder_stopping(&self) {
        self.inner.recorder.show_stopping();
    }

    pub fn show_recorder_sending(&self) {
        self.inner.recorder.show_sending();
    }

    /// A8: Cancel arrived while start/stop was still in flight. The bar stays
    /// up (and the composer stays locked) until that completion resolves.
    pub fn show_recorder_cancelling(&self) {
        self.inner.recorder.show_cancelling();
    }

    pub fn show_recorder_error(&self, message: &str, retryable: bool) {
        self.release_composer_focus();
        self.inner.composer_box.set_visible(false);
        self.inner.recorder.show_error(message, retryable);
    }

    pub fn hide_recorder(&self) {
        self.inner.recorder.hide();
        if !self.inner.selection_mode.get() {
            self.inner.composer_box.set_visible(true);
        }
    }

    pub fn recorder_visible(&self) -> bool {
        self.inner.recorder.is_visible()
    }

    pub fn recorder_status(&self) -> String {
        self.inner.recorder.status_text()
    }

    pub fn probe_recorder_cancel(&self) {
        self.inner.recorder.trigger_cancel();
    }

    pub fn probe_recorder_send(&self) {
        self.inner.recorder.trigger_send();
    }

    pub fn probe_recorder_retry(&self) {
        self.inner.recorder.trigger_retry();
    }

    pub fn prepare_attachment(&self) {
        self.cancel_reply();
        self.cancel_edit();
    }

    fn cancel_reply(&self) {
        self.inner.reply_to.set(None);
        self.inner.reply_bar.set_visible(false);
    }

    fn cancel_edit(&self) {
        let edit = self.inner.edit.borrow_mut().take();
        if let Some(edit) = edit {
            self.set_composer_text(&edit.draft);
        }
        self.inner.edit_bar.set_visible(false);
    }

    pub fn show_ai_draft(&self, text: &str) {
        self.cancel_reply();
        self.cancel_edit();
        self.set_composer_text(text);
        self.inner.ai_draft.set(true);
        self.inner
            .edit_label
            .set_label("AI draft — Enter sends, Esc discards");
        self.inner.edit_bar.set_visible(true);
        self.focus_composer();
    }

    pub fn ai_draft_visible(&self) -> bool {
        self.inner.ai_draft.get() && self.inner.edit_bar.is_visible()
    }

    pub fn complete_text_operation(&self, snapshot_text: &str, epoch_is_current: bool) {
        if !epoch_is_current || self.composer_text() != snapshot_text {
            return;
        }
        self.set_composer_text("");
        self.inner.edit.borrow_mut().take();
        self.inner.ai_draft.set(false);
        self.inner.edit_bar.set_visible(false);
        self.inner.edit_label.set_label("Editing message");
        self.cancel_reply();
    }

    pub fn composer_text(&self) -> String {
        let buffer = self.inner.composer.buffer();
        buffer
            .text(&buffer.start_iter(), &buffer.end_iter(), true)
            .to_string()
    }

    /// The text view that owns composer-scoped shortcut controllers (A28).
    pub fn composer(&self) -> gtk::TextView {
        self.inner.composer.clone()
    }

    pub fn set_composer_text(&self, text: &str) {
        self.inner.composer_signal_blocked.set(true);
        self.inner.composer.buffer().set_text(text);
        self.inner.composer_signal_blocked.set(false);
    }

    pub fn composer_cursor(&self) -> i32 {
        self.inner.composer.buffer().cursor_position()
    }

    pub fn is_editing(&self) -> bool {
        self.inner.edit.borrow().is_some()
    }

    pub fn draft_snapshot_for_switch(&self) -> (String, Option<i32>, i32) {
        if self.is_editing() {
            self.cancel_edit();
        }
        (
            self.composer_text(),
            self.reply_to(),
            self.composer_cursor(),
        )
    }

    pub fn restore_draft(&self, text: &str, reply_to: Option<i32>, cursor: i32) {
        self.cancel_all_modes();
        self.set_composer_text(text);
        let buffer = self.inner.composer.buffer();
        let mut iter = buffer.iter_at_offset(cursor.clamp(0, buffer.char_count()));
        buffer.place_cursor(&iter);
        if let Some(reply_to) = reply_to {
            if self.message(reply_to).is_some() {
                self.begin_reply(reply_to);
            } else {
                self.inner.reply_to.set(Some(reply_to));
                self.inner.reply_label.set_label("Reply to message");
                self.inner.reply_bar.set_visible(true);
            }
        }
        // Keep the iter alive through place_cursor on older gtk-rs bindings.
        let _ = &mut iter;
    }

    pub fn show_draft_error(&self, error: &str) {
        self.show_error(&format!("draft not saved — retry: {error}"));
        self.inner.draft_retry.set_visible(true);
    }

    pub fn clear_draft_error(&self) {
        if self.inner.error.label().starts_with("draft not saved") {
            self.clear_error();
        }
        self.inner.draft_retry.set_visible(false);
    }

    /// Probe support for the live GTK wrapping path: select character offsets
    /// exactly as a user selection in the composer would.
    pub fn probe_select_composer(&self, start: i32, end: i32) {
        let buffer = self.inner.composer.buffer();
        let chars = buffer.char_count();
        let start = buffer.iter_at_offset(start.clamp(0, chars));
        let end = buffer.iter_at_offset(end.clamp(0, chars));
        buffer.select_range(&start, &end);
    }

    pub fn probe_composer_cursor(&self) -> i32 {
        let buffer = self.inner.composer.buffer();
        buffer.iter_at_mark(&buffer.get_insert()).offset()
    }

    pub fn reply_to(&self) -> Option<i32> {
        self.inner.reply_to.get()
    }

    pub fn edit_id(&self) -> Option<i32> {
        self.inner.edit.borrow().as_ref().map(|edit| edit.msg_id)
    }

    pub fn set_busy(&self, busy: bool) {
        self.inner.busy.set(busy);
        let sensitive = self.inner.virtual_mode.get() || !busy;
        self.inner.composer.set_sensitive(sensitive);
        self.inner.attach.set_sensitive(sensitive);
        self.inner.emoji.set_sensitive(sensitive);
        self.inner.sticker.set_sensitive(sensitive);
        self.inner.send.set_sensitive(sensitive);
        self.inner
            .drop_target
            .set_actions(if busy && !self.inner.virtual_mode.get() {
                gdk::DragAction::empty()
            } else {
                gdk::DragAction::COPY
            });
    }

    pub fn is_busy(&self) -> bool {
        self.inner.busy.get()
    }

    pub fn focus_composer(&self) {
        self.inner.composer.grab_focus();
    }

    pub fn show_error(&self, message: &str) {
        self.inner.error.set_label(message);
        self.inner.error.set_visible(true);
    }

    pub fn show_reaction_error(&self, message: &str) {
        self.show_error(message);
        self.inner.operation_retry.set_visible(true);
    }

    pub fn clear_reaction_error(&self) {
        self.inner.operation_retry.set_visible(false);
        if self.inner.error.label().contains("reaction")
            || self.inner.error.label().contains("transient failure")
        {
            self.clear_error();
        }
    }

    pub fn clear_error(&self) {
        self.inner.error.set_visible(false);
        self.inner.error.set_label("");
        self.inner.draft_retry.set_visible(false);
        self.inner.operation_retry.set_visible(false);
    }

    pub fn message(&self, msg_id: i32) -> Option<Msg> {
        self.inner
            .store
            .borrow()
            .entries
            .get(&msg_id)
            .map(|entry| entry.msg.clone())
    }

    pub fn messages(&self) -> Vec<Msg> {
        let store = self.inner.store.borrow();
        store
            .order
            .iter()
            .filter_map(|id| store.entries.get(id).map(|entry| entry.msg.clone()))
            .collect()
    }

    pub fn last_id(&self) -> Option<i32> {
        self.inner.store.borrow().order.last().copied()
    }

    pub fn last_message(&self) -> Option<Msg> {
        let store = self.inner.store.borrow();
        store
            .order
            .last()
            .and_then(|id| store.entries.get(id))
            .map(|entry| entry.msg.clone())
    }

    pub fn last_before(&self, msg_id: i32) -> Option<Msg> {
        let store = self.inner.store.borrow();
        store
            .order
            .iter()
            .rev()
            .filter(|id| **id != msg_id)
            .find_map(|id| store.entries.get(id))
            .map(|entry| entry.msg.clone())
    }

    pub fn last_excluding(&self, excluded: &[i32]) -> Option<Msg> {
        let store = self.inner.store.borrow();
        store
            .order
            .iter()
            .rev()
            .filter(|id| !excluded.contains(id))
            .find_map(|id| store.entries.get(id))
            .map(|entry| entry.msg.clone())
    }

    pub fn is_last(&self, msg_id: i32) -> bool {
        self.inner.store.borrow().order.last() == Some(&msg_id)
    }

    pub fn remove(&self, msg_id: i32) -> Option<Msg> {
        self.drop_selection_ids(&[msg_id]);
        self.dismiss_row_popovers();
        self.inner.pending_messages.borrow_mut().remove(&msg_id);
        let entry = {
            let mut store = self.inner.store.borrow_mut();
            let entry = store.entries.remove(&msg_id)?;
            store.order.retain(|id| *id != msg_id);
            entry
        };
        self.move_focus_before_removal(&entry.row.widget);
        for source in entry.row.animation_sources.borrow_mut().drain(..) {
            remove_source_if_present(source);
        }
        if let Some(source) = entry.row.media_loading_source.borrow_mut().take() {
            remove_source_if_present(source);
        }
        self.inner.list.remove(&entry.row.widget);
        self.reorder_rows();
        self.refresh_quotes();
        Some(entry.msg)
    }

    pub fn animate_deleted(&self, msg_id: i32) -> bool {
        self.drop_selection_ids(&[msg_id]);
        let row = self
            .inner
            .store
            .borrow()
            .entries
            .get(&msg_id)
            .map(|entry| entry.row.clone());
        let Some(row) = row else {
            return false;
        };
        self.dismiss_row_popovers();
        self.move_focus_before_removal(&row.widget);
        let (animated, source) = self
            .inner
            .effects
            .message_deleted_tracked(row.widget.upcast_ref());
        if let Some(source) = source {
            row.animation_sources.borrow_mut().push(source);
        }
        animated
    }

    pub fn mark_deleted(&self, msg_id: i32) -> bool {
        self.drop_selection_ids(&[msg_id]);
        let row = {
            let mut store = self.inner.store.borrow_mut();
            let Some(entry) = store.entries.get_mut(&msg_id) else {
                return false;
            };
            entry.msg.deleted = true;
            entry.row.clone()
        };
        set_deleted_rendering(&row, true);
        row.selection.set_sensitive(false);
        row.selection.set_visible(false);
        true
    }

    pub fn is_deleted(&self, msg_id: i32) -> bool {
        self.inner
            .store
            .borrow()
            .entries
            .get(&msg_id)
            .is_some_and(|entry| entry.msg.deleted)
    }

    pub fn is_marked_deleted(&self, msg_id: i32) -> bool {
        self.inner
            .store
            .borrow()
            .entries
            .get(&msg_id)
            .is_some_and(|entry| {
                entry.msg.deleted
                    && entry.row.widget.has_css_class("omg-msg-deleted")
                    && entry.row.deleted_tag.is_visible()
            })
    }

    pub fn contains(&self, msg_id: i32) -> bool {
        self.inner.store.borrow().entries.contains_key(&msg_id)
    }

    pub fn contains_text(&self, text: &str) -> bool {
        self.inner
            .store
            .borrow()
            .entries
            .values()
            .any(|entry| entry.msg.text == text)
    }

    pub fn len(&self) -> usize {
        self.inner.store.borrow().order.len()
    }

    pub fn is_loading(&self) -> bool {
        self.inner.loading.is_visible() && self.inner.loading.label() == "Loading…"
    }

    pub fn is_empty_state(&self) -> bool {
        self.inner.loading.is_visible() && self.inner.loading.label() == "Select a chat"
    }

    pub fn aux_contains(&self, msg_id: i32, needle: &str) -> bool {
        let slot = self
            .inner
            .store
            .borrow()
            .entries
            .get(&msg_id)
            .map(|entry| entry.row.aux_slot.clone());
        let Some(slot) = slot else { return false };
        let mut child = slot.first_child();
        while let Some(widget) = child {
            if widget
                .downcast_ref::<gtk::Label>()
                .is_some_and(|label| label.label().contains(needle))
            {
                return true;
            }
            child = widget.next_sibling();
        }
        false
    }

    pub fn pagination_ready(&self) -> bool {
        !self.inner.loading.is_visible()
            && !self.inner.paging.get()
            && !self.inner.suppress_paging.get()
    }

    pub fn find_outgoing_text(&self, text: &str) -> Option<i32> {
        let pending = self.inner.pending_messages.borrow();
        self.inner
            .store
            .borrow()
            .entries
            .values()
            .find(|entry| {
                entry.msg.outgoing && entry.msg.text == text && !pending.contains(&entry.msg.id)
            })
            .map(|entry| entry.msg.id)
    }

    pub fn find_incoming_text_after(&self, text: &str, after_id: i32) -> Option<i32> {
        self.inner
            .store
            .borrow()
            .entries
            .values()
            .find(|entry| !entry.msg.outgoing && entry.msg.id > after_id && entry.msg.text == text)
            .map(|entry| entry.msg.id)
    }

    pub fn find_edited_incoming_after(&self, after_id: i32) -> Option<i32> {
        self.inner
            .store
            .borrow()
            .entries
            .values()
            .find(|entry| !entry.msg.outgoing && entry.msg.id > after_id && entry.msg.edited)
            .map(|entry| entry.msg.id)
    }

    pub fn begin_media(&self, msg_id: i32) -> Option<MediaKind> {
        let (kind, button) = {
            let mut store = self.inner.store.borrow_mut();
            let entry = store.entries.get_mut(&msg_id)?;
            let may_start = matches!(entry.media_state, MediaState::NotStarted)
                || matches!(entry.media_state, MediaState::Failed) && entry.media_retryable;
            if !may_start {
                return None;
            }
            let kind = entry.msg.media?;
            entry.media_state = MediaState::InFlight;
            entry.media_retryable = false;
            (kind, entry.row.media_button.clone())
        };
        if let Some(button) = button {
            button.set_sensitive(false);
        }
        Some(kind)
    }

    pub fn media_state(&self, msg_id: i32) -> Option<MediaState> {
        self.inner
            .store
            .borrow()
            .entries
            .get(&msg_id)
            .map(|entry| entry.media_state.clone())
    }

    pub fn media_retryable(&self, msg_id: i32) -> bool {
        self.inner
            .store
            .borrow()
            .entries
            .get(&msg_id)
            .is_some_and(|entry| entry.media_retryable)
    }

    pub fn media_kind(&self, msg_id: i32) -> Option<MediaKind> {
        self.message(msg_id).and_then(|message| message.media)
    }

    pub fn media_path(&self, msg_id: i32) -> Option<PathBuf> {
        self.inner
            .store
            .borrow()
            .entries
            .get(&msg_id)
            .and_then(|entry| match &entry.media_state {
                MediaState::Done(path) => Some(path.clone()),
                _ => None,
            })
    }

    pub fn photo_messages(&self) -> Vec<Msg> {
        self.messages()
            .into_iter()
            .filter(|message| message.media == Some(MediaKind::Photo) && !message.deleted)
            .collect()
    }

    pub fn focus_message_or_composer(&self, msg_id: i32) {
        let row = self
            .inner
            .store
            .borrow()
            .entries
            .get(&msg_id)
            .map(|entry| entry.row.widget.clone());
        if !row.is_some_and(|row| row.grab_focus()) {
            self.focus_composer();
        }
    }

    pub fn rendered_markup(&self, msg_id: i32) -> Option<String> {
        self.inner
            .store
            .borrow()
            .entries
            .get(&msg_id)
            .and_then(|entry| {
                entry
                    .row
                    .text
                    .borrow()
                    .as_ref()
                    .map(|label| label.label().to_string())
            })
    }

    pub fn probe_click_spoiler(&self, msg_id: i32) -> bool {
        let target = self.inner.store.borrow().entries.get(&msg_id).and_then(|entry| {
            let label = entry.row.text.borrow().as_ref().cloned()?;
            let start = entry.msg.spans.iter().find_map(|span| {
                (matches!(span.kind, SpanKind::Spoiler)
                    && !entry
                        .row
                        .revealed_spoilers
                        .borrow()
                        .contains(&(span.start, span.end)))
                .then_some(span.start)
            })?;
            Some((label, entry.msg.text.clone(), start))
        });
        let Some((label, text, start)) = target else { return false };
        let byte_index = text
            .char_indices()
            .nth(start)
            .map(|(index, _)| index)
            .unwrap_or(text.len()) as i32;
        let rectangle = label.layout().index_to_pos(byte_index);
        let (offset_x, offset_y) = label.layout_offsets();
        let x = f64::from(offset_x)
            + f64::from(rectangle.x() + rectangle.width().max(gtk::pango::SCALE) / 2)
                / f64::from(gtk::pango::SCALE);
        let y = f64::from(offset_y)
            + f64::from(rectangle.y() + rectangle.height().max(gtk::pango::SCALE) / 2)
                / f64::from(gtk::pango::SCALE);
        let controllers = label.observe_controllers();
        for index in 0..controllers.n_items() {
            let Some(controller) = controllers.item(index) else {
                continue;
            };
            if let Ok(gesture) = controller.downcast::<gtk::GestureClick>() {
                if gesture.name().as_deref() != Some("omg-spoiler-gesture") {
                    continue;
                }
                gesture.emit_by_name::<()>("pressed", &[&1_i32, &x, &y]);
                return true;
            }
        }
        false
    }

    pub fn message_has_pre_block(&self, msg_id: i32) -> bool {
        self.inner
            .store
            .borrow()
            .entries
            .get(&msg_id)
            .is_some_and(|entry| entry.row.text_block.has_css_class("omg-pre-block"))
    }

    pub fn message_has_css_class(&self, msg_id: i32, class: &str) -> bool {
        self.inner
            .store
            .borrow()
            .entries
            .get(&msg_id)
            .is_some_and(|entry| {
                entry
                    .row
                    .text
                    .borrow()
                    .as_ref()
                    .is_some_and(|label| label.has_css_class(class))
            })
    }

    pub fn row_has_css_class(&self, msg_id: i32, class: &str) -> bool {
        self.inner
            .store
            .borrow()
            .entries
            .get(&msg_id)
            .is_some_and(|entry| entry.row.widget.has_css_class(class))
    }

    pub fn error_text(&self) -> String {
        self.inner.error.label().to_string()
    }

    pub fn message_has_focus(&self, msg_id: i32) -> bool {
        self.inner
            .store
            .borrow()
            .entries
            .get(&msg_id)
            // is_focus() = the focus widget of the toplevel; has_focus() also
            // needs the toplevel to be active, which a tiling compositor
            // never grants the smoke window.
            .is_some_and(|entry| {
                entry.row.widget.is_focus() || entry.row.widget.focus_child().is_some()
            })
    }

    pub fn reaction(&self, msg_id: i32, emoji: &str) -> Option<crate::tg::Reaction> {
        self.message(msg_id).and_then(|message| {
            message
                .reactions
                .into_iter()
                .find(|reaction| reaction.emoji == emoji)
        })
    }

    pub fn trigger_reaction_retry(&self) {
        self.inner.operation_retry.emit_clicked();
    }

    pub fn available_reactions_settled(&self) -> bool {
        !self.inner.available_reactions_loading.get()
    }

    pub fn probe_choose_quick_reaction(&self, msg_id: i32, emoji: &str) -> bool {
        Self::show_context_menu(&self.inner, msg_id, 0.0, 0.0);
        let Some(button) = self.context_reaction_button(emoji) else {
            return false;
        };
        button.emit_clicked();
        true
    }

    pub fn probe_retry_available_reactions(&self, msg_id: i32) -> bool {
        Self::show_context_menu(&self.inner, msg_id, 0.0, 0.0);
        let Some(button) = self.context_reaction_button("Retry") else {
            return false;
        };
        button.emit_clicked();
        true
    }

    pub fn probe_open_reaction_chooser(&self, msg_id: i32) -> bool {
        Self::show_context_menu(&self.inner, msg_id, 0.0, 0.0);
        let Some(button) = self.context_reaction_button("…") else {
            return false;
        };
        button.emit_clicked();
        self.inner.context_popover.borrow().is_none() && self.inner.reaction_popover.is_open()
    }

    pub fn probe_pick_reaction_emoji(&self, emoji: &str) -> bool {
        let chooser = self.inner.probe_reaction_chooser.borrow().as_ref().cloned();
        let Some(chooser) = chooser else { return false };
        chooser.emit_by_name::<()>("emoji-picked", &[&emoji]);
        self.inner.reaction_popover.dismiss();
        self.inner.probe_reaction_chooser.borrow_mut().take();
        true
    }

    fn context_reaction_button(&self, label: &str) -> Option<gtk::Button> {
        let popover = self.inner.context_popover.borrow().as_ref().cloned()?;
        let contents = popover.child()?.downcast::<gtk::Box>().ok()?;
        let quick = contents.first_child()?.downcast::<gtk::Box>().ok()?;
        let mut child = quick.first_child();
        while let Some(widget) = child {
            if let Ok(button) = widget.clone().downcast::<gtk::Button>() {
                if button.label().as_deref() == Some(label) {
                    return Some(button);
                }
            }
            child = widget.next_sibling();
        }
        None
    }

    pub fn reaction_retry_visible(&self) -> bool {
        self.inner.operation_retry.is_visible()
    }

    pub fn receipt_text(&self, msg_id: i32) -> Option<String> {
        self.inner
            .store
            .borrow()
            .entries
            .get(&msg_id)
            .map(|entry| entry.row.receipt.label().to_string())
    }

    pub fn has_media_card(&self, kind: MediaKind) -> bool {
        self.inner.store.borrow().entries.values().any(|entry| {
            entry.msg.media == Some(kind)
                && entry
                    .row
                    .media_button
                    .as_ref()
                    .is_some_and(gtk::prelude::WidgetExt::is_visible)
        })
    }

    pub fn trigger_media(&self, msg_id: i32) -> bool {
        let button = self
            .inner
            .store
            .borrow()
            .entries
            .get(&msg_id)
            .and_then(|entry| entry.row.media_button.clone());
        let Some(button) = button else { return false };
        button.emit_clicked();
        true
    }

    pub fn web_preview_visible(&self, msg_id: i32) -> bool {
        self.inner
            .store
            .borrow()
            .entries
            .get(&msg_id)
            .is_some_and(|entry| {
                entry.row.web_preview.is_visible() && entry.row.web_preview.first_child().is_some()
            })
    }

    pub fn forwarded_header_text(&self, msg_id: i32) -> Option<String> {
        self.inner
            .store
            .borrow()
            .entries
            .get(&msg_id)
            .filter(|entry| entry.row.forwarded.is_visible())
            .map(|entry| entry.row.forwarded.label().to_string())
    }

    pub fn finish_image(&self, msg_id: i32, path: PathBuf, texture: &gdk::Texture) -> bool {
        // A historical (detached) view must never be yanked to the tail by a
        // late image swap-in (C4 + detached semantics).
        let should_stick = self.inner.stick_to_bottom.get() && !self.inner.detached.get();
        let adjustment = self.inner.scroll.vadjustment();
        let saved_upper = adjustment.upper();
        if should_stick {
            self.inner.suppress_paging.set(true);
        }
        let (media_slot, outgoing, media_loading_source) = {
            let mut store = self.inner.store.borrow_mut();
            let Some(entry) = store.entries.get_mut(&msg_id) else {
                if should_stick {
                    self.inner.suppress_paging.set(false);
                }
                return false;
            };
            entry.media_state = MediaState::Done(path.clone());
            (
                entry.row.media_slot.clone(),
                entry.msg.outgoing,
                entry.row.media_loading_source.clone(),
            )
        };
        if let Some(source) = media_loading_source.borrow_mut().take() {
            remove_source_if_present(source);
        }
        self.move_focus_before_removal(&media_slot);
        while let Some(child) = media_slot.first_child() {
            media_slot.remove(&child);
        }
        let picture = gtk::Picture::for_paintable(texture);
        picture.set_can_shrink(true);
        picture.set_hexpand(true);
        picture.set_vexpand(true);
        picture.set_content_fit(gtk::ContentFit::Contain);
        picture.set_halign(if outgoing {
            gtk::Align::End
        } else {
            gtk::Align::Start
        });
        let picture = PhotoClamp::new(&picture);
        picture.set_halign(if outgoing {
            gtk::Align::End
        } else {
            gtk::Align::Start
        });
        let action = self.inner.action.clone();
        let gesture = gtk::GestureClick::new();
        gesture.set_name(Some("omg-photo-gesture"));
        gesture.connect_released(move |_, _, _, _| {
            if let Some(callback) = action.borrow().as_ref().cloned() {
                callback(MessageAction::Media(msg_id));
            }
        });
        picture.add_controller(gesture);
        media_slot.append(&picture);
        if should_stick {
            let inner = self.inner.clone();
            self.after_upper_change(saved_upper, move |adjustment, changed| {
                if changed {
                    adjustment.set_value((adjustment.upper() - adjustment.page_size()).max(0.0));
                }
                inner.stick_to_bottom.set(true);
                inner.suppress_paging.set(false);
            });
        }
        true
    }

    pub fn finish_media_path(&self, msg_id: i32, path: PathBuf) -> bool {
        let button = {
            let mut store = self.inner.store.borrow_mut();
            let Some(entry) = store.entries.get_mut(&msg_id) else {
                return false;
            };
            entry.media_state = MediaState::Done(path.clone());
            entry.row.media_button.clone()
        };
        if let Some(button) = button {
            button.set_sensitive(true);
        }
        let callbacks = self
            .inner
            .media_ready
            .borrow_mut()
            .remove(&msg_id)
            .unwrap_or_default();
        for callback in callbacks {
            callback(path.clone());
        }
        true
    }

    pub fn on_media_ready(&self, msg_id: i32, callback: impl FnOnce(PathBuf) + 'static) -> bool {
        let state = self.media_state(msg_id);
        match state {
            Some(MediaState::Done(path)) => callback(path),
            Some(_) => self
                .inner
                .media_ready
                .borrow_mut()
                .entry(msg_id)
                .or_default()
                .push(Box::new(callback)),
            None => return false,
        }
        true
    }

    pub fn has_media_continuation(&self, msg_id: i32) -> bool {
        self.inner
            .media_ready
            .borrow()
            .get(&msg_id)
            .is_some_and(|callbacks| !callbacks.is_empty())
    }

    pub fn drop_media_continuations(&self, msg_id: i32) {
        self.inner.media_ready.borrow_mut().remove(&msg_id);
    }

    pub fn fail_media(&self, msg_id: i32, retryable: bool) -> bool {
        self.inner.media_ready.borrow_mut().remove(&msg_id);
        let (kind, media_slot, button, base, media_loading_source) = {
            let mut store = self.inner.store.borrow_mut();
            let Some(entry) = store.entries.get_mut(&msg_id) else {
                return false;
            };
            entry.media_state = MediaState::Failed;
            entry.media_retryable = retryable;
            let kind = entry.msg.media;
            let base = kind.map(|_| media_title(&entry.msg));
            (
                kind,
                entry.row.media_slot.clone(),
                entry.row.media_button.clone(),
                base,
                entry.row.media_loading_source.clone(),
            )
        };
        if let Some(source) = media_loading_source.borrow_mut().take() {
            remove_source_if_present(source);
        }
        match kind {
            Some(MediaKind::Photo | MediaKind::Sticker) => {
                self.move_focus_before_removal(&media_slot);
                while let Some(child) = media_slot.first_child() {
                    media_slot.remove(&child);
                }
                if retryable {
                    let retry = gtk::Button::with_label("Image unavailable — Retry");
                    retry.add_css_class("omg-doc-pill");
                    let action = self.inner.action.clone();
                    retry.connect_clicked(move |_| {
                        if let Some(callback) = action.borrow().as_ref().cloned() {
                            callback(MessageAction::Media(msg_id));
                        }
                    });
                    media_slot.append(&retry);
                } else {
                    let label = gtk::Label::new(Some("image unavailable"));
                    label.add_css_class("omg-media-placeholder");
                    media_slot.append(&label);
                }
            }
            Some(
                MediaKind::Document
                | MediaKind::Voice
                | MediaKind::Video
                | MediaKind::Gif
                | MediaKind::Audio
                | MediaKind::VideoNote
                | MediaKind::Unsupported,
            ) => {
                if let (Some(button), Some(base)) = (button, base) {
                    button.set_child(None::<&gtk::Widget>);
                    let label = if retryable {
                        format!("{base} — Retry")
                    } else {
                        format!("{base} (unavailable)")
                    };
                    button.set_label(&label);
                    button.set_sensitive(retryable);
                }
            }
            None => {}
        }
        true
    }

    pub fn set_typing(&self, name: &str) -> u64 {
        if let Some(source) = self.inner.typing_animation.borrow_mut().take() {
            remove_source_if_present(source);
        }
        let generation = self.inner.typing_generation.get().wrapping_add(1);
        self.inner.typing_generation.set(generation);
        let label = if name.is_empty() {
            "typing…".to_string()
        } else {
            format!("{name} is typing…")
        };
        self.inner.typing.set_label(&label);
        self.inner.typing.set_visible(true);
        self.inner.typing.add_css_class("omg-typing");
        *self.inner.typing_animation.borrow_mut() =
            self.inner.effects.typing_frame(&self.inner.typing, name);
        generation
    }

    pub fn clear_typing_if(&self, generation: u64) {
        if self.inner.typing_generation.get() == generation {
            if let Some(source) = self.inner.typing_animation.borrow_mut().take() {
                remove_source_if_present(source);
            }
            self.inner.typing.set_visible(false);
            self.inner.typing.remove_css_class("omg-typing");
            self.inner
                .typing
                .set_label(&self.inner.base_status.borrow());
            self.inner.typing.set_visible(true);
        }
    }

    pub fn clear_typing(&self) {
        if let Some(source) = self.inner.typing_animation.borrow_mut().take() {
            remove_source_if_present(source);
        }
        self.inner
            .typing_generation
            .set(self.inner.typing_generation.get().wrapping_add(1));
        self.inner.typing.remove_css_class("omg-typing");
        self.inner
            .typing
            .set_label(&self.inner.base_status.borrow());
        self.inner.typing.set_visible(true);
    }

    pub fn typing_generation(&self) -> u64 {
        self.inner.typing_generation.get()
    }

    /// The strftime format used for message times; re-formats every row.
    /// Invalid formats are rejected so chrono's formatter can never panic
    /// while rendering (the last valid format is kept instead).
    pub fn set_time_format(&self, format: &str) {
        if !valid_time_format(format) {
            return;
        }
        *self.inner.time_format.borrow_mut() = format.to_string();
        let rows: Vec<(gtk::Label, Msg)> = {
            let store = self.inner.store.borrow();
            store
                .order
                .iter()
                .filter_map(|id| {
                    store
                        .entries
                        .get(id)
                        .map(|entry| (entry.row.time.clone(), entry.msg.clone()))
                })
                .collect()
        };
        for (label, message) in rows {
            set_time_label(&label, &message, format);
        }
    }

    pub fn refresh_theme(&self) {
        let darker = crate::theme::load_colors()
            .get("darker_background")
            .cloned()
            .unwrap_or_default();
        if *self.inner.darker_background.borrow() == darker {
            return;
        }
        *self.inner.darker_background.borrow_mut() = darker.clone();
        let rows = {
            let store = self.inner.store.borrow();
            store
                .entries
                .values()
                .filter_map(|entry| {
                    entry.row.text.borrow().as_ref().cloned().map(|label| {
                        (
                            entry.msg.clone(),
                            label,
                            entry.row.revealed_spoilers.clone(),
                        )
                    })
                })
                .collect::<Vec<_>>()
        };
        for (message, label, revealed) in rows {
            apply_message_markup(&label, &message, &revealed, &darker);
        }
    }

    pub fn set_clock(&self, text: Option<&str>) {
        match text {
            Some(text) => {
                self.inner.clock.set_label(text);
                self.inner.clock.set_visible(true);
            }
            None => {
                self.inner.clock.set_visible(false);
                self.inner.clock.set_label("");
            }
        }
    }

    pub fn set_ghost(&self, on: bool) {
        self.inner.ghost.set_visible(on);
    }

    pub fn set_edit_history(&self, on: bool) {
        self.inner.edit_history.set(on);
    }

    pub fn set_ai_enabled(&self, on: bool) {
        self.inner.ai_enabled.set(on);
        let buttons: Vec<gtk::Button> = self
            .inner
            .store
            .borrow()
            .entries
            .values()
            .filter_map(|entry| entry.row.transcribe_button.clone())
            .collect();
        for button in buttons {
            button.set_visible(on);
        }
        if !on {
            self.dismiss_row_popovers();
        }
    }

    pub fn set_status(&self, text: Option<&str>) {
        match text {
            Some(text) => {
                *self.inner.base_status.borrow_mut() = text.to_string();
                self.inner.typing.set_label(text);
                self.inner.typing.set_visible(true);
            }
            None => {
                self.inner.base_status.borrow_mut().clear();
                self.clear_typing();
            }
        }
    }

    pub fn set_chat_summary(&self, summary: &ChatSummary, tg: &Tg) {
        self.inner.header_title.set_label(&summary.title);
        self.inner
            .header_avatar
            .bind(tg, summary.id, &summary.title, summary.has_photo);
        self.inner.chat_kind.set(summary.kind);
        self.inner
            .read_outbox
            .set(self.inner.read_outbox.get().max(summary.read_outbox_max_id));
        *self.inner.header_summary.borrow_mut() = Some(summary.clone());
        let status = match summary.kind {
            ChatKind::User => presence_text(summary.presence, Local::now()),
            ChatKind::Bot => "bot".to_string(),
            ChatKind::Group | ChatKind::Channel => String::new(),
            ChatKind::Saved => "saved messages".to_string(),
        };
        self.set_base_status(&status, summary.presence == Presence::Online);
    }

    pub fn set_chat_info(&self, info: &ChatInfo) {
        let status = match info.kind {
            ChatKind::Group => info
                .members
                .map(|count| format!("{count} members"))
                .unwrap_or_default(),
            ChatKind::Channel => info
                .members
                .map(|count| format!("{count} subscribers"))
                .unwrap_or_default(),
            ChatKind::User => presence_text(info.presence, Local::now()),
            ChatKind::Bot => "bot".to_string(),
            ChatKind::Saved => "saved messages".to_string(),
        };
        self.set_base_status(&status, info.presence == Presence::Online);
    }

    pub fn set_virtual_header(&self, chat_id: i64, tg: &Tg) {
        let (title, initials_name) = if chat_id == super::virtual_chat::ASSISTANT_CHAT {
            ("Assistant", "AI")
        } else {
            ("Omarchy", "OM")
        };
        self.inner.header_title.set_label(title);
        self.inner
            .header_avatar
            .bind(tg, chat_id, initials_name, false);
        self.inner.chat_kind.set(ChatKind::Bot);
        self.inner.header_summary.borrow_mut().take();
        self.set_base_status("local", false);
    }

    pub fn set_presence(&self, presence: Presence) {
        let status = presence_text(presence, Local::now());
        self.set_base_status(&status, presence == Presence::Online);
        if let Some(summary) = self.inner.header_summary.borrow_mut().as_mut() {
            summary.presence = presence;
        }
    }

    fn set_base_status(&self, status: &str, online: bool) {
        *self.inner.base_status.borrow_mut() = status.to_string();
        if !self.inner.typing.has_css_class("omg-typing") {
            self.inner.typing.set_label(status);
            self.inner.typing.set_visible(true);
        }
        self.inner.online_dot.set_visible(online);
        if online {
            self.inner.typing.add_css_class("omg-online");
        } else {
            self.inner.typing.remove_css_class("omg-online");
        }
    }

    pub fn set_read_outbox(&self, max_id: i32) {
        let max_id = self.inner.read_outbox.get().max(max_id);
        self.inner.read_outbox.set(max_id);
        let rows = {
            let store = self.inner.store.borrow();
            store
                .entries
                .values()
                .map(|entry| {
                    (
                        entry.msg.id,
                        entry.msg.outgoing,
                        self.inner.pending_messages.borrow().contains(&entry.msg.id),
                        entry.row.receipt.clone(),
                    )
                })
                .collect::<Vec<_>>()
        };
        for (id, outgoing, pending, receipt) in rows {
            receipt.set_visible(outgoing);
            if outgoing {
                receipt.set_label(receipt_glyph(pending, id, max_id));
            }
        }
    }

    pub fn header_menu_open(&self) -> bool {
        self.inner.header_popover.is_open()
    }

    pub fn open_header_menu(&self) {
        Self::show_header_menu(&self.inner, &self.inner.header_more);
    }

    pub fn open_jump_calendar(&self) {
        if self.inner.header_summary.borrow().is_some() {
            Self::show_jump_calendar(&self.inner);
        }
    }

    pub fn dismiss_header_menu(&self) {
        self.inner.header_popover.dismiss();
    }

    pub fn dismiss_owned_popovers(&self) {
        self.move_focus_before_removal(&self.widget);
        self.dismiss_row_popovers();
        self.inner.reaction_popover.dismiss();
        self.inner.header_popover.dismiss();
        self.inner.pinned_popover.dismiss();
        self.inner.composer_popover.dismiss();
    }

    pub fn has_sender_name(&self) -> bool {
        self.inner
            .store
            .borrow()
            .entries
            .values()
            .any(|entry| entry.row.sender.is_visible())
    }

    pub fn header_title(&self) -> String {
        self.inner.header_title.label().to_string()
    }

    pub fn header_avatar_key(&self) -> i64 {
        self.inner.header_avatar.key()
    }

    pub fn bubble_metrics(&self) -> (i32, i32) {
        let max_bubble = self
            .inner
            .store
            .borrow()
            .entries
            .values()
            // The clamp fills the row; its child is the visible bubble.
            .map(|entry| {
                entry
                    .row
                    .widget
                    .first_child()
                    .map(|c| c.width())
                    .unwrap_or(0)
            })
            .max()
            .unwrap_or(0);
        (max_bubble, self.inner.scroll.width())
    }

    pub fn set_probe_pane_width(&self, width: Option<i32>) {
        self.inner.probe_pane_width.set(width);
        let width = width.unwrap_or_else(|| self.widget.width());
        apply_pane_width(&self.inner, width);
    }

    pub fn animate_chat_switched(&self) {
        let rows: Vec<gtk::Widget> = {
            let store = self.inner.store.borrow();
            store
                .order
                .iter()
                .filter_map(|id| {
                    store
                        .entries
                        .get(id)
                        .map(|entry| entry.row.widget.clone().upcast())
                })
                .collect()
        };
        let sources = self.inner.effects.chat_switched(&self.inner.list, &rows);
        let store = self.inner.store.borrow();
        for (widget, source) in sources {
            if let Some(row) = store.entries.values().find_map(|entry| {
                (entry.row.widget.clone().upcast::<gtk::Widget>() == widget)
                    .then(|| entry.row.clone())
            }) {
                row.animation_sources.borrow_mut().push(source);
            } else {
                remove_source_if_present(source);
            }
        }
    }

    pub fn mark_recent_incoming(&self) {
        if let Some(source) = self.inner.presence_timeout.borrow_mut().take() {
            remove_source_if_present(source);
        }
        self.inner.presence_recent.set(true);
        self.inner
            .effects
            .recent_presence(self.inner.online_dot.upcast_ref(), true);
        let inner = Rc::downgrade(&self.inner);
        let source = glib::timeout_add_local_once(std::time::Duration::from_secs(300), move || {
            if let Some(inner) = inner.upgrade() {
                inner.presence_timeout.borrow_mut().take();
                inner.presence_recent.set(false);
                inner.online_dot.set_visible(false);
            }
        });
        *self.inner.presence_timeout.borrow_mut() = Some(source);
    }

    pub fn clear_recent_presence(&self) {
        if let Some(source) = self.inner.presence_timeout.borrow_mut().take() {
            remove_source_if_present(source);
        }
        self.inner.presence_recent.set(false);
        self.inner.online_dot.set_visible(false);
    }

    pub fn refresh_animations(&self) {
        self.inner.effects.composer_idle(
            self.inner.composer_cursor.upcast_ref(),
            self.composer_text().is_empty(),
        );
        self.inner.effects.recent_presence(
            self.inner.online_dot.upcast_ref(),
            self.inner.presence_recent.get(),
        );
        if !self.inner.effects.on("equalizer") {
            self.inner
                .effects
                .composer_typing(&self.inner.equalizer, false);
        }
        if self.is_empty_state() {
            self.inner
                .effects
                .empty_state(&self.inner.empty_effects, true);
        }
    }

    pub fn send_button(&self) -> gtk::Button {
        self.inner.send.clone()
    }

    pub fn start_send_feedback(&self) {
        self.stop_send_feedback();
        *self.inner.send_spin.borrow_mut() =
            self.inner.effects.send_started(&self.inner.send_label);
    }

    pub fn stop_send_feedback(&self) {
        if let Some(source) = self.inner.send_spin.borrow_mut().take() {
            remove_source_if_present(source);
        }
        self.inner.send_label.set_label("Send");
    }

    pub fn set_monospace(&self, msg_id: i32, monospace: bool) {
        let row = self
            .inner
            .store
            .borrow()
            .entries
            .get(&msg_id)
            .map(|entry| entry.row.widget.clone());
        if let Some(row) = row {
            if monospace {
                row.add_css_class("omg-msg-mono");
            } else {
                row.remove_css_class("omg-msg-mono");
            }
        }
    }

    pub fn is_monospace(&self, msg_id: i32) -> bool {
        self.inner
            .store
            .borrow()
            .entries
            .get(&msg_id)
            .is_some_and(|entry| entry.row.widget.has_css_class("omg-msg-mono"))
    }

    pub fn render_aux(
        &self,
        msg_id: i32,
        transcript: Option<&str>,
        translation: Option<&str>,
        summary: Option<&str>,
    ) -> bool {
        let slot = self
            .inner
            .store
            .borrow()
            .entries
            .get(&msg_id)
            .map(|entry| entry.row.aux_slot.clone());
        let Some(slot) = slot else { return false };
        self.move_focus_before_removal(&slot);
        while let Some(child) = slot.first_child() {
            slot.remove(&child);
        }
        for (prefix, value) in [
            ("transcript: ", transcript),
            ("translation: ", translation),
            ("summary: ", summary),
        ] {
            if let Some(value) = value {
                slot.append(&aux_label(&format!("{prefix}{value}"), false));
            }
        }
        true
    }

    pub fn show_aux_error(&self, msg_id: i32, message: &str) -> bool {
        let slot = self
            .inner
            .store
            .borrow()
            .entries
            .get(&msg_id)
            .map(|entry| entry.row.aux_slot.clone());
        let Some(slot) = slot else { return false };
        slot.append(&aux_label(message, true));
        true
    }

    pub fn clear_aux(&self, msg_id: i32) {
        let slot = self
            .inner
            .store
            .borrow()
            .entries
            .get(&msg_id)
            .map(|entry| entry.row.aux_slot.clone());
        if let Some(slot) = slot {
            self.move_focus_before_removal(&slot);
            while let Some(child) = slot.first_child() {
                slot.remove(&child);
            }
        }
    }

    pub fn clear_history_probe(&self) {
        self.inner.history_version_count.set(0);
        self.inner.history_current_text.borrow_mut().take();
    }

    pub fn show_edit_history(&self, msg_id: i32, versions: Vec<MsgVersion>) -> bool {
        let (row, current_text) = {
            let store = self.inner.store.borrow();
            let Some(entry) = store.entries.get(&msg_id) else {
                return false;
            };
            if entry.msg.deleted {
                return false;
            }
            (entry.row.widget.clone(), entry.msg.text.clone())
        };

        let version_count = versions.len();
        row.grab_focus();
        Self::dismiss_popover(&self.inner.history_popover);
        let popover = gtk::Popover::new();
        popover.add_css_class("omg-history");
        popover.set_has_arrow(false);
        popover.set_parent(&row);
        popover.connect_closed(|popover| {
            if popover.parent().is_some() {
                popover.unparent();
            }
        });
        *self.inner.history_popover.borrow_mut() = Some(popover.clone());
        let contents = gtk::Box::new(gtk::Orientation::Vertical, 0);
        popover.set_child(Some(&contents));

        if versions.is_empty() {
            let empty = gtk::Label::new(Some("no earlier versions"));
            empty.add_css_class("omg-empty-state");
            empty.set_halign(gtk::Align::Start);
            empty.set_selectable(true);
            contents.append(&empty);
            self.inner.history_current_text.borrow_mut().take();
        } else {
            let format = self.inner.time_format.borrow().clone();
            for version in versions {
                let time = format_time(&version.replaced_at, &format)
                    .or_else(|| format_time(&version.replaced_at, "%H:%M"))
                    .unwrap_or_default();
                contents.append(&history_row(&time, &version.text));
            }
            contents.append(&history_row("current", &current_text));
            *self.inner.history_current_text.borrow_mut() = Some(current_text);
        }
        self.inner.history_version_count.set(version_count);
        popover.popup();
        true
    }

    fn dismiss_popover(slot: &RefCell<Option<gtk::Popover>>) {
        let popover = slot.borrow_mut().take();
        if let Some(popover) = popover {
            popover.popdown();
            if popover.parent().is_some() {
                popover.unparent();
            }
        }
    }

    pub fn dismiss_row_popovers(&self) {
        // Popovers are parented to message rows. Move focus to a stable widget
        // before either the popover or its row can be torn down.
        self.move_focus_before_removal(&self.inner.list);
        Self::dismiss_popover(&self.inner.context_popover);
        Self::dismiss_popover(&self.inner.history_popover);
        self.inner.reaction_popover.dismiss();
        self.inner.probe_reaction_chooser.borrow_mut().take();
    }

    fn move_focus_before_removal(&self, subtree: &impl IsA<gtk::Widget>) {
        let Some(root) = self.widget.root() else {
            return;
        };
        let Some(focus) = root.focus() else {
            return;
        };
        let subtree = subtree.as_ref();
        if focus != subtree.clone() && !focus.is_ancestor(subtree) {
            return;
        }
        if !self.inner.composer.grab_focus() {
            root.set_focus(None::<&gtk::Widget>);
        }
    }

    pub fn history_version_count(&self) -> usize {
        self.inner.history_version_count.get()
    }

    pub fn history_current_text(&self) -> Option<String> {
        self.inner.history_current_text.borrow().clone()
    }

    pub fn initial_render_count(&self) -> u64 {
        self.inner.initial_render_count.get()
    }

    pub fn ids_unique(&self) -> bool {
        let store = self.inner.store.borrow();
        let mut ids = std::collections::HashSet::new();
        store.order.iter().all(|id| ids.insert(*id)) && store.order.len() == store.entries.len()
    }

    pub fn rendered_row_count(&self) -> usize {
        self.inner
            .store
            .borrow()
            .entries
            .values()
            .filter(|entry| entry.row.widget.parent().is_some())
            .count()
    }

    /// Detached = viewing a jumped-to historical page; the ▼ button stays
    /// visible and reloads the latest page.
    pub fn set_detached(&self, detached: bool) {
        self.inner.detached.set(detached);
        if !detached {
            self.inner.bottom_unread.set(0);
            self.inner.bottom_badge.set_visible(false);
        }
        self.inner.bottom_button.set_visible(detached);
    }

    pub fn is_detached(&self) -> bool {
        self.inner.detached.get()
    }

    /// Drives the same code path as clicking the ▼ button.
    pub fn trigger_jump_to_latest(&self) {
        self.inner.bottom_button.emit_clicked();
    }

    pub fn scroll_to_message(&self, msg_id: i32) -> bool {
        self.scroll_to_message_impl(msg_id, true)
    }

    pub fn scroll_to_search_result(&self, msg_id: i32) -> bool {
        self.scroll_to_message_impl(msg_id, false)
    }

    fn scroll_to_message_impl(&self, msg_id: i32, focus: bool) -> bool {
        let row = self
            .inner
            .store
            .borrow()
            .entries
            .get(&msg_id)
            .map(|entry| entry.row.widget.clone());
        let Some(row) = row else { return false };
        let Some(bounds) = row.compute_bounds(&self.inner.list) else {
            return false;
        };
        let adjustment = self.inner.scroll.vadjustment();
        adjustment.set_value(
            (f64::from(bounds.y()) - adjustment.page_size() / 3.0)
                .clamp(0.0, (adjustment.upper() - adjustment.page_size()).max(0.0)),
        );
        if focus {
            row.grab_focus();
        }
        true
    }

    /// Time label text of the newest message in the store (probe helper).
    pub fn last_time_label(&self) -> Option<String> {
        let store = self.inner.store.borrow();
        store
            .order
            .last()
            .and_then(|id| store.entries.get(id))
            .map(|entry| entry.row.time.label().to_string())
    }
}

fn apply_pane_width(inner: &MessagesInner, width: i32) {
    if width > 0 && inner.pane_width.replace(width) != width {
        for entry in inner.store.borrow().entries.values() {
            entry.row.widget.queue_resize();
        }
    }
}

fn update_date_chip_label(inner: &MessagesInner, adjustment: &gtk::Adjustment) {
    let visible_top = adjustment.value() as f32;
    let store = inner.store.borrow();
    let first_visible = store
        .order
        .iter()
        .filter_map(|id| store.entries.get(id))
        .find(|entry| {
            entry
                .row
                .widget
                .compute_bounds(&inner.list)
                .is_some_and(|bounds| bounds.y() + bounds.height() >= visible_top)
        })
        .or_else(|| store.order.last().and_then(|id| store.entries.get(id)));
    let Some(message) = first_visible.map(|entry| &entry.msg) else {
        inner.date_chip.set_label("");
        return;
    };
    let text = if message.ts.date_naive() == Local::now().date_naive() {
        "today".to_string()
    } else {
        message.ts.format("%b %-d, %Y").to_string()
    };
    inner.date_chip.set_label(&text);
}

fn remove_source_if_present(source_id: glib::SourceId) {
    if let Some(source) = glib::MainContext::default().find_source_by_id(&source_id) {
        source.destroy();
    }
}

fn set_time_label(label: &gtk::Label, message: &Msg, format: &str) {
    let edited = if message.edited { " edited" } else { "" };
    // Never let an invalid strftime panic the formatter: fall back to "%H:%M".
    let text = format_time(&message.ts, format)
        .or_else(|| format_time(&message.ts, "%H:%M"))
        .unwrap_or_default();
    label.set_label(&format!("{text}{edited}"));
}

/// Render a timestamp, returning None instead of panicking: chrono's Display
/// formatter fails with fmt::Error on invalid directives, which `to_string()`
/// would turn into a panic. `write!` surfaces the error.
fn format_time(ts: &DateTime<Local>, format: &str) -> Option<String> {
    use std::fmt::Write;
    let mut rendered = String::new();
    write!(rendered, "{}", ts.format(format))
        .ok()
        .map(|_| rendered)
}

/// Probe a strftime format by writing a fixed timestamp into a String; an
/// invalid directive yields Err instead of panicking later during rendering.
fn valid_time_format(format: &str) -> bool {
    use std::fmt::Write;
    let Some(fixed) = DateTime::<chrono::Utc>::from_timestamp(946_782_345, 0) else {
        return false;
    };
    let mut rendered = String::new();
    write!(rendered, "{}", fixed.format(format)).is_ok()
}

fn presence_text(presence: Presence, now: DateTime<Local>) -> String {
    match presence {
        Presence::Online => "Online".to_string(),
        Presence::LastSeen(time) if time.date_naive() == now.date_naive() => {
            format!("last seen at {}", time.format("%H:%M"))
        }
        Presence::LastSeen(time) => format!("last seen {}", time.format("%d.%m.%y")),
        Presence::Recently => "last seen recently".to_string(),
        Presence::LastWeek => "last seen within a week".to_string(),
        Presence::LastMonth => "last seen within a month".to_string(),
        Presence::LongAgo => "last seen a long time ago".to_string(),
        Presence::Unknown => String::new(),
    }
}

fn sender_identity(message: &Msg) -> String {
    message
        .sender_id
        .map(|id| id.to_string())
        .unwrap_or_else(|| message.sender.clone())
}

fn sender_color_index(message: &Msg) -> usize {
    if let Some(sender_id) = message.sender_id {
        return sender_id.rem_euclid(7) as usize;
    }
    let hash = message
        .sender
        .bytes()
        .fold(1_469_598_103_934_665_603_u64, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(1_099_511_628_211)
        });
    (hash % 7) as usize
}

pub fn day_separator_label_at(date: chrono::NaiveDate, today: chrono::NaiveDate) -> String {
    if date == today {
        "Today".to_string()
    } else if date == today - chrono::Duration::days(1) {
        "Yesterday".to_string()
    } else if date.year() == today.year() {
        date.format("%B %-d").to_string()
    } else {
        date.format("%-d %B %Y").to_string()
    }
}

/// 23:59:59 local on the calendar's selected day.
fn calendar_day_end(calendar: &gtk::Calendar) -> Option<DateTime<Local>> {
    let date = calendar.date();
    let naive = chrono::NaiveDate::from_ymd_opt(
        date.year(),
        date.month() as u32,
        date.day_of_month() as u32,
    )?
    .and_hms_opt(23, 59, 59)?;
    naive.and_local_timezone(Local).earliest()
}

fn message_label(text: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.set_halign(gtk::Align::Start);
    label.set_selectable(true);
    // Pointer selection does not require this label to enter the keyboard
    // NOTE: selectable labels must stay keyboard-focusable. Making them
    // `can_focus(false)` breaks GTK4's focus bookkeeping when a popover pops
    // up on their row (Gtk-CRITICAL gtk_widget_is_ancestor, reproduced 6/6).
    // Teardown safety comes from move_focus_before_removal instead.
    label.set_wrap(true);
    label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    label.set_max_width_chars(60);
    label
}

fn formatted_message_label(
    message: &Msg,
    revealed: &Rc<RefCell<HashSet<(usize, usize)>>>,
    action: &Rc<RefCell<Option<Rc<dyn Fn(MessageAction)>>>>,
    darker_background: &str,
) -> gtk::Label {
    let label = message_label(&message.text);
    apply_message_markup(&label, message, revealed, darker_background);
    let link_action = action.clone();
    let msg_id = message.id;
    label.connect_activate_link(move |_, target| {
        let message_action = if let Some(target) = target.strip_prefix("omg-user:") {
            target.parse::<i64>().ok().map(MessageAction::OpenMention)
        } else if markup::is_allowed_link(target) {
            Some(MessageAction::OpenLink(target.to_string()))
        } else {
            None
        };
        if let (Some(callback), Some(message_action)) =
            (link_action.borrow().as_ref().cloned(), message_action)
        {
            callback(message_action);
        }
        glib::Propagation::Stop
    });
    let spoiler_ranges = message
        .spans
        .iter()
        .filter_map(|span| matches!(span.kind, SpanKind::Spoiler).then_some((span.start, span.end)))
        .collect::<Vec<_>>();
    if !spoiler_ranges.is_empty() {
        let gesture = gtk::GestureClick::new();
        gesture.set_name(Some("omg-spoiler-gesture"));
        gesture.set_button(1);
        gesture.set_propagation_phase(gtk::PropagationPhase::Capture);
        let action = action.clone();
        let revealed = revealed.clone();
        let hit_label = label.clone();
        let plain_text = message.text.clone();
        gesture.connect_pressed(move |gesture, _, x, y| {
            let (offset_x, offset_y) = hit_label.layout_offsets();
            let layout_x = ((x - f64::from(offset_x)) * f64::from(gtk::pango::SCALE)) as i32;
            let layout_y = ((y - f64::from(offset_y)) * f64::from(gtk::pango::SCALE)) as i32;
            let (inside, byte_index, trailing) = hit_label.layout().xy_to_index(layout_x, layout_y);
            if !inside || byte_index < 0 {
                return;
            }
            let byte_index = (byte_index as usize).min(plain_text.len());
            // Pango reports a trailing edge as the position *after* the
            // grapheme. Map that edge back onto the grapheme itself so the
            // last character of a half-open spoiler range remains clickable.
            let trailing = trailing.max(0) as usize;
            let character = plain_text[..byte_index].chars().count()
                + trailing.saturating_sub(usize::from(trailing > 0));
            let hidden = spoiler_ranges
                .iter()
                .copied()
                .filter(|(start, end)| {
                    *start <= character
                        && character < *end
                        && !revealed.borrow().contains(&(*start, *end))
                })
                .collect::<Vec<_>>();
            if hidden.is_empty() {
                return;
            }
            // The first click reveals the concealed text and must not also
            // activate a link that overlaps it.
            if gesture.current_sequence().is_some() {
                gesture.set_state(gtk::EventSequenceState::Claimed);
            }
            if let Some(callback) = action.borrow().as_ref().cloned() {
                for (start, end) in hidden {
                    callback(MessageAction::RevealSpoiler { msg_id, start, end });
                }
            }
        });
        label.add_controller(gesture);
    }
    label
}

fn apply_message_markup(
    label: &gtk::Label,
    message: &Msg,
    revealed: &Rc<RefCell<HashSet<(usize, usize)>>>,
    darker_background: &str,
) {
    let rendered = markup::render_with_code_background(
        &message.text,
        &message.spans,
        &revealed.borrow(),
        darker_background,
    );
    label.set_markup(&rendered.markup);
    if rendered.has_spoiler {
        label.add_css_class("omg-spoiler");
        label.set_cursor_from_name(Some("pointer"));
    } else {
        label.remove_css_class("omg-spoiler");
        label.set_cursor_from_name(None);
    }
}

fn set_text_block_style(block: &gtk::Box, message: &Msg) {
    let text_len = message.text.chars().count();
    let full_pre = message.spans.iter().any(|span| {
        matches!(span.kind, SpanKind::Pre(_)) && span.start == 0 && span.end == text_len
    });
    let full_quote = message.spans.iter().any(|span| {
        matches!(span.kind, SpanKind::Blockquote) && span.start == 0 && span.end == text_len
    });
    if full_pre {
        block.add_css_class("omg-pre-block");
    } else {
        block.remove_css_class("omg-pre-block");
    }
    if full_quote {
        block.add_css_class("omg-blockquote");
    } else {
        block.remove_css_class("omg-blockquote");
    }
}

fn render_web_preview(
    slot: &gtk::Box,
    message: &Msg,
    action: &Rc<RefCell<Option<Rc<dyn Fn(MessageAction)>>>>,
) {
    while let Some(child) = slot.first_child() {
        slot.remove(&child);
    }
    let Some(preview) = message.webpage.as_ref() else {
        slot.set_visible(false);
        return;
    };
    let button = gtk::Button::new();
    button.add_css_class("omg-web-preview");
    let content = gtk::Box::new(gtk::Orientation::Vertical, 2);
    let site = gtk::Label::new(Some(&preview.site_name));
    site.add_css_class("omg-small");
    site.set_halign(gtk::Align::Start);
    content.append(&site);
    let title = gtk::Label::new(Some(&preview.title));
    title.add_css_class("omg-web-title");
    title.set_halign(gtk::Align::Start);
    title.set_wrap(true);
    content.append(&title);
    let description = gtk::Label::new(Some(&preview.description));
    description.add_css_class("omg-muted");
    description.set_halign(gtk::Align::Start);
    description.set_wrap(true);
    description.set_ellipsize(gtk::pango::EllipsizeMode::End);
    description.set_lines(3);
    content.append(&description);
    button.set_child(Some(&content));
    let url = preview.url.clone();
    let action = action.clone();
    button.connect_clicked(move |_| {
        if let Some(callback) = action.borrow().as_ref().cloned() {
            callback(MessageAction::OpenLink(url.clone()));
        }
    });
    slot.append(&button);
    slot.set_visible(true);
}

fn media_title(message: &Msg) -> String {
    match message.media {
        Some(MediaKind::Photo) => "Photo".to_string(),
        Some(MediaKind::Sticker) => "Sticker".to_string(),
        Some(MediaKind::Voice) => "Voice message".to_string(),
        Some(MediaKind::Document) => message
            .doc_name
            .clone()
            .unwrap_or_else(|| "Document".into()),
        Some(MediaKind::Video) => message.doc_name.clone().unwrap_or_else(|| "Video".into()),
        Some(MediaKind::Gif) => message.doc_name.clone().unwrap_or_else(|| "GIF".into()),
        Some(MediaKind::Audio) => message.doc_name.clone().unwrap_or_else(|| "Audio".into()),
        Some(MediaKind::VideoNote) => "Video message".to_string(),
        Some(MediaKind::Unsupported) => "Unsupported message".to_string(),
        None => "Message".to_string(),
    }
}

fn media_card(message: &Msg) -> gtk::Button {
    let button = gtk::Button::new();
    button.add_css_class("omg-doc-pill");
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let glyph = match message.media {
        Some(MediaKind::Gif) => icons::GIF,
        Some(MediaKind::Voice | MediaKind::Audio) => icons::MIC,
        Some(MediaKind::Video | MediaKind::VideoNote) => icons::IMAGE,
        _ => icons::FILE,
    };
    let icon = gtk::Label::new(Some(glyph));
    icon.add_css_class("omg-muted");
    content.append(&icon);
    let copy = gtk::Box::new(gtk::Orientation::Vertical, 0);
    copy.set_hexpand(true);
    let title = gtk::Label::new(Some(&media_title(message)));
    title.set_halign(gtk::Align::Start);
    title.set_ellipsize(gtk::pango::EllipsizeMode::End);
    title.set_max_width_chars(36);
    copy.append(&title);
    let details = media_details(message);
    if !details.is_empty() {
        let details = gtk::Label::new(Some(&details));
        details.add_css_class("omg-small");
        details.set_halign(gtk::Align::Start);
        copy.append(&details);
    }
    content.append(&copy);
    let download = gtk::Label::new(Some("Download"));
    download.add_css_class("omg-small");
    content.append(&download);
    button.set_child(Some(&content));
    button
}

fn media_details(message: &Msg) -> String {
    let mut details = Vec::new();
    if let Some(duration) = message.duration {
        details.push(format!("{}:{:02}", duration / 60, duration % 60));
    }
    if let Some(size) = message.doc_size {
        details.push(if size >= 1_000_000 {
            format!("{:.1} MB", size as f64 / 1_000_000.0)
        } else if size >= 1_000 {
            format!("{:.1} KB", size as f64 / 1_000.0)
        } else {
            format!("{size} B")
        });
    }
    details.join(" · ")
}

pub fn search_position(index: Option<usize>, total: usize) -> String {
    match (index, total) {
        (Some(index), total) if index < total => format!("{} of {total}", index + 1),
        _ => "No results".to_string(),
    }
}

fn aux_label(text: &str, error: bool) -> gtk::Label {
    let label = message_label(text);
    label.add_css_class(if error { "omg-error" } else { "omg-msg-quote" });
    label
}

fn update_reactions(
    row: &MessageRow,
    message: &Msg,
    effects: &Effects,
    action: &Rc<RefCell<Option<Rc<dyn Fn(MessageAction)>>>>,
    animate: bool,
) {
    let mut buttons = row.reaction_buttons.borrow_mut();
    while buttons.len() < message.reactions.len() {
        let button = gtk::Button::new();
        button.add_css_class("omg-reaction");
        let action = action.clone();
        let msg_id = message.id;
        button.connect_clicked(move |button| {
            let emoji = button.tooltip_text().map(|text| text.to_string());
            if let (Some(callback), Some(emoji)) = (action.borrow().as_ref().cloned(), emoji) {
                callback(MessageAction::Reaction {
                    msg_id,
                    emoji: Some(emoji),
                });
            }
        });
        let previous = buttons.last().cloned();
        row.reactions.insert_child_after(&button, previous.as_ref());
        buttons.push(button);
    }
    for (index, button) in buttons.iter().enumerate() {
        let Some(reaction) = message.reactions.get(index) else {
            button.set_visible(false);
            continue;
        };
        let text = if reaction.count == 1 {
            reaction.emoji.clone()
        } else {
            format!("{} {}", reaction.emoji, reaction.count)
        };
        let label = button
            .child()
            .and_downcast::<gtk::Label>()
            .unwrap_or_else(|| {
                let label = gtk::Label::new(None);
                button.set_child(Some(&label));
                label
            });
        let changed = label.label().as_str() != text;
        let rolling = animate && changed && effects.reaction_changed(&label, &text);
        if !rolling {
            label.set_label(&text);
        }
        button.set_tooltip_text(Some(&reaction.emoji));
        button.set_visible(true);
        if reaction.chosen {
            button.add_css_class("omg-reaction-chosen");
        } else {
            button.remove_css_class("omg-reaction-chosen");
        }
        if animate && changed {
            row.animation_sources
                .borrow_mut()
                .extend(effects.reaction_added(button.upcast_ref()));
        }
    }
    row.reaction_anchor.set_visible(true);
    row.reactions.set_visible(true);
}

fn set_deleted_rendering(row: &MessageRow, deleted: bool) {
    if deleted {
        row.widget.add_css_class("omg-msg-deleted");
    } else {
        row.widget.remove_css_class("omg-msg-deleted");
    }
    row.deleted_tag.set_visible(deleted);
}

fn receipt_glyph(pending: bool, id: i32, read_outbox: i32) -> &'static str {
    if pending {
        icons::CLOCK
    } else if id <= read_outbox {
        icons::CHECK_DOUBLE
    } else {
        icons::CHECK
    }
}

fn history_row(time: &str, text: &str) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Vertical, 4);
    row.add_css_class("omg-history-row");
    let time = gtk::Label::new(Some(time));
    time.add_css_class("omg-msg-time");
    time.set_halign(gtk::Align::Start);
    row.append(&time);
    let text = message_label(text);
    row.append(&text);
    row
}

fn menu_button(label: &str, danger: bool) -> gtk::Button {
    let button = gtk::Button::with_label(label);
    button.add_css_class("omg-menu-item");
    if danger {
        button.add_css_class("omg-danger");
    }
    button.set_halign(gtk::Align::Fill);
    button
}

fn snippet(text: &str, max_chars: usize) -> String {
    let mut out: String = text.chars().take(max_chars).collect();
    if text.chars().count() > max_chars {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod wave5_tests {
    use chrono::NaiveDate;

    use super::{SelectionBook, bubble_width_limit, day_separator_label_at, search_position};

    #[test]
    fn day_separator_labels_cover_relative_and_absolute_dates() {
        let today = NaiveDate::from_ymd_opt(2026, 9, 2).unwrap();
        assert_eq!(day_separator_label_at(today, today), "Today");
        assert_eq!(
            day_separator_label_at(NaiveDate::from_ymd_opt(2026, 9, 1).unwrap(), today),
            "Yesterday"
        );
        assert_eq!(
            day_separator_label_at(NaiveDate::from_ymd_opt(2026, 8, 20).unwrap(), today),
            "August 20"
        );
        assert_eq!(
            day_separator_label_at(NaiveDate::from_ymd_opt(2025, 9, 1).unwrap(), today),
            "1 September 2025"
        );
    }

    #[test]
    fn bubble_width_is_capped_by_pane_fraction_and_pixels() {
        assert_eq!(bubble_width_limit(Some(600)), 396);
        assert_eq!(bubble_width_limit(Some(1200)), 520);
        assert_eq!(bubble_width_limit(None), 520);
    }

    #[test]
    fn in_chat_search_position_is_one_based_and_handles_empty_results() {
        assert_eq!(search_position(None, 0), "No results");
        assert_eq!(search_position(Some(0), 3), "1 of 3");
        assert_eq!(search_position(Some(2), 3), "3 of 3");
        assert_eq!(search_position(Some(3), 3), "No results");
    }

    #[test]
    fn selection_book_drops_deleted_ids_and_keeps_display_order() {
        let mut selection = SelectionBook::default();
        selection.set(30, true);
        selection.set(10, true);
        selection.set(20, true);
        assert_eq!(selection.ordered(&[10, 20, 30]), vec![10, 20, 30]);
        assert!(selection.remove_deleted(&[20, 99]));
        assert_eq!(selection.ordered(&[10, 20, 30]), vec![10, 30]);
        assert!(!selection.remove_deleted(&[20, 99]));
    }
}
