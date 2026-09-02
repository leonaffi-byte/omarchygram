use std::cell::Cell;
use std::f64::consts::TAU;
use std::time::Duration;

use gtk::glib;
use gtk::prelude::*;
use gtk4 as gtk;

use super::{Effects, EffectsCore, tick_start};

#[derive(Default)]
pub(super) struct OverlayState {
    launched: bool,
    scanlines: Option<gtk::DrawingArea>,
    vignette: Option<gtk::DrawingArea>,
    flicker: Option<gtk::Box>,
    empty_host: Option<glib::WeakRef<gtk::Overlay>>,
    matrix: Option<gtk::DrawingArea>,
    grid: Option<gtk::Box>,
}

pub(super) fn sync_permanent(core: &EffectsCore) {
    let mut state = core.overlays.borrow_mut();
    if let Some(scanlines) = &state.scanlines {
        scanlines.set_visible(core.on("scanlines"));
    }
    if !core.on("vignette") {
        if let Some(vignette) = state.vignette.take() {
            remove_from_overlay(&vignette);
        }
    }
    if !core.on("flicker") {
        if let Some(flicker) = state.flicker.take() {
            remove_from_overlay(&flicker);
        }
    }
    if !core.on("matrixrain") {
        if let Some(matrix) = state.matrix.take() {
            remove_from_overlay(&matrix);
        }
    }
    if !core.on("gridshimmer") {
        if let Some(grid) = state.grid.take() {
            remove_from_overlay(&grid);
        }
    }
}

fn remove_from_overlay(widget: &impl IsA<gtk::Widget>) {
    if let Some(parent) = widget.parent().and_downcast::<gtk::Overlay>() {
        parent.remove_overlay(widget);
    }
}

fn defer_remove_from_overlay(widget: gtk::Widget) {
    let widget = widget.downgrade();
    glib::timeout_add_local_once(Duration::ZERO, move || {
        if let Some(widget) = widget.upgrade() {
            remove_from_overlay(&widget);
        }
    });
}

pub(super) fn launch(effects: &Effects, host: &gtk::Overlay) {
    effects.core.overlays.borrow_mut().launched = true;
    ensure_scanlines(effects, host);
    ensure_vignette(effects, host);
    ensure_flicker(effects, host);
    boot_log(effects, host);
    power_on(effects, host);
}

pub(super) fn refresh(effects: &Effects, host: &gtk::Overlay) {
    if !effects.core.overlays.borrow().launched {
        return;
    }
    ensure_scanlines(effects, host);
    ensure_vignette(effects, host);
    ensure_flicker(effects, host);
}

fn ensure_scanlines(effects: &Effects, host: &gtk::Overlay) {
    if !effects.on("scanlines") {
        return;
    }
    if let Some(area) = effects.core.overlays.borrow().scanlines.as_ref() {
        area.set_visible(true);
        return;
    }
    let area = gtk::DrawingArea::new();
    area.add_css_class("omg-overlay-ink");
    area.set_hexpand(true);
    area.set_vexpand(true);
    area.set_can_target(false);
    area.set_draw_func(|area, context, width, height| {
        let color = area.color();
        set_source_color(
            context,
            color.red() as f64,
            color.green() as f64,
            color.blue() as f64,
            0.16,
        );
        let mut y = 0;
        while y < height {
            context.rectangle(0.0, y as f64, width as f64, 1.0);
            y += 3;
        }
        let _ = context.fill();
    });
    host.add_overlay(&area);
    effects.core.overlays.borrow_mut().scanlines = Some(area);
}

fn ensure_vignette(effects: &Effects, host: &gtk::Overlay) {
    if !effects.on("vignette") {
        return;
    }
    if let Some(area) = effects.core.overlays.borrow().vignette.as_ref() {
        area.set_visible(true);
        return;
    }
    let area = gtk::DrawingArea::new();
    area.add_css_class("omg-overlay-ink");
    area.set_hexpand(true);
    area.set_vexpand(true);
    area.set_can_target(false);
    area.set_draw_func(|area, context, width, height| {
        let color = area.color();
        let radius = f64::from(width.max(height)).max(1.0) * 0.72;
        let gradient = gtk::cairo::RadialGradient::new(
            f64::from(width) / 2.0,
            f64::from(height) / 2.0,
            radius * 0.25,
            f64::from(width) / 2.0,
            f64::from(height) / 2.0,
            radius,
        );
        add_gradient_stop(
            gradient.as_ref(),
            0.0,
            color.red() as f64,
            color.green() as f64,
            color.blue() as f64,
            0.0,
        );
        add_gradient_stop(
            gradient.as_ref(),
            1.0,
            color.red() as f64,
            color.green() as f64,
            color.blue() as f64,
            0.74,
        );
        let _ = context.set_source(&gradient);
        let _ = context.paint();
    });
    host.add_overlay(&area);
    effects.core.overlays.borrow_mut().vignette = Some(area.clone());
    let started = Cell::new(None::<i64>);
    let area_for_tick = area.clone();
    effects.tracked_tick(area.upcast_ref(), &["vignette"], move |_, clock| {
        let start = tick_start(&started, clock.frame_time());
        let phase = ((clock.frame_time() - start) as f64 / 5_000_000.0) * TAU;
        area_for_tick.set_opacity(0.78 + phase.sin() * 0.12);
        glib::ControlFlow::Continue
    });
}

