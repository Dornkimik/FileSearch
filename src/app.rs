use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Align, Align2, Button, CentralPanel, Color32, ComboBox, CursorIcon, FontId, Frame, Key, Label, Layout,
    Margin, Modifiers, Panel, Rect, Response, RichText, ScrollArea, Sense, Stroke, TextEdit, TextureHandle,
    TextureOptions, Theme, Ui, pos2, vec2,
};
use egui_extras::{Column, TableBuilder};

use crate::icons;
use crate::index::{self, NONE, Shared};
use crate::preview;
use crate::search::{CATEGORIES, DatePreset, Filters, Kind, Mode, Request, Searcher, SizeUnit, Sort, SortKey};
use crate::theme::{self, Palette};
use crate::util;

const ROW_HEIGHT: f32 = 30.0;

enum Action {
    Open(String),
    Reveal(String),
    Copy(String),
    Exclude(String),
}

pub struct App {
    shared: Arc<Shared>,
    searcher: Searcher,
    drives: Vec<PathBuf>,

    filters: Filters,
    sort: Sort,
    /// What the current results were requested with.
    sent: Option<(Filters, Sort, u64)>,
    next_id: u64,
    pending: bool,

    results: Vec<u32>,
    results_version: u64,
    elapsed: Duration,
    error: Option<String>,

    selected: Option<usize>,
    scroll_to: Option<usize>,
    show_filters: bool,
    focus_search: bool,
    search_focused: bool,
    controls_width: f32,
    last_len: usize,
    last_refresh: Instant,

    show_preview: bool,
    previewer: preview::Loader,
    preview: PreviewState,
}

/// The selected result, copied out of the index for the details pane.
struct Selection {
    path: String,
    name: String,
    folder: String,
    size: u64,
    modified: u32,
    is_dir: bool,
    ext: String,
    kind: Option<preview::Kind>,
}

#[derive(Default)]
struct PreviewState {
    /// Path the preview belongs to ("" when nothing is previewed).
    path: String,
    id: u64,
    status: PreviewStatus,
    started: Option<Instant>,
}

