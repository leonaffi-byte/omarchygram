//! Location sharing dialog (spec-wave6 §4.3). Pattern: `newgroup.rs`
//! (`new()` / `widget` / `set_action`). It emits
//! `LocationDialogAction::Send { point, live_secs }` and the shell calls
//! `tg.send_location` / `tg.send_live_location`.
//!
//! It holds a `Tg` handle for one read-only purpose: fetching the preview
//! tiles with `download_map`. Every state-changing call stays in the shell.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use gtk::gdk;
use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use gtk4 as gtk;

use crate::tg::{GeoPoint, Tg};

use super::icons;

#[derive(Clone, Debug)]
pub enum LocationDialogAction {
    Close,
    Send {
        point: GeoPoint,
        live_secs: Option<u32>,
    },
}

type Callback = Rc<dyn Fn(LocationDialogAction)>;

const DEFAULT_POINT: GeoPoint = GeoPoint {
    lat: 52.52,
    lon: 13.405,
};
const DEFAULT_ZOOM: u8 = 12;
const MIN_ZOOM: u8 = 3;
const MAX_ZOOM: u8 = 17;
const CELL: u32 = 96;
/// "Share live for" entries: label plus the period `send_live_location` wants.
const LIVE_CHOICES: [(&str, Option<u32>); 4] = [
    ("Off", None),
    ("15 min", Some(900)),
    ("1 h", Some(3_600)),
    ("8 h", Some(28_800)),
];

pub struct LocationDialog {
    pub widget: gtk::Box,
    lat: gtk::Entry,
    lon: gtk::Entry,
    error: gtk::Label,
    map_error: gtk::Label,
    grid: gtk::Grid,
    cells: RefCell<Vec<gtk::Picture>>,
    zoom_label: gtk::Label,
    zoom_in: gtk::Button,
    zoom_out: gtk::Button,
    heading_label: gtk::Label,
    live_row: gtk::Box,
    live: gtk::DropDown,
    send: gtk::Button,
    tg: Tg,
    point: Cell<GeoPoint>,
    zoom: Cell<u8>,
    map_tiles: Cell<bool>,
    is_update: Cell<bool>,
    /// Bumped on every open/close and every re-centre: a tile that arrives
    /// for an older view is dropped instead of painted over the new one.
    generation: Cell<u64>,
    map_pending: Cell<usize>,
    map_failures: Cell<usize>,
    /// True while the entries are being written by the dialog itself, so the
    /// `changed` handler does not fight the caller.
    updating: Cell<bool>,
    busy: Cell<bool>,
    action: RefCell<Option<Callback>>,
    self_weak: RefCell<Weak<LocationDialog>>,
}