fn ensure_flicker(effects: &Effects, host: &gtk::Overlay) {
    if !effects.on("flicker") {
        return;
    }
    if let Some(flicker) = effects.core.overlays.borrow().flicker.as_ref() {
        flicker.set_visible(true);
        return;
    }
    let flicker = gtk::Box::new(gtk::Orientation::Vertical, 0);
    flicker.add_css_class("omg-flicker-overlay");
    flicker.set_hexpand(true);
    flicker.set_vexpand(true);
    flicker.set_can_target(false);
    flicker.set_opacity(0.0);
    host.add_overlay(&flicker);
    effects.core.overlays.borrow_mut().flicker = Some(flicker.clone());
    let started = Cell::new(None::<i64>);
    let flicker_for_tick = flicker.clone();
    effects.tracked_tick(flicker.upcast_ref(), &["flicker"], move |_, clock| {
        let start = tick_start(&started, clock.frame_time());
        let frame = ((clock.frame_time() - start) / 16_667) % 360;
        flicker_for_tick.set_opacity(match frame {
            0 => 0.12,
            1 => 0.06,
            _ => 0.0,
        });
        glib::ControlFlow::Continue
    });
}

fn boot_log(effects: &Effects, host: &gtk::Overlay) {
    if !effects.on("bootlog") {
        return;
    }
    let panel = gtk::Box::new(gtk::Orientation::Vertical, 0);
    panel.add_css_class("omg-bootlog");
    panel.set_halign(gtk::Align::Start);
    panel.set_valign(gtk::Align::Start);
    panel.set_margin_start(16);
    panel.set_margin_top(16);
    panel.set_can_target(false);
    let label = gtk::Label::new(None);
    label.set_halign(gtk::Align::Start);
    label.set_xalign(0.0);
    panel.append(&label);
    host.add_overlay(&panel);
    let lines = RcLines::new(vec![
        "omarchygram / active theme".to_string(),
        format!(
            "gtk {}.{}.{}",
            gtk::major_version(),
            gtk::minor_version(),
            gtk::micro_version()
        ),
        "session ok".to_string(),
        "theme monitor ok".to_string(),
        "dialogs ok".to_string(),
        "updates live".to_string(),
        "ready".to_string(),
    ]);
    let index = Cell::new(0usize);
    let label_weak = label.downgrade();
    let panel_weak = panel.downgrade();
    let host_weak = host.downgrade();
    let core = effects.core.clone();
    glib::timeout_add_local(Duration::from_millis(120), move || {
        let Some(label) = label_weak.upgrade() else {
            return glib::ControlFlow::Break;
        };
        // Switched off mid-run (e.g. the Purist preset): take the panel down now.
        if !core.on("bootlog") {
            if let (Some(panel), Some(host)) = (panel_weak.upgrade(), host_weak.upgrade()) {
                if panel.parent().is_some() {
                    host.remove_overlay(&panel);
                }
            }
            return glib::ControlFlow::Break;
        }
        let current = index.get();
        if current >= lines.len() {
            let panel_weak = panel_weak.clone();
            let host_weak = host_weak.clone();
            glib::timeout_add_local_once(Duration::from_millis(450), move || {
                if let (Some(panel), Some(host)) = (panel_weak.upgrade(), host_weak.upgrade()) {
                    if panel.parent().is_some() {
                        host.remove_overlay(&panel);
                    }
                }
            });
            return glib::ControlFlow::Break;
        }
        label.set_label(&lines.prefix(current + 1));
        index.set(current + 1);
        glib::ControlFlow::Continue
    });
}

struct RcLines(std::rc::Rc<Vec<String>>);

impl Clone for RcLines {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl RcLines {
    fn new(lines: Vec<String>) -> Self {
        Self(std::rc::Rc::new(lines))
    }