#[derive(Default)]
enum PreviewStatus {
    #[default]
    Idle,
    Loading,
    Ready { frames: Vec<(TextureHandle, Duration)>, total: Duration, dims: Option<[u32; 2]> },
    Failed,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        theme::install(&cc.egui_ctx);
        let shared = Arc::new(Shared::new());
        let ctx = cc.egui_ctx.clone();
        let searcher = Searcher::new(shared.clone(), move || ctx.request_repaint());
        let drives = util::fixed_drives();
        let ctx = cc.egui_ctx.clone();
        index::start(shared.clone(), drives.clone(), true, move || ctx.request_repaint());
        let ctx = cc.egui_ctx.clone();
        let previewer = preview::Loader::new(move || ctx.request_repaint());
        Self {
            show_preview: true,
            previewer,
            preview: PreviewState::default(),
            shared,
            searcher,
            drives,
            filters: Filters { excluded: load_excluded(), ..Filters::default() },
            sort: Sort { key: SortKey::Name, ascending: true },
            sent: None,
            next_id: 0,
            pending: false,
            results: Vec::new(),
            results_version: u64::MAX,
            elapsed: Duration::ZERO,
            error: None,
            selected: None,
            scroll_to: None,
            show_filters: true,
            focus_search: true,
            search_focused: false,
            controls_width: 420.0,
            last_len: 0,
            last_refresh: Instant::now(),
        }
    }

    fn rescan(&self, ctx: &egui::Context) {
        let ctx = ctx.clone();
        index::start(self.shared.clone(), self.drives.clone(), false, move || ctx.request_repaint());
    }

    /// Picks up finished searches and submits a new one when anything changed.
    fn sync_search(&mut self, index_len: usize) {
        if let Some(out) = self.searcher.outcome.lock().take() {
            if out.id == self.next_id {
                self.pending = false;
                self.elapsed = out.elapsed;
                self.error = out.error;
                if self.error.is_none() {
                    self.results = out.hits;
                    self.results_version = out.index_version;
                }
                if self.selected.is_some_and(|s| s >= self.results.len()) {
                    self.selected = None;
                }
            }
        }

        let version = self.shared.version.load(Ordering::SeqCst);
        let indexing = self.shared.indexing.load(Ordering::Relaxed);
        let changed = match &self.sent {
            Some((f, s, v)) => *f != self.filters || *s != self.sort || *v != version,
            None => true,
        };
        // While the first scan streams in, refresh results periodically.
        let grew = indexing
            && index_len != self.last_len
            && self.last_refresh.elapsed() > Duration::from_millis(700);
        let finished = !indexing && index_len != self.last_len;

        if changed || grew || finished {
            if self.sent.as_ref().is_some_and(|(f, s, _)| *f != self.filters || *s != self.sort) {
                self.selected = None;
            }
            self.next_id += 1;
            self.pending = true;
            self.last_len = index_len;
            self.last_refresh = Instant::now();
            self.sent = Some((self.filters.clone(), self.sort, version));
            self.searcher.submit(Request { id: self.next_id, filters: self.filters.clone(), sort: self.sort });
        }
    }

    fn handle_keys(&mut self, ctx: &egui::Context) -> Option<Action> {
        let n = self.results.len();
        let (down, up, pgdn, pgup, enter, shift_enter, esc, find) = ctx.input_mut(|i| {
            (
                i.consume_key(Modifiers::NONE, Key::ArrowDown),
                i.consume_key(Modifiers::NONE, Key::ArrowUp),
                i.consume_key(Modifiers::NONE, Key::PageDown),
                i.consume_key(Modifiers::NONE, Key::PageUp),
                i.consume_key(Modifiers::NONE, Key::Enter),
                i.consume_key(Modifiers::SHIFT, Key::Enter),
                i.consume_key(Modifiers::NONE, Key::Escape),
                i.consume_key(Modifiers::COMMAND, Key::F),
            )
        });
        if find {
            self.focus_search = true;
        }
        if esc && !self.filters.text.is_empty() {
            self.filters.text.clear();
            self.focus_search = true;
        }
        if n > 0 {
            let step: isize = if down { 1 } else if up { -1 } else if pgdn { 15 } else if pgup { -15 } else { 0 };
            if step != 0 {
                let cur = self.selected.map_or(-1, |s| s as isize);
                let next = (cur + step).clamp(0, n as isize - 1) as usize;
                self.selected = Some(next);
                self.scroll_to = Some(next);
            }
        }
        if (enter || shift_enter) && n > 0 && self.results_version == self.shared.version.load(Ordering::SeqCst) {
            let row = self.selected.unwrap_or(0);
            let idx = self.shared.index.read();
            let path = idx.entries.get(self.results[row] as usize).map(|e| idx.full_path(e))?;
            return Some(if shift_enter { Action::Reveal(path) } else { Action::Open(path) });
        }
        None
    }

    // -----------------------------------------------------------------------
    // Top bar

    fn search_bar(&mut self, ui: &mut Ui, p: &Palette) {
        ui.horizontal(|ui| {
            // The field takes whatever the controls (measured last frame) leave over.
            let gap = 10.0;
            let field_w = (ui.available_width() - self.controls_width - gap).max(220.0);
            let border = if self.search_focused { p.accent } else { p.border };
            Frame::new()
                .fill(p.field)
                .stroke(Stroke::new(1.0, border))
                .corner_radius(10)
                .inner_margin(Margin::symmetric(12, 7))
                .show(ui, |ui| {
                    ui.set_width(field_w - 26.0);
                    ui.horizontal(|ui| {
                        let (r, _) = ui.allocate_exact_size(vec2(20.0, 22.0), Sense::hover());
                        icons::magnifier(ui.painter(), r, p.muted);
                        let hint = match self.filters.mode {
                            Mode::Contains => "Search files and folders   (use -word to exclude)",
                            Mode::Wildcard => "Wildcard pattern, e.g.  *.pdf   or   report_??.xlsx",
                            Mode::Regex => r"Regular expression, e.g.  ^IMG_\d+\.jpe?g$",
                        };
                        let clear_w = if self.filters.text.is_empty() { 0.0 } else { 26.0 };
                        let edit = TextEdit::singleline(&mut self.filters.text)
                            .hint_text(RichText::new(hint).color(p.muted))
                            .frame(Frame::NONE)
                            .font(FontId::proportional(17.0))
                            .desired_width(ui.available_width() - clear_w);
                        let resp = ui.add(edit);
                        if std::mem::take(&mut self.focus_search) {
                            resp.request_focus();
                        }
                        self.search_focused = resp.has_focus();
                        if clear_w > 0.0 {
                            let (r, resp) = ui.allocate_exact_size(vec2(22.0, 22.0), Sense::click());
                            let color = if resp.hovered() { p.text } else { p.muted };
                            icons::close(ui.painter(), r, color);
                            if resp.on_hover_cursor(CursorIcon::PointingHand).on_hover_text("Clear  (Esc)").clicked() {
                                self.filters.text.clear();
                                self.focus_search = true;
                            }
                        }
                    });
                });
            ui.add_space(gap - ui.spacing().item_spacing.x);

            let start = ui.cursor().min.x;
            segmented(
                ui,
                p,
                &mut self.filters.mode,
                &[(Mode::Contains, "Contains"), (Mode::Wildcard, "Wildcard"), (Mode::Regex, "Regex")],
            );
            if text_button(ui, p, "Aa", self.filters.case_sensitive).on_hover_text("Match case").clicked() {
                self.filters.case_sensitive = !self.filters.case_sensitive;
            }
            if text_button(ui, p, "Path", self.filters.match_path)
                .on_hover_text("Match against the full path, not just the name")
                .clicked()
            {
                self.filters.match_path = !self.filters.match_path;
            }
            ui.add_space(4.0);
            let active = self.filters.active_count();
            let label = if active > 0 { format!("Filters · {active}") } else { "Filters".to_string() };
            if chip(ui, p, self.show_filters, &label).clicked() {
                self.show_filters = !self.show_filters;
            }
            if chip(ui, p, self.show_preview, "Preview").clicked() {
                self.show_preview = !self.show_preview;
            }
            let dark = ui.visuals().dark_mode;
            let theme_btn = paint_button(ui, p, |painter, r| {
                if dark { icons::sun(painter, r, p.text) } else { icons::moon(painter, r, p.text, p.field) }
            });
            if theme_btn.on_hover_text(if dark { "Light theme" } else { "Dark theme" }).clicked() {
                ui.ctx().set_theme(if dark { Theme::Light } else { Theme::Dark });
            }

            let width = ui.min_rect().max.x - start;
            if (width - self.controls_width).abs() > 0.5 {
                self.controls_width = width;
                ui.ctx().request_repaint();
            }
        });
    }

    // -----------------------------------------------------------------------
    // Filters sidebar

    fn filters_panel(&mut self, ui: &mut Ui, p: &Palette, index_len: usize) -> bool {
        let mut rescan = false;
        ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            let f = &mut self.filters;
            let full = ui.available_width();

            section(ui, p, "TYPE");
            segmented(ui, p, &mut f.kind, &[(Kind::All, "All"), (Kind::Files, "Files"), (Kind::Folders, "Folders")]);

            section(ui, p, "CATEGORY");
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
                for (i, cat) in CATEGORIES.iter().enumerate() {
                    if chip(ui, p, f.categories[i], cat.name).clicked() {
                        f.categories[i] = !f.categories[i];
                    }
                }
            });

            section(ui, p, "EXTENSIONS");
            ui.add(field(&mut f.extensions, "e.g.  pdf, docx, psd", full));

            section(ui, p, "SIZE");
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                let w = ((full - 90.0) / 2.0).max(40.0);
                ui.add(field(&mut f.min_size, "Min", w));
                ui.label(RichText::new("–").color(p.muted));
                ui.add(field(&mut f.max_size, "Max", w));
                ComboBox::from_id_salt("size_unit")
                    .width(54.0)
                    .selected_text(f.size_unit.label())
                    .show_ui(ui, |ui| {
                        for u in SizeUnit::ALL {
                            ui.selectable_value(&mut f.size_unit, u, u.label());
                        }
                    });
            });

            section(ui, p, "MODIFIED");
            ComboBox::from_id_salt("modified")
                .width(full - 8.0)
                .selected_text(f.modified.label())
                .show_ui(ui, |ui| {
                    for d in DatePreset::ALL {
                        ui.selectable_value(&mut f.modified, d, d.label());
                    }
                });

            section(ui, p, "LOCATION");
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                ui.add(field(&mut f.location, "Anywhere", full - 42.0));
                let browse = paint_button(ui, p, |painter, r| icons::folder(painter, r, p.muted));
                if browse.on_hover_text("Choose a folder").clicked() {
                    if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                        f.location = dir.display().to_string();
                    }
                }
            });

            section(ui, p, "OPTIONS");
            ui.checkbox(&mut f.include_system, "Include system files").on_hover_text(
                "Windows, Program Files, ProgramData, AppData and items marked as system files",
            );
            ui.checkbox(&mut f.include_hidden, "Include hidden items");

            if !f.excluded.is_empty() {
                section(ui, p, "EXCLUDED FOLDERS");
                let mut remove = None;
                for (i, path) in f.excluded.iter().enumerate() {
                    ui.horizontal(|ui| {
                        let (r, resp) = ui.allocate_exact_size(vec2(22.0, 22.0), Sense::click());
                        icons::close(ui.painter(), r, if resp.hovered() { p.error } else { p.muted });
                        if resp.on_hover_cursor(CursorIcon::PointingHand).on_hover_text("Stop excluding").clicked() {
                            remove = Some(i);
                        }
                        ui.add(Label::new(RichText::new(path).color(p.text)).truncate());
                    });
                }
                if let Some(i) = remove {
                    f.excluded.remove(i);
                    save_excluded(&f.excluded);
                }
            }

            if f.active_count() > 0 || f.include_hidden || f.include_system {
                ui.add_space(14.0);
                if ui.add(Button::new("Reset filters").min_size(vec2(full, 30.0))).clicked() {
                    // Exclusions are a lasting preference, so a reset keeps them.
                    let keep = (f.text.clone(), f.mode, f.case_sensitive, f.match_path, std::mem::take(&mut f.excluded));
                    *f = Filters::default();
                    (f.text, f.mode, f.case_sensitive, f.match_path, f.excluded) = keep;
                }
            }

            section(ui, p, "INDEX");
            let drives: Vec<String> = self.drives.iter().map(|d| d.display().to_string()).collect();
            ui.label(RichText::new(format!("{} items on {}", util::format_count(index_len), drives.join("  "))).color(p.muted));
            let indexing = self.shared.indexing.load(Ordering::Relaxed);
            ui.add_space(4.0);
            let btn = Button::new(if indexing { "Scanning…" } else { "Rescan drives" }).min_size(vec2(full, 30.0));
            if ui.add_enabled(!indexing, btn).clicked() {
                rescan = true;
            }
            ui.add_space(12.0);
        });
        rescan
    }

    // -----------------------------------------------------------------------
    // Results

    fn results_view(&mut self, ui: &mut Ui, p: &Palette) -> Option<Action> {
        let idx = self.shared.index.read();
        let valid = self.results_version == self.shared.version.load(Ordering::SeqCst);
        let n = if valid { self.results.len() } else { 0 };
        let indexing = self.shared.indexing.load(Ordering::Relaxed);

        if n == 0 {
            ui.vertical_centered(|ui| {
                ui.add_space((ui.available_height() * 0.32).max(20.0));
                if indexing && idx.entries.is_empty() {
                    ui.add(egui::Spinner::new().size(28.0).color(p.accent));
                    ui.add_space(12.0);
                    ui.label(RichText::new("Building the index").font(theme::semibold(18.0)).color(p.text));
                    ui.label(
                        RichText::new("Results will appear as soon as files are found.").color(p.muted),
                    );
                } else if self.pending || !valid {
                    ui.add(egui::Spinner::new().size(22.0).color(p.accent));
                } else {
                    let (r, _) = ui.allocate_exact_size(vec2(44.0, 44.0), Sense::hover());
                    icons::magnifier(ui.painter(), r, p.muted.gamma_multiply(0.6));
                    ui.add_space(8.0);
                    ui.label(RichText::new("No matches").font(theme::semibold(18.0)).color(p.text));
                    ui.label(RichText::new("Try a different search or loosen the filters.").color(p.muted));
                }
            });
            return None;
        }

        let mut action = None;
        let mut sort = self.sort;
        let mut selected = self.selected;
        let results = &self.results;

        let mut table = TableBuilder::new(ui)
            .id_salt("results")
            .striped(false)
            .resizable(false)
            .sense(Sense::click())
            .cell_layout(Layout::left_to_right(Align::Center))
            // Name and Folder share whatever width is left, so the table always fits beside the preview.
            .column(Column::remainder().at_least(120.0).clip(true))
            .column(Column::remainder().at_least(90.0).clip(true))
            .column(Column::exact(76.0).clip(true))
            .column(Column::exact(138.0).clip(true))
            .min_scrolled_height(0.0)
            .auto_shrink([false, false]);
        if let Some(r) = self.scroll_to.take() {
            table = table.scroll_to_row(r, None);
        }

        table
            .header(34.0, |mut header| {
                for (key, label, right) in [
                    (SortKey::Name, "Name", false),
                    (SortKey::Folder, "Folder", false),
                    (SortKey::Size, "Size", true),
                    (SortKey::Modified, "Modified", false),
                ] {
                    header.col(|ui| sort_header(ui, p, &mut sort, key, label, right));
                }
            })
            .body(|body| {
                body.rows(ROW_HEIGHT, n, |mut row| {
                    let i = row.index();
                    let Some(e) = idx.entries.get(results[i] as usize) else { return };
                    row.set_selected(selected == Some(i));
                    let name = idx.name(e);
                    row.col(|ui| {
                        let (r, _) = ui.allocate_exact_size(vec2(22.0, ROW_HEIGHT), Sense::hover());
                        if e.is_dir() {
                            icons::folder(ui.painter(), r, util::FOLDER_COLOR);
                        } else {
                            icons::file(ui.painter(), r, util::file_color(idx.name_lower(e)));
                        }
                        ui.add(Label::new(RichText::new(name).color(p.text)).truncate());
                    });
                    row.col(|ui| {
                        ui.add(Label::new(RichText::new(idx.dir_path(e.parent)).color(p.muted)).truncate());
                    });
                    row.col(|ui| {
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if !e.is_dir() {
                                ui.label(RichText::new(util::format_size(e.size)).color(p.muted));
                            }
                        });
                    });
                    row.col(|ui| {
                        ui.label(RichText::new(util::format_time(e.modified)).color(p.muted));
                    });

                    let resp = row.response();
                    if resp.clicked() || resp.secondary_clicked() {
                        selected = Some(i);
                    }
                    if resp.double_clicked() {
                        action = Some(Action::Open(idx.full_path(e)));
                    }
                    resp.context_menu(|ui| {
                        ui.set_min_width(190.0);
                        let path = idx.full_path(e);
                        if ui.button("Open").clicked() {
                            action = Some(Action::Open(path.clone()));
                        }
                        if ui.button("Show in Explorer").clicked() {
                            action = Some(Action::Reveal(path.clone()));
                        }
                        ui.separator();
                        if ui.button("Copy path").clicked() {
                            action = Some(Action::Copy(path));
                        }
                        if ui.button("Copy name").clicked() {
                            action = Some(Action::Copy(name.to_string()));
                        }
                        ui.separator();
                        ui.menu_button("Exclude folder", |ui| {
                            ui.set_min_width(200.0);
                            // The folder itself (for folders), then each ancestor up to the drive.
                            if e.is_dir() && ui.button(short(name)).on_hover_text(idx.full_path(e)).clicked() {
                                action = Some(Action::Exclude(idx.full_path(e)));
                            }
                            let mut d = e.parent;
                            while d != NONE && idx.dirs[d as usize].parent != NONE {
                                let path = idx.dir_path(d);
                                if ui.button(short(idx.dir_name(d, false))).on_hover_text(&path).clicked() {
                                    action = Some(Action::Exclude(path));
                                }
                                d = idx.dirs[d as usize].parent;
                            }
                        });
                    });
                });
            });

        self.sort = sort;
        self.selected = selected;
        action
    }

    fn selection(&self) -> Option<Selection> {
        if self.results_version != self.shared.version.load(Ordering::SeqCst) {
            return None;
        }
        let id = *self.results.get(self.selected?)?;
        let idx = self.shared.index.read();
        let e = idx.entries.get(id as usize)?;
        let lower = idx.name_lower(e);
        Some(Selection {
            path: idx.full_path(e),
            name: idx.name(e).to_string(),
            folder: idx.dir_path(e.parent),
            size: e.size,
            modified: e.modified,
            is_dir: e.is_dir(),
            ext: crate::search::extension(lower).to_string(),
            kind: if e.is_dir() { None } else { preview::kind(lower) },
        })
    }

    /// Starts loading a preview when the selection changes and uploads finished ones as textures.
    fn sync_preview(&mut self, ctx: &egui::Context, sel: Option<&Selection>) {
        let want = sel.filter(|s| self.show_preview && s.kind.is_some());
        let key = want.map_or("", |s| s.path.as_str());
        if key != self.preview.path {
            self.preview.path = key.to_string();
            self.preview.id += 1;
            self.preview.status = match want {
                Some(s) => {
                    self.previewer.request(self.preview.id, s.path.clone(), s.kind.unwrap());
                    PreviewStatus::Loading
                }
                None => PreviewStatus::Idle,
            };
        }

        let finished = self.previewer.result.lock().take();
        if let Some((id, result)) = finished {
            if id == self.preview.id {
                self.preview.status = match result {
                    Ok(decoded) => {
                        let frames: Vec<(TextureHandle, Duration)> = decoded
                            .frames
                            .into_iter()
                            .enumerate()
                            .map(|(i, f)| (ctx.load_texture(format!("preview-{i}"), f.image, TextureOptions::LINEAR), f.delay))
                            .collect();
                        let total = frames.iter().map(|f| f.1).sum();
                        PreviewStatus::Ready { frames, total, dims: decoded.dims }
                    }
                    Err(_) => PreviewStatus::Failed,
                };
                self.preview.started = Some(Instant::now());
            }
        }
    }

    fn preview_panel(&mut self, ui: &mut Ui, p: &Palette, sel: Option<&Selection>) -> Option<Action> {
        let mut action = None;
        Frame::new()
            .fill(p.surface)
            .stroke(Stroke::new(1.0, p.border))
            .corner_radius(10)
            .inner_margin(Margin::same(14))
            .show(ui, |ui| {
                ui.set_min_size(ui.available_size());
                let Some(s) = sel else {
                    ui.vertical_centered(|ui| {
                        ui.add_space((ui.available_height() * 0.4).max(20.0));
                        ui.label(RichText::new("Select a file to preview it").color(p.muted));
                    });
                    return;
                };
                ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    // Media area
                    let w = ui.available_width();
                    let h = (w * 0.8).clamp(160.0, 520.0);
                    let (rect, _) = ui.allocate_exact_size(vec2(w, h), Sense::hover());
                    ui.painter().rect_filled(rect, 8, p.field);
                    let inner = rect.shrink(10.0);
                    match &self.preview.status {
                        PreviewStatus::Ready { frames, total, .. } if !frames.is_empty() => {
                            let i = self.current_frame(ui.ctx(), frames, *total);
                            let tex = &frames[i].0;
                            let size = tex.size_vec2();
                            let scale = (inner.width() / size.x).min(inner.height() / size.y).min(2.0);
                            let img = Rect::from_center_size(rect.center(), size * scale);
                            let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
                            ui.painter().image(tex.id(), img, uv, Color32::WHITE);
                            if s.kind == Some(preview::Kind::Video) {
                                icons::play_badge(ui.painter(), rect.center());
                            }
                        }
                        PreviewStatus::Loading => {
                            ui.put(Rect::from_center_size(rect.center(), vec2(24.0, 24.0)), egui::Spinner::new().size(24.0).color(p.accent));
                        }
                        _ => {
                            let label = if s.is_dir {
                                "Folder".to_string()
                            } else if s.kind.is_some() {
                                "No preview available".to_string()
                            } else if s.ext.is_empty() {
                                "File".to_string()
                            } else {
                                s.ext.to_uppercase()
                            };
                            let big = s.kind.is_none() && !s.is_dir;
                            let font = if big { theme::semibold(28.0) } else { FontId::proportional(14.0) };
                            ui.painter().text(rect.center(), Align2::CENTER_CENTER, label, font, p.muted);
                        }
                    }

                    // Details
                    ui.add_space(14.0);
                    ui.add(Label::new(RichText::new(&s.name).font(theme::semibold(15.5)).color(p.text)).wrap());
                    ui.add_space(8.0);
                    let row = |ui: &mut Ui, k: &str, v: String| {
                        ui.label(RichText::new(k).color(p.muted));
                        ui.label(RichText::new(v).color(p.text));
                        ui.end_row();
                    };
                    egui::Grid::new("details").num_columns(2).spacing([14.0, 6.0]).show(ui, |ui| {
                        row(ui, "Type", type_label(s));
                        if !s.is_dir {
                            row(ui, "Size", util::format_size(s.size));
                        }
                        if let PreviewStatus::Ready { frames, dims, .. } = &self.preview.status {
                            if let Some([w, h]) = dims {
                                row(ui, "Dimensions", format!("{w} × {h}"));
                            }
                            if frames.len() > 1 {
                                row(ui, "Frames", frames.len().to_string());
                            }
                        }
                        row(ui, "Modified", util::format_time(s.modified));
                    });
                    ui.add_space(8.0);
                    ui.label(RichText::new("Folder").color(p.muted));
                    ui.add(Label::new(RichText::new(&s.folder).color(p.text)).wrap());

                    ui.add_space(14.0);
                    ui.horizontal(|ui| {
                        let half = (ui.available_width() - ui.spacing().item_spacing.x) / 2.0;
                        if ui.add(Button::new("Open").min_size(vec2(half, 32.0))).clicked() {
                            action = Some(Action::Open(s.path.clone()));
                        }
                        if ui.add(Button::new("Show in Explorer").min_size(vec2(half, 32.0))).clicked() {
                            action = Some(Action::Reveal(s.path.clone()));
                        }
                    });
                });
            });
        action
    }

    /// Picks the GIF frame for the current time and schedules the next repaint.
    fn current_frame(&self, ctx: &egui::Context, frames: &[(TextureHandle, Duration)], total: Duration) -> usize {
        if frames.len() < 2 || total.is_zero() {
            return 0;
        }
        let elapsed = self.preview.started.map_or(Duration::ZERO, |t| t.elapsed());
        let mut t = Duration::from_nanos((elapsed.as_nanos() % total.as_nanos()) as u64);
        for (i, (_, delay)) in frames.iter().enumerate() {
            if t < *delay {
                ctx.request_repaint_after(*delay - t);
                return i;
            }
            t -= *delay;
        }
        0
    }

    fn status_bar(&self, ui: &mut Ui, p: &Palette, index_len: usize) {
        ui.horizontal(|ui| {
            let small = |s: String, c: Color32| RichText::new(s).size(12.5).color(c);
            if let Some(err) = &self.error {
                ui.label(small(format!("⚠  {err}"), p.error));
            } else if self.results_version == self.shared.version.load(Ordering::SeqCst) {
                let n = self.results.len();
                ui.label(small(format!("{} {}", util::format_count(n), if n == 1 { "result" } else { "results" }), p.text));
                ui.label(small(format!("·  {} ms", self.elapsed.as_millis()), p.muted));
            }

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if self.shared.indexing.load(Ordering::Relaxed) {
                    let scanned = self.shared.scanned.load(Ordering::Relaxed) as usize;
                    let status = self.shared.status.lock().clone();
                    ui.label(small(format!("{status}  ·  {} items", util::format_count(scanned)), p.muted));
                    ui.add(egui::Spinner::new().size(12.0).color(p.accent));
                } else {
                    let when = match *self.shared.last_scan.lock() {
                        Some((at, took)) => {
                            format!("updated {} (took {:.1}s)", time_ago(at.elapsed()), took.as_secs_f32())
                        }
                        None => "loaded from cache".into(),
                    };
                    ui.label(small(format!("{} items indexed  ·  {when}", util::format_count(index_len)), p.muted));
                }
            });
        });
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let index_len = self.shared.index.read().entries.len();
        self.sync_search(index_len);
        let mut action = self.handle_keys(&ctx);
        let p = theme::palette(ctx.global_style().visuals.dark_mode);

        Panel::top("search")
            .show_separator_line(false)
            .frame(Frame::new().fill(p.bg).inner_margin(Margin { left: 16, right: 16, top: 14, bottom: 12 }))
            .show(ui, |ui| self.search_bar(ui, &p));

        Panel::bottom("status")
            .show_separator_line(false)
            .frame(
                Frame::new()
                    .fill(p.surface)
                    .stroke(Stroke::new(1.0, p.border))
                    .inner_margin(Margin::symmetric(16, 6)),
            )
            .show(ui, |ui| self.status_bar(ui, &p, index_len));

        let mut rescan = false;
        let mut open = self.show_filters;
        Panel::left("filters")
            .resizable(false)
            .exact_size(262.0)
            .show_separator_line(false)
            .drag_to_open(false)
            .frame(Frame::new().fill(p.bg).inner_margin(Margin { left: 16, right: 10, top: 0, bottom: 0 }))
            .show_collapsible(ui, &mut open, |ui| rescan = self.filters_panel(ui, &p, index_len));
        self.show_filters = open;
        if rescan {
            self.rescan(&ctx);
        }

        let selection = self.selection();
        self.sync_preview(&ctx, selection.as_ref());
        let mut open = self.show_preview;
        Panel::right("preview")
            .resizable(true)
            .default_size(340.0)
            .size_range(260.0..=720.0)
            .show_separator_line(false)
            .drag_to_open(false)
            .frame(Frame::new().fill(p.bg).inner_margin(Margin { left: 0, right: 16, top: 0, bottom: 12 }))
            .show_collapsible(ui, &mut open, |ui| {
                if let Some(a) = self.preview_panel(ui, &p, selection.as_ref()) {
                    action = Some(a);
                }
            });
        self.show_preview = open;

        CentralPanel::no_frame()
            .frame(Frame::new().fill(p.bg).inner_margin(Margin { left: 8, right: 12, top: 0, bottom: 0 }))
            .show(ui, |ui| {
                Frame::new()
                    .fill(p.surface)
                    .stroke(Stroke::new(1.0, p.border))
                    .corner_radius(10)
                    .inner_margin(Margin { left: 10, right: 10, top: 2, bottom: 4 })
                    .show(ui, |ui| {
                        ui.set_min_size(ui.available_size());
                        if let Some(a) = self.results_view(ui, &p) {
                            action = Some(a);
                        }
                    });
                ui.add_space(12.0);
            });

        match action {
            Some(Action::Open(path)) => util::open(&path),
            Some(Action::Reveal(path)) => util::reveal(&path),
            Some(Action::Copy(text)) => ctx.copy_text(text),
            Some(Action::Exclude(path)) => {
                if !self.filters.excluded.iter().any(|p| p.eq_ignore_ascii_case(&path)) {
                    self.filters.excluded.push(path);
                    save_excluded(&self.filters.excluded);
                }
            }
            None => {}
        }

        if self.shared.indexing.load(Ordering::Relaxed) || self.pending {
            ctx.request_repaint_after(Duration::from_millis(250));
        }
    }
}