impl LocationDialog {
    pub fn new(tg: Tg) -> Rc<Self> {
        let widget = gtk::Box::new(gtk::Orientation::Vertical, 0);
        widget.add_css_class("omg-overlay-backdrop");
        widget.set_hexpand(true);
        widget.set_vexpand(true);
        widget.set_halign(gtk::Align::Fill);
        widget.set_valign(gtk::Align::Fill);
        widget.set_visible(false);

        let card = gtk::Box::new(gtk::Orientation::Vertical, 8);
        card.add_css_class("omg-location-dialog");
        card.set_halign(gtk::Align::Center);
        // vexpand + valign Center = the card floats in the middle. Without
        // the expand a GtkBox would pack it against the top edge.
        card.set_vexpand(true);
        card.set_valign(gtk::Align::Center);
        card.set_size_request(360, -1);
        widget.append(&card);

        let heading = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let heading_label = gtk::Label::new(Some("Share location"));
        heading_label.add_css_class("omg-title");
        heading_label.set_halign(gtk::Align::Start);
        heading_label.set_hexpand(true);
        heading.append(&heading_label);
        let close = gtk::Button::with_label(icons::CLOSE);
        close.add_css_class("omg-icon-button");
        close.set_tooltip_text(Some("Close"));
        heading.append(&close);
        card.append(&heading);

        let coords = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let lat = gtk::Entry::new();
        lat.set_placeholder_text(Some("Latitude"));
        lat.set_hexpand(true);
        lat.set_tooltip_text(Some("Latitude, −90 to 90"));
        let lon = gtk::Entry::new();
        lon.set_placeholder_text(Some("Longitude"));
        lon.set_hexpand(true);
        lon.set_tooltip_text(Some("Longitude, −180 to 180"));
        coords.append(&lat);
        coords.append(&lon);
        card.append(&coords);

        let error = gtk::Label::new(None);
        error.add_css_class("omg-error");
        error.set_halign(gtk::Align::Start);
        error.set_wrap(true);
        error.set_visible(false);
        card.append(&error);

        let zoom_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let zoom_out = gtk::Button::with_label("−");
        zoom_out.add_css_class("omg-icon-button");
        zoom_out.set_tooltip_text(Some("Zoom out"));
        zoom_row.append(&zoom_out);
        let zoom_label = gtk::Label::new(None);
        zoom_label.add_css_class("omg-small");
        zoom_label.add_css_class("omg-muted");
        zoom_label.set_hexpand(true);
        zoom_label.set_halign(gtk::Align::Center);
        zoom_row.append(&zoom_label);
        let zoom_in = gtk::Button::with_label("+");
        zoom_in.add_css_class("omg-icon-button");
        zoom_in.set_tooltip_text(Some("Zoom in"));
        zoom_row.append(&zoom_in);
        card.append(&zoom_row);

        let grid = gtk::Grid::new();
        grid.add_css_class("omg-map-grid");
        grid.set_row_spacing(2);
        grid.set_column_spacing(2);
        grid.set_halign(gtk::Align::Center);
        card.append(&grid);

        let map_error = gtk::Label::new(None);
        map_error.add_css_class("omg-error");
        map_error.set_halign(gtk::Align::Start);
        map_error.set_wrap(true);
        map_error.set_visible(false);
        card.append(&map_error);

        let live_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let live_label = gtk::Label::new(Some("Share live for"));
        live_label.set_halign(gtk::Align::Start);
        live_label.set_hexpand(true);
        live_row.append(&live_label);
        let labels: Vec<&str> = LIVE_CHOICES.iter().map(|(label, _)| *label).collect();
        let live = gtk::DropDown::from_strings(&labels);
        live.set_halign(gtk::Align::End);
        live_row.append(&live);
        card.append(&live_row);

        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let cancel = gtk::Button::with_label("Cancel");
        cancel.add_css_class("omg-menu-item");
        cancel.set_halign(gtk::Align::Start);
        cancel.set_hexpand(true);
        let send = gtk::Button::with_label("Send");
        send.add_css_class("omg-primary");
        send.set_halign(gtk::Align::End);
        actions.append(&cancel);
        actions.append(&send);
        card.append(&actions);

        let this = Rc::new(Self {
            widget,
            lat,
            lon,
            error,
            map_error,
            grid,
            cells: RefCell::new(Vec::new()),
            zoom_label,
            zoom_in,
            zoom_out,
            heading_label,
            live_row,
            live,
            send,
            tg,
            point: Cell::new(DEFAULT_POINT),
            zoom: Cell::new(DEFAULT_ZOOM),
            map_tiles: Cell::new(true),
            is_update: Cell::new(false),
            generation: Cell::new(0),
            map_pending: Cell::new(0),
            map_failures: Cell::new(0),
            updating: Cell::new(false),
            busy: Cell::new(false),
            action: RefCell::new(None),
            self_weak: RefCell::new(Weak::new()),
        });
        *this.self_weak.borrow_mut() = Rc::downgrade(&this);

        for index in 0..9 {
            let picture = gtk::Picture::new();
            picture.set_content_fit(gtk::ContentFit::Cover);
            picture.set_size_request(CELL as i32, CELL as i32);
            let cell = gtk::Button::new();
            cell.add_css_class("omg-map-cell");
            cell.set_child(Some(&picture));
            cell.set_tooltip_text(Some("Move the pin here"));
            let weak = this.self_weak.borrow().clone();
            cell.connect_clicked(move |_| {
                if let Some(this) = weak.upgrade() {
                    this.pick_cell(index);
                }
            });
            this.grid
                .attach(&cell, (index % 3) as i32, (index / 3) as i32, 1, 1);
            this.cells.borrow_mut().push(picture);
        }

        {
            let weak = Rc::downgrade(&this);
            close.connect_clicked(move |_| {
                if let Some(this) = weak.upgrade() {
                    this.emit(LocationDialogAction::Close);
                }
            });
        }
        {
            let weak = Rc::downgrade(&this);
            cancel.connect_clicked(move |_| {
                if let Some(this) = weak.upgrade() {
                    this.emit(LocationDialogAction::Close);
                }
            });
        }
        {
            let weak = Rc::downgrade(&this);
            this.lat.connect_changed(move |_| {
                if let Some(this) = weak.upgrade() {
                    this.entries_changed();
                }
            });
        }
        {
            let weak = Rc::downgrade(&this);
            this.lon.connect_changed(move |_| {
                if let Some(this) = weak.upgrade() {
                    this.entries_changed();
                }
            });
        }
        {
            let weak = Rc::downgrade(&this);
            this.zoom_in.connect_clicked(move |_| {
                if let Some(this) = weak.upgrade() {
                    this.set_zoom(this.zoom.get().saturating_add(1));
                }
            });
        }
        {
            let weak = Rc::downgrade(&this);
            this.zoom_out.connect_clicked(move |_| {
                if let Some(this) = weak.upgrade() {
                    this.set_zoom(this.zoom.get().saturating_sub(1));
                }
            });
        }
        {
            let weak = Rc::downgrade(&this);
            this.send.connect_clicked(move |_| {
                let Some(this) = weak.upgrade() else { return };
                if this.busy.get() {
                    return;
                }
                let Some(point) = this.read_point() else { return };
                this.point.set(point);
                let live_secs = if this.is_update.get() {
                    None
                } else {
                    LIVE_CHOICES
                        .get(this.live.selected() as usize)
                        .and_then(|(_, secs)| *secs)
                };
                this.emit(LocationDialogAction::Send { point, live_secs });
            });
        }

        this
    }