    fn len(&self) -> usize {
        self.0.len()
    }

    fn prefix(&self, len: usize) -> String {
        self.0[..len].join("\n")
    }
}

fn power_on(effects: &Effects, host: &gtk::Overlay) {
    if !effects.on("poweron") {
        return;
    }
    let beam = gtk::Box::new(gtk::Orientation::Vertical, 0);
    beam.add_css_class("omg-power-beam");
    beam.set_halign(gtk::Align::Fill);
    beam.set_valign(gtk::Align::Fill);
    beam.set_hexpand(true);
    beam.set_vexpand(true);
    beam.set_can_target(false);
    host.add_overlay(&beam);
    let started = Cell::new(None::<i64>);
    let beam_for_finish = beam.clone();
    effects.tracked_tick_with_finish(
        beam.upcast_ref(),
        &["poweron"],
        move |beam, clock| {
            let start = tick_start(&started, clock.frame_time());
            let progress = ((clock.frame_time() - start) as f64 / 800_000.0).clamp(0.0, 1.0);
            let (width, height) = beam
                .parent()
                .map(|parent| (parent.width().max(0), parent.height().max(0)))
                .unwrap_or_default();
            let horizontal = (progress / 0.4).clamp(0.0, 1.0);
            let vertical = ((progress - 0.4) / 0.4).clamp(0.0, 1.0);
            beam.set_margin_start((f64::from(width) * 0.5 * (1.0 - horizontal)).round() as i32);
            beam.set_margin_end((f64::from(width) * 0.5 * (1.0 - horizontal)).round() as i32);
            let vertical_margin =
                ((f64::from(height) - 2.0).max(0.0) * 0.5 * (1.0 - vertical)).round() as i32;
            beam.set_margin_top(vertical_margin);
            beam.set_margin_bottom(vertical_margin);
            beam.set_opacity(if progress < 0.8 {
                1.0
            } else {
                (1.0 - (progress - 0.8) / 0.2).clamp(0.0, 1.0)
            });
            if progress >= 1.0 {
                glib::ControlFlow::Break
            } else {
                glib::ControlFlow::Continue
            }
        },
        move || defer_remove_from_overlay(beam_for_finish.upcast()),
    );
}

pub(super) fn static_burst(effects: &Effects, host: &gtk::Overlay) {
    if !effects.on("staticerror") {
        return;
    }
    let area = gtk::DrawingArea::new();
    area.add_css_class("omg-overlay-accent");
    area.set_hexpand(true);
    area.set_vexpand(true);
    area.set_can_target(false);
    let frame = std::rc::Rc::new(Cell::new(0u32));
    let frame_for_draw = frame.clone();
    area.set_draw_func(move |area, context, width, height| {
        let color = area.color();
        let seed = frame_for_draw
            .get()
            .wrapping_mul(1_664_525)
            .wrapping_add(1_013_904_223);
        for index in 0..96u32 {
            let value = seed.wrapping_add(index.wrapping_mul(2_654_435_761));
            let x = (value % width.max(1) as u32) as f64;
            let y = (value.rotate_left(11) % height.max(1) as u32) as f64;
            set_source_color(
                context,
                color.red() as f64,
                color.green() as f64,
                color.blue() as f64,
                if index % 3 == 0 { 0.5 } else { 0.22 },
            );
            context.rectangle(x, y, 1.0 + f64::from(index % 5), 1.0);
            let _ = context.fill();
        }
    });
    host.add_overlay(&area);
    let area_for_tick = area.clone();
    let area_for_finish = area.clone();
    let started = Cell::new(None::<i64>);
    effects.tracked_tick_with_finish(
        area.upcast_ref(),
        &["staticerror"],
        move |_, clock| {
            let start = tick_start(&started, clock.frame_time());
            frame.set(frame.get().wrapping_add(1));
            area_for_tick.queue_draw();
            if clock.frame_time() - start >= 300_000 {
                glib::ControlFlow::Break
            } else {
                glib::ControlFlow::Continue
            }
        },
        move || defer_remove_from_overlay(area_for_finish.upcast()),
    );
}

pub(super) fn scan_sweep(effects: &Effects, host: &gtk::Overlay) {
    if !effects.on("scansweep") {
        return;
    }
    let sweep = gtk::Box::new(gtk::Orientation::Vertical, 0);
    sweep.add_css_class("omg-scan-sweep");
    sweep.set_height_request(56);
    sweep.set_halign(gtk::Align::Fill);
    sweep.set_valign(gtk::Align::Start);
    sweep.set_hexpand(true);
    sweep.set_can_target(false);
    host.add_overlay(&sweep);
    let sweep_for_tick = sweep.clone();
    let sweep_for_finish = sweep.clone();
    let started = Cell::new(None::<i64>);
    effects.tracked_tick_with_finish(
        sweep.upcast_ref(),
        &["scansweep"],
        move |_, clock| {
            let start = tick_start(&started, clock.frame_time());
            let progress = ((clock.frame_time() - start) as f64 / 600_000.0).clamp(0.0, 1.0);
            let height = sweep_for_tick
                .parent()
                .map(|parent| parent.height())
                .unwrap_or_default();
            sweep_for_tick.set_margin_top(((height + 56) as f64 * progress - 56.0).round() as i32);
            if progress >= 1.0 {
                glib::ControlFlow::Break
            } else {
                glib::ControlFlow::Continue
            }
        },
        move || defer_remove_from_overlay(sweep_for_finish.upcast()),
    );
}

pub(super) fn empty_state(effects: &Effects, host: &gtk::Overlay, visible: bool) {
    let host_changed = effects
        .core
        .overlays
        .borrow()
        .empty_host
        .as_ref()
        .and_then(glib::WeakRef::upgrade)
        .is_none_or(|current| current != *host);
    if host_changed {
        let mut state = effects.core.overlays.borrow_mut();
        let matrix = state.matrix.take();
        let grid = state.grid.take();
        state.empty_host = Some(host.downgrade());
        drop(state);
        if let Some(matrix) = matrix {
            remove_from_overlay(&matrix);
        }
        if let Some(grid) = grid {
            remove_from_overlay(&grid);
        }
    }
    if !visible {
        let mut state = effects.core.overlays.borrow_mut();
        if let Some(matrix) = state.matrix.take() {
            remove_from_overlay(&matrix);
        }
        if let Some(grid) = state.grid.take() {
            remove_from_overlay(&grid);
        }
        return;
    }
    if visible && effects.on("gridshimmer") {
        ensure_grid(effects, host);
    }
    if visible && effects.on("matrixrain") {
        ensure_matrix(effects, host);
    }
    let state = effects.core.overlays.borrow();
    if let Some(grid) = &state.grid {
        grid.set_visible(visible && effects.on("gridshimmer"));
    }
    if let Some(matrix) = &state.matrix {
        matrix.set_visible(visible && effects.on("matrixrain"));
    }
}

pub(super) fn preview_empty(effects: &Effects, host: &gtk::Overlay, id: &str) {
    if id == "gridshimmer" {
        let grid = gtk::Box::new(gtk::Orientation::Vertical, 0);
        grid.add_css_class("omg-grid-shimmer");
        grid.set_hexpand(true);
        grid.set_vexpand(true);
        grid.set_can_target(false);
        host.add_overlay(&grid);
        let grid_for_tick = grid.clone();
        let grid_for_finish = grid.clone();
        let started = Cell::new(None::<i64>);
        let previous = Cell::new(usize::MAX);
        effects.tracked_tick_with_finish(
            grid.upcast_ref(),
            &["gridshimmer"],
            move |_, clock| {
                let start = tick_start(&started, clock.frame_time());
                let step = (((clock.frame_time() - start) / 50_000) as usize) % 16;
                if previous.get() != usize::MAX {
                    grid_for_tick.remove_css_class(&format!("omg-grid-step-{}", previous.get()));
                }
                grid_for_tick.add_css_class(&format!("omg-grid-step-{step}"));
                previous.set(step);
                if clock.frame_time() - start >= 1_000_000 {
                    glib::ControlFlow::Break
                } else {
                    glib::ControlFlow::Continue
                }
            },
            move || defer_remove_from_overlay(grid_for_finish.upcast()),
        );
        return;
    }

    let area = gtk::DrawingArea::new();
    area.add_css_class("omg-overlay-accent");
    area.set_hexpand(true);
    area.set_vexpand(true);
    area.set_can_target(false);
    let frame = std::rc::Rc::new(Cell::new(0u32));
    let frame_for_draw = frame.clone();
    area.set_draw_func(move |area, context, width, height| {
        let color = area.color();
        set_source_color(
            context,
            color.red() as f64,
            color.green() as f64,
            color.blue() as f64,
            0.34,
        );
        context.set_font_size(11.0);
        for column in 0..(width.max(1) / 16).max(1) {
            let y = ((frame_for_draw.get() as i32 + column * 5) % ((height.max(1) / 14) + 1)) * 14;
            context.move_to(f64::from(column * 16), f64::from(y));
            let _ = context.show_text(if column % 2 == 0 { "0" } else { "1" });
        }
    });
    host.add_overlay(&area);
    let area_for_tick = area.clone();
    let area_for_finish = area.clone();
    let started = Cell::new(None::<i64>);
    effects.tracked_tick_with_finish(
        area.upcast_ref(),
        &["matrixrain"],
        move |_, clock| {
            let start = tick_start(&started, clock.frame_time());
            frame.set(frame.get().wrapping_add(1));
            area_for_tick.queue_draw();
            if clock.frame_time() - start >= 1_000_000 {
                glib::ControlFlow::Break
            } else {
                glib::ControlFlow::Continue
            }
        },
        move || defer_remove_from_overlay(area_for_finish.upcast()),
    );
}

fn ensure_grid(effects: &Effects, host: &gtk::Overlay) {
    if effects.core.overlays.borrow().grid.is_some() {
        return;
    }
    let grid = gtk::Box::new(gtk::Orientation::Vertical, 0);
    grid.add_css_class("omg-grid-shimmer");
    grid.set_hexpand(true);
    grid.set_vexpand(true);
    grid.set_can_target(false);
    host.add_overlay(&grid);
    effects.core.overlays.borrow_mut().grid = Some(grid.clone());
    let started = Cell::new(None::<i64>);
    let previous = Cell::new(usize::MAX);
    let grid_for_tick = grid.clone();
    effects.tracked_tick(grid.upcast_ref(), &["gridshimmer"], move |_, clock| {
        let start = tick_start(&started, clock.frame_time());
        let step = (((clock.frame_time() - start) as f64 / 14_000_000.0) * 16.0) as usize % 16;
        if step != previous.get() {
            if previous.get() != usize::MAX {
                grid_for_tick.remove_css_class(&format!("omg-grid-step-{}", previous.get()));
            }
            grid_for_tick.add_css_class(&format!("omg-grid-step-{step}"));
            previous.set(step);
        }
        glib::ControlFlow::Continue
    });
}

fn ensure_matrix(effects: &Effects, host: &gtk::Overlay) {
    if effects.core.overlays.borrow().matrix.is_some() {
        return;
    }
    let area = gtk::DrawingArea::new();
    area.add_css_class("omg-overlay-accent");
    area.set_hexpand(true);
    area.set_vexpand(true);
    area.set_can_target(false);
    let frame = std::rc::Rc::new(Cell::new(0u32));
    let frame_for_draw = frame.clone();
    area.set_draw_func(move |area, context, width, height| {
        let color = area.color();
        set_source_color(
            context,
            color.red() as f64,
            color.green() as f64,
            color.blue() as f64,
            0.34,
        );
        context.select_font_face(
            "JetBrainsMono Nerd Font",
            gtk::cairo::FontSlant::Normal,
            gtk::cairo::FontWeight::Normal,
        );
        context.set_font_size(11.0);
        let glyphs = ["0", "1", "+", "-", "<", ">", "░", "▓"];
        let columns = (width.max(1) / 16).max(1);
        for column in 0..columns {
            let row = ((frame_for_draw.get() as i32 + column * 7) % ((height.max(1) / 14) + 5)) - 4;
            for trail in 0..4 {
                let y = (row - trail) * 14;
                if y >= 0 && y < height {
                    context.move_to(f64::from(column * 16), f64::from(y));
                    let glyph =
                        glyphs[((column + row + trail).unsigned_abs() as usize) % glyphs.len()];
                    let _ = context.show_text(glyph);
                }
            }
        }
    });
    host.add_overlay(&area);
    effects.core.overlays.borrow_mut().matrix = Some(area.clone());
    let last = Cell::new(0i64);
    let area_for_tick = area.clone();
    effects.tracked_tick(area.upcast_ref(), &["matrixrain"], move |_, clock| {
        if clock.frame_time() - last.get() >= 66_000 {
            last.set(clock.frame_time());
            frame.set(frame.get().wrapping_add(1));
            area_for_tick.queue_draw();
        }
        glib::ControlFlow::Continue
    });
}

fn set_source_color(context: &gtk::cairo::Context, red: f64, green: f64, blue: f64, alpha: f64) {
    let apply = gtk::cairo::Context::set_source_rgba;
    apply(context, red, green, blue, alpha);
}

fn add_gradient_stop(
    gradient: &gtk::cairo::Gradient,
    offset: f64,
    red: f64,
    green: f64,
    blue: f64,
    alpha: f64,
) {
    let apply = gtk::cairo::Gradient::add_color_stop_rgba;
    apply(gradient, offset, red, green, blue, alpha);
}