// ---------------------------------------------------------------------------
// Widgets

fn section(ui: &mut Ui, p: &Palette, title: &str) {
    ui.add_space(16.0);
    ui.label(RichText::new(title).font(theme::semibold(11.0)).color(p.muted).extra_letter_spacing(0.8));
    ui.add_space(2.0);
}

fn field<'a>(text: &'a mut String, hint: &str, width: f32) -> TextEdit<'a> {
    TextEdit::singleline(text).hint_text(hint).desired_width(width).margin(Margin::symmetric(8, 5))
}

fn chip(ui: &mut Ui, p: &Palette, on: bool, label: &str) -> Response {
    let dark = ui.visuals().dark_mode;
    let text_color = if on { if dark { Color32::from_rgb(0xD5, 0xDB, 0xFF) } else { p.accent } } else { p.text };
    let btn = Button::new(RichText::new(label).size(13.0).color(text_color))
        .fill(if on { p.accent_soft } else { p.field })
        .stroke(Stroke::new(1.0, if on { p.accent } else { p.border }))
        .corner_radius(14)
        .min_size(vec2(0.0, 28.0));
    ui.add(btn).on_hover_cursor(CursorIcon::PointingHand)
}

fn text_button(ui: &mut Ui, p: &Palette, label: &str, on: bool) -> Response {
    let btn = Button::new(RichText::new(label).size(13.5).color(if on { p.accent } else { p.text }))
        .fill(if on { p.accent_soft } else { p.field })
        .stroke(Stroke::new(1.0, if on { p.accent } else { p.border }))
        .corner_radius(8)
        .min_size(vec2(34.0, 32.0));
    ui.add(btn).on_hover_cursor(CursorIcon::PointingHand)
}