    pub fn set_action(&self, callback: Callback) {
        *self.action.borrow_mut() = Some(callback);
    }

    fn emit(&self, event: LocationDialogAction) {
        let callback = self.action.borrow().as_ref().cloned();
        if let Some(callback) = callback {
            callback(event);
        }
    }

    /// `last` is the remembered point from ui-state, `map_tiles` the
    /// `settings.media.map_tiles` toggle (off → no network fetch, no grid).
    pub fn begin(&self, last: Option<(f64, f64)>, map_tiles: bool) {
        self.bump();
        self.is_update.set(false);
        self.heading_label.set_label("Share location");
        self.live_row.set_visible(true);
        self.map_tiles.set(map_tiles);
        self.busy.set(false);
        self.send.set_label("Send");
        self.error.set_label("");
        self.error.set_visible(false);
        self.map_error.set_label("");
        self.map_error.set_visible(false);
        self.live.set_selected(0);
        self.zoom.set(DEFAULT_ZOOM);
        let point = last
            .map(|(lat, lon)| GeoPoint { lat, lon })
            .filter(|point| in_range(point.lat, point.lon))
            .unwrap_or(DEFAULT_POINT);
        self.set_point(point);
        self.widget.set_visible(true);
        self.lat.grab_focus();
    }

    pub fn begin_update(&self, point: GeoPoint, map_tiles: bool) {
        self.bump();
        self.is_update.set(true);
        self.heading_label.set_label("Update position");
        self.live_row.set_visible(false);
        self.map_tiles.set(map_tiles);
        self.busy.set(false);
        self.send.set_label("Update");
        self.error.set_label("");
        self.error.set_visible(false);
        self.map_error.set_label("");
        self.map_error.set_visible(false);
        self.zoom.set(DEFAULT_ZOOM);
        self.set_point(point);
        self.widget.set_visible(true);
        self.lat.grab_focus();
    }

    pub fn close(&self) {
        if !self.widget.is_visible() {
            return;
        }
        self.bump();
        move_focus_outside(self.widget.upcast_ref());
        self.widget.set_visible(false);
        self.set_busy(false);
        self.heading_label.set_label("Share location");
        self.live_row.set_visible(true);
        self.is_update.set(false);
    }

    pub fn is_open(&self) -> bool {
        self.widget.is_visible()
    }

    pub fn set_busy(&self, busy: bool) {
        self.busy.set(busy);
        let default_label = if self.is_update.get() { "Update" } else { "Send" };
        self.send.set_label(if busy { "Sending…" } else { default_label });
        self.lat.set_sensitive(!busy);
        self.lon.set_sensitive(!busy);
        self.live.set_sensitive(!busy);
        self.grid.set_sensitive(!busy);
        self.send.set_sensitive(!busy && self.read_point().is_some());
    }

    pub fn show_error(&self, error: &str) {
        self.set_busy(false);
        self.send.set_label("Retry");
        self.error.set_label(error);
        self.error.set_visible(true);
    }

    pub fn point(&self) -> GeoPoint {
        self.point.get()
    }

    fn bump(&self) {
        self.generation.set(self.generation.get().wrapping_add(1));
    }

    /// Coordinates currently in the entries, when they parse and are in range.
    fn read_point(&self) -> Option<GeoPoint> {
        let lat = self.lat.text().trim().parse::<f64>().ok()?;
        let lon = self.lon.text().trim().parse::<f64>().ok()?;
        in_range(lat, lon).then_some(GeoPoint { lat, lon })
    }

    fn entries_changed(&self) {
        if self.updating.get() {
            return;
        }
        match self.read_point() {
            Some(point) => {
                self.point.set(point);
                self.error.set_label("");
                self.error.set_visible(false);
                self.send.set_sensitive(!self.busy.get());
                self.refresh_grid();
            }
            None => {
                self.error
                    .set_label("latitude −90..90, longitude −180..180");
                self.error.set_visible(true);
                self.send.set_sensitive(false);
            }
        }
    }

    fn set_point(&self, point: GeoPoint) {
        self.point.set(point);
        self.updating.set(true);
        self.lat.set_text(&format!("{:.5}", point.lat));
        self.lon.set_text(&format!("{:.5}", point.lon));
        self.updating.set(false);
        self.error.set_label("");
        self.error.set_visible(false);
        self.send.set_sensitive(!self.busy.get());
        self.refresh_grid();
    }

    fn set_zoom(&self, zoom: u8) {
        let zoom = zoom.clamp(MIN_ZOOM, MAX_ZOOM);
        if zoom == self.zoom.get() {
            self.update_zoom_controls();
            return;
        }
        self.zoom.set(zoom);
        self.refresh_grid();
    }

    fn update_zoom_controls(&self) {
        let zoom = self.zoom.get();
        self.zoom_label.set_label(&format!("zoom {zoom}"));
        self.zoom_out.set_sensitive(zoom > MIN_ZOOM);
        self.zoom_in.set_sensitive(zoom < MAX_ZOOM);
    }

    /// Degrees covered by one 96 px cell at the current zoom (Web Mercator).
    fn cell_span(&self) -> (f64, f64) {
        let zoom = self.zoom.get();
        let world = 256.0 * 2f64.powi(zoom as i32);
        let lon_span = 360.0 * CELL as f64 / world;
        let lat = self.point.get().lat.clamp(-85.0, 85.0);
        let lat_span = lon_span * lat.to_radians().cos();
        (lat_span, lon_span)
    }

    fn cell_center(&self, index: usize) -> GeoPoint {
        let (lat_span, lon_span) = self.cell_span();
        let column = (index % 3) as f64 - 1.0;
        let row = (index / 3) as f64 - 1.0;
        let point = self.point.get();
        GeoPoint {
            lat: (point.lat - row * lat_span).clamp(-90.0, 90.0),
            lon: wrap_lon(point.lon + column * lon_span),
        }
    }

    fn pick_cell(&self, index: usize) {
        if self.busy.get() {
            return;
        }
        self.set_point(self.cell_center(index));
    }

    fn refresh_grid(&self) {
        self.update_zoom_controls();
        self.grid.set_visible(self.map_tiles.get());
        self.map_error.set_label("");
        self.map_error.set_visible(false);
        if !self.map_tiles.get() {
            self.map_pending.set(0);
            self.map_failures.set(0);
            return;
        }
        self.bump();
        let generation = self.generation.get();
        let zoom = self.zoom.get();
        self.map_pending.set(self.cells.borrow().len());
        self.map_failures.set(0);
        for (index, picture) in self.cells.borrow().iter().enumerate() {
            let center = self.cell_center(index);
            picture.set_paintable(None::<&gdk::Paintable>);
            let picture = picture.clone();
            let tg = self.tg.clone();
            let weak = self.self_weak.borrow().clone();
            glib::MainContext::default().spawn_local(async move {
                let texture = match tg.download_map(center, zoom, CELL, CELL).await {
                    Ok(Some(path)) => {
                        gdk::Texture::from_file(&gio::File::for_path(&path)).ok()
                    }
                    Ok(None) | Err(_) => None,
                };
                let Some(this) = weak.upgrade() else { return };
                if this.generation.get() != generation {
                    return;
                }
                let loaded = texture.is_some();
                if let Some(texture) = texture {
                    picture.set_paintable(Some(&texture));
                }
                this.complete_tile(generation, loaded);
            });
        }
    }