/// A square button whose icon is painted by `paint` into the button's rect.
fn paint_button(ui: &mut Ui, p: &Palette, paint: impl FnOnce(&egui::Painter, egui::Rect)) -> Response {
    let btn = Button::new("")
        .fill(p.field)
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(8)
        .min_size(vec2(34.0, 32.0));
    let resp = ui.add(btn).on_hover_cursor(CursorIcon::PointingHand);
    paint(ui.painter(), resp.rect);
    resp
}

fn segmented<T: PartialEq + Copy>(ui: &mut Ui, p: &Palette, value: &mut T, options: &[(T, &str)]) {
    Frame::new()
        .fill(p.surface)
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(9)
        .inner_margin(Margin::same(3))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                for (v, label) in options {
                    let on = *value == *v;
                    let btn = Button::new(RichText::new(*label).size(13.0).color(if on { p.text } else { p.muted }))
                        .fill(if on { p.field } else { Color32::TRANSPARENT })
                        .stroke(if on { Stroke::new(1.0, p.border) } else { Stroke::NONE })
                        .corner_radius(6)
                        .min_size(vec2(0.0, 24.0));
                    if ui.add(btn).on_hover_cursor(CursorIcon::PointingHand).clicked() {
                        *value = *v;
                    }
                }
            });
        });
}

fn sort_header(ui: &mut Ui, p: &Palette, sort: &mut Sort, key: SortKey, label: &str, right: bool) {
    let active = sort.key == key;
    let arrow = match (active, sort.ascending) {
        (false, _) => "",
        (true, true) => "  ↑",
        (true, false) => "  ↓",
    };
    let text = RichText::new(format!("{label}{arrow}")).font(theme::semibold(12.5)).color(if active { p.text } else { p.muted });
    let layout = if right { Layout::right_to_left(Align::Center) } else { Layout::left_to_right(Align::Center) };
    let rect = ui.max_rect();
    ui.painter().hline(rect.x_range().expand(4.0), rect.bottom(), Stroke::new(1.0, p.border));
    let clicked = ui
        .with_layout(layout, |ui| ui.add(Label::new(text).sense(Sense::click())).on_hover_cursor(CursorIcon::PointingHand))
        .inner
        .clicked();
    if clicked {
        if active {
            sort.ascending = !sort.ascending;
        } else {
            // Names read best A→Z; sizes and dates are most useful largest/newest first.
            *sort = Sort { key, ascending: matches!(key, SortKey::Name | SortKey::Folder) };
        }
    }
}

fn type_label(s: &Selection) -> String {
    if s.is_dir {
        return "Folder".into();
    }
    if s.ext.is_empty() {
        return "File".into();
    }
    let ext = s.ext.to_uppercase();
    match s.kind {
        Some(preview::Kind::Image) => format!("{ext} image"),
        Some(preview::Kind::Gif) => "GIF animation".into(),
        Some(preview::Kind::Video) => format!("{ext} video"),
        None => format!("{ext} file"),
    }
}

/// Shortens a folder name for a menu label.
fn short(name: &str) -> String {
    if name.chars().count() <= 32 {
        return name.to_string();
    }
    let head: String = name.chars().take(31).collect();
    format!("{head}…")
}

fn excluded_path() -> Option<PathBuf> {
    index::cache_path().map(|p| p.with_file_name("excluded.txt"))
}

fn load_excluded() -> Vec<String> {
    let Some(text) = excluded_path().and_then(|p| std::fs::read_to_string(p).ok()) else {
        return Vec::new();
    };
    text.lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect()
}

fn save_excluded(paths: &[String]) {
    if let Some(file) = excluded_path() {
        if let Some(dir) = file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(file, paths.join("\n"));
    }
}

fn time_ago(d: Duration) -> String {
    let s = d.as_secs();
    match s {
        0..=59 => "just now".into(),
        60..=3599 => format!("{} min ago", s / 60),
        3600..=86_399 => format!("{} h ago", s / 3600),
        _ => format!("{} d ago", s / 86_400),
    }
}