    fn complete_tile(&self, generation: u64, loaded: bool) {
        if self.generation.get() != generation || self.map_pending.get() == 0 {
            return;
        }
        if !loaded {
            self.map_failures
                .set(self.map_failures.get().saturating_add(1));
        }
        self.map_pending
            .set(self.map_pending.get().saturating_sub(1));
        if self.map_pending.get() != 0 {
            return;
        }
        let failures = self.map_failures.get();
        if failures > 0 {
            self.map_error.set_label(&format!(
                "Map unavailable ({failures} of 9 tiles failed). You can still send or change the point to retry."
            ));
            self.map_error.set_visible(true);
        }
    }

    // ----- probe helpers -----

    pub fn probe_set_point(&self, lat: f64, lon: f64) {
        self.set_point(GeoPoint { lat, lon });
    }

    pub fn probe_type_coords(&self, lat: &str, lon: &str) {
        self.lat.set_text(lat);
        self.lon.set_text(lon);
    }

    pub fn probe_click_cell(&self, index: usize) {
        let cell = self.grid.child_at((index % 3) as i32, (index / 3) as i32);
        if let Some(button) = cell.and_downcast::<gtk::Button>() {
            button.emit_clicked();
        }
    }

    pub fn probe_zoom_in(&self) {
        self.zoom_in.emit_clicked();
    }

    pub fn probe_zoom_out(&self) {
        self.zoom_out.emit_clicked();
    }

    pub fn probe_zoom(&self) -> u8 {
        self.zoom.get()
    }

    pub fn probe_set_live(&self, index: u32) {
        self.live.set_selected(index);
    }

    pub fn probe_send(&self) {
        self.send.emit_clicked();
    }

    pub fn probe_can_send(&self) -> bool {
        self.send.is_sensitive()
    }

    pub fn probe_error(&self) -> String {
        self.error.label().to_string()
    }

    pub fn probe_grid_visible(&self) -> bool {
        self.grid.is_visible()
    }

    pub fn probe_tiles_loaded(&self) -> usize {
        self.cells
            .borrow()
            .iter()
            .filter(|picture| picture.paintable().is_some())
            .count()
    }

    pub fn probe_fail_map_fetch(&self) {
        self.bump();
        let generation = self.generation.get();
        self.map_pending.set(1);
        self.map_failures.set(0);
        self.complete_tile(generation, false);
    }

    pub fn probe_map_error(&self) -> String {
        self.map_error.label().to_string()
    }

    pub fn probe_retry_map(&self) {
        self.refresh_grid();
    }
}

fn in_range(lat: f64, lon: f64) -> bool {
    (-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon)
}

fn wrap_lon(lon: f64) -> f64 {
    let mut lon = lon;
    while lon > 180.0 {
        lon -= 360.0;
    }
    while lon < -180.0 {
        lon += 360.0;
    }
    lon
}

fn move_focus_outside(subtree: &gtk::Widget) {
    let Some(root) = subtree.root() else { return };
    let Some(focus) = root.focus() else { return };
    if focus == *subtree || focus.is_ancestor(subtree) {
        root.set_focus(None::<&gtk::Widget>);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_check_matches_the_spec() {
        assert!(in_range(52.52, 13.405));
        assert!(in_range(-90.0, 180.0));
        assert!(!in_range(90.1, 0.0));
        assert!(!in_range(0.0, -180.1));
    }

    #[test]
    fn longitude_wraps_at_the_antimeridian() {
        assert_eq!(wrap_lon(181.0), -179.0);
        assert_eq!(wrap_lon(-181.0), 179.0);
        assert_eq!(wrap_lon(13.405), 13.405);
    }
}
