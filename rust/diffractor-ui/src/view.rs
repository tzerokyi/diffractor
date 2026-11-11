//! Native egui presentation for the Linux preview.
//!
//! `Browser` owns state and worker coordination in `main.rs`; this module
//! owns the app's visible interaction and delegates state changes to Browser.
//! Kept behavior-preserving as the initial boundary for later view decomposition.

use super::*;

impl eframe::App for Browser {
    fn ui(&mut self, root: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = root.ctx().clone();
        self.receive(&ctx);
        self.tick_slideshow(&ctx);
        let dropped = ctx.input(|input| input.raw.dropped_files.clone());
        if !dropped.is_empty() {
            if dropped.len() == 1 {
                self.open_path(dropped[0].path().to_path_buf());
            } else {
                self.status = Status::Text(Text::DropOne);
            }
        }
        // Escape leaves magnification first, then fullscreen; it never
        // unexpectedly closes the native window or drops file selection.
        if !ctx.egui_wants_keyboard_input() {
            if ctx.input(|input| input.key_pressed(egui::Key::F5)) {
                self.refresh();
            } else if ctx.input(|input| input.key_pressed(egui::Key::F11)) {
                self.toggle_fullscreen(&ctx);
            } else if ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
                if self.slideshow {
                    self.stop_slideshow();
                } else if self.selected.is_some() && self.source_extent.is_some()
                    && self.zoom.is_magnified(
                        self.source_extent.unwrap_or(egui::Vec2::ONE),
                        self.last_viewport,
                    ) {
                    self.zoom.fit();
                } else if self.fullscreen {
                    self.toggle_fullscreen(&ctx);
                }
            }
        }
        // Mirror the original Windows command grammar without intercepting
        // path, search or other active editors.
        if !ctx.egui_wants_keyboard_input() {
            if ctx.input(|input| input.key_pressed(egui::Key::F2)) {
                self.start_rename();
            } else if ctx.input(|input| input.modifiers.command && input.key_pressed(egui::Key::O)) {
                self.choose_path(false);
            } else if ctx.input(|input| input.modifiers.shift && !input.modifiers.ctrl && !input.modifiers.alt && input.key_pressed(egui::Key::Space)) {
                self.toggle_slideshow(&ctx);
            } else if ctx.input(|input| input.key_pressed(egui::Key::Enter))
                && self.selected.is_some() && !self.fullscreen {
                self.toggle_fullscreen(&ctx);
            } else if ctx.input(|input| input.key_pressed(egui::Key::K)) {
                self.show_details = !self.show_details;
            } else if ctx.input(|input|
                input.modifiers.ctrl && input.key_pressed(egui::Key::Space)) {
                if self.selected.as_ref().is_some_and(|path| is_image(path)) {
                    if let Some(source) = self.source_extent {
                        let viewport = self.last_viewport.max(egui::Vec2::ONE);
                        self.zoom.actual_pixels(source, viewport);
                    }
                }
            } else if ctx.input(|input| input.key_pressed(egui::Key::Num0)
                && !input.modifiers.ctrl && !input.modifiers.alt) {
                self.zoom.fit();
            } else if ctx.input(|input| input.key_pressed(egui::Key::Space)
                && !input.modifiers.shift && !input.modifiers.ctrl) {
                self.toggle_playback(&ctx);
            }
        }
        // Never steal arrow keys from path/search edits or focused media controls.
        if !ctx.egui_wants_keyboard_input() && !self.folder_open_pending {
            let (back, forward, up, all, next, previous, extend) = ctx.input(|input| {
                let mods = input.modifiers;
                (
                    mods.alt && input.key_pressed(egui::Key::ArrowLeft),
                    mods.alt && input.key_pressed(egui::Key::ArrowRight),
                    mods.alt && input.key_pressed(egui::Key::ArrowUp),
                    mods.command && input.key_pressed(egui::Key::A),
                    !mods.alt && input.key_pressed(egui::Key::ArrowRight),
                    !mods.alt && input.key_pressed(egui::Key::ArrowLeft),
                    mods.shift || mods.ctrl,
                )
            });
            if back {
                self.go_back();
            } else if forward {
                self.go_forward();
            } else if up {
                if let Some(parent) = self.folder.parent().map(Path::to_path_buf) {
                    self.open_folder(parent);
                }
            } else if all {
                self.selection.select_all(&self.visible_media);
                self.present_selection();
            } else if next {
                self.step_media(1, extend);
            } else if previous {
                self.step_media(-1, extend);
            }
        }
        let mut prepare_rating = false;
        let mut prepare_batch = false;
        if !self.fullscreen {
        egui::Panel::top("header").show(root, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Diffractor").size(22.0).strong()
                    .color(Color32::from_rgb(220, 237, 244)));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button(self.language.badge()).clicked() {
                        self.language.toggle();
                    }
                    if ui.add_enabled(self.selected.is_some(),
                        egui::Button::new("⛶"))
                        .on_hover_text(tr(self.language, Text::Fullscreen))
                        .clicked() {
                        self.toggle_fullscreen(&ctx);
                    }
                });
            });
            ui.horizontal(|ui| {
                let can_back = self.history.back_target().is_some();
                if ui.add_enabled(can_back, egui::Button::new("←"))
                    .on_hover_text(tr(self.language, Text::HistoryBack)).clicked() {
                    self.go_back();
                }
                let can_forward = self.history.forward_target().is_some();
                if ui.add_enabled(can_forward, egui::Button::new("→"))
                    .on_hover_text(tr(self.language, Text::HistoryForward)).clicked() {
                    self.go_forward();
                }
                let parent = self.folder.parent().map(Path::to_path_buf);
                if ui.add_enabled(
                    parent.is_some(),
                    egui::Button::new(tr(self.language, Text::Up)),
                ).clicked() {
                    if let Some(parent) = parent {
                        self.open_folder(parent);
                    }
                }
                if ui.add_enabled(!self.folder_open_pending && !self.folder.as_os_str().is_empty(),
                    egui::Button::new("⟳"))
                    .on_hover_text(tr(self.language, Text::Refresh)).clicked() {
                    self.refresh();
                }
                if ui.add_enabled(!self.picker_busy && !self.folder_open_pending,
                    egui::Button::new(tr(self.language, Text::OpenFile))).clicked() {
                    self.choose_path(false);
                }
                if ui.add_enabled(!self.picker_busy && !self.folder_open_pending,
                    egui::Button::new(tr(self.language, Text::OpenFolder))).clicked() {
                    self.choose_path(true);
                }
                let edit = ui.add_sized(
                    [(ui.available_width() - 65.0).max(100.0), 28.0],
                    egui::TextEdit::singleline(&mut self.address).hint_text(tr(self.language, Text::AddressHint)),
                );
                if (edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)))
                    || ui.button(tr(self.language, Text::Open)).clicked()
                {
                    self.open_path(PathBuf::from(self.address.clone()));
                }
            });
            ui.label(RichText::new(tr(self.language, Text::SearchScope)).weak());
            ui.horizontal(|ui| {
                let return_visible = self.search_active || self.search_pending;
                let width_reserved = if return_visible { 185.0 } else { 92.0 };
                let edit = ui.add_sized(
                    [(ui.available_width() - width_reserved).max(100.0), 28.0],
                    egui::TextEdit::singleline(&mut self.search_text)
                        .hint_text(tr(self.language, Text::SearchHint)),
                );
                let enter = edit.lost_focus()
                    && ui.input(|i| i.key_pressed(egui::Key::Enter));
                let can_search = !self.folder_open_pending
                    && !self.folder.as_os_str().is_empty()
                    && !self.search_text.trim().is_empty();
                let clicked = ui.add_enabled(
                    can_search,
                    egui::Button::new(tr(self.language, Text::Search)),
                ).clicked();
                if can_search && (enter || clicked) {
                    self.search();
                }
                if return_visible && ui.button(tr(self.language, Text::BackToFolder)).clicked() {
                    self.open_folder(self.folder.clone());
                }
            });
        });
        let mut open = None;
        let mut needed_thumbnails = Vec::new();
        egui::Panel::left("items")
            .resizable(true)
            .default_size(300.0)
            .size_range(180.0..=500.0)
            .show(root, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(if self.search_active {
                            tr(self.language, Text::SearchResults)
                        } else {
                            tr(self.language, Text::FoldersMedia)
                        })
                        .strong(),
                    );
                    ui.label(RichText::new(self.entries.len().to_string()).weak());
                    if ui.selectable_label(self.grid_view, "▦")
                        .on_hover_text(tr(self.language, Text::GridView))
                        .clicked() {
                        self.grid_view = true;
                    }
                    if ui.selectable_label(!self.grid_view, "☷")
                        .on_hover_text(tr(self.language, Text::ListView))
                        .clicked() {
                        self.grid_view = false;
                    }
                });
                if self.selection.len() > 0 {
                    ui.label(
                        RichText::new(format!("{} {}", self.selection.len(),
                            tr(self.language, Text::SelectedCount)))
                            .small()
                            .color(Color32::from_rgb(180, 215, 237)),
                    );
                }
                if self.truncated {
                    ui.label(tr(self.language, Text::EntryLimit));
                }
                ui.separator();
                if self.grid_view {
                    let columns = grid_columns(ui.available_width());
                    let gap = ui.spacing().item_spacing.x;
                    let cell_width = ((ui.available_width() - gap * (columns - 1) as f32)
                        / columns as f32).max(96.0);
                    let row_count = self.entries.len().div_ceil(columns);
                    egui::ScrollArea::vertical().show_rows(ui, 152.0, row_count, |ui, rows| {
                        for row in rows {
                            ui.horizontal(|ui| {
                                for index in row * columns..((row + 1) * columns).min(self.entries.len()) {
                                    let entry = &self.entries[index];
                                    let selected = self.selection.contains(&entry.path);
                                    let name = if self.search_active {
                                        entry.path.strip_prefix(&self.folder)
                                            .unwrap_or(&entry.path).to_string_lossy()
                                    } else {
                                        entry.path.file_name().unwrap_or_default().to_string_lossy()
                                    };
                                    let frame = egui::Frame::new()
                                        .fill(if selected { ui.visuals().selection.bg_fill } else {
                                            ui.visuals().faint_bg_color
                                        })
                                        .corner_radius(7.0)
                                        .inner_margin(egui::Margin::same(5))
                                        .show(ui, |ui| {
                                            ui.set_width((cell_width - 12.0).max(84.0));
                                            ui.allocate_ui_with_layout(
                                                egui::vec2((cell_width - 22.0).max(72.0), 96.0),
                                                egui::Layout::centered_and_justified(egui::Direction::TopDown),
                                                |ui| {
                                                    if let Some(texture) = self.thumbnails.get(&entry.path) {
                                                        let extent = texture.size_vec2();
                                                        let scale = ((cell_width - 22.0) / extent.x)
                                                            .min(96.0 / extent.y).min(1.0);
                                                        if ui.add(egui::Image::new(texture)
                                                            .fit_to_exact_size(extent * scale)
                                                            .sense(egui::Sense::click())).clicked() {
                                                            open = Some((entry.path.clone(), entry.directory,
                                                                selection_intent(ui.input(|i| i.modifiers))));
                                                        }
                                                    } else {
                                                        if !entry.directory
                                                            && (is_image(&entry.path) || is_video(&entry.path)) {
                                                            needed_thumbnails.push(entry.path.clone());
                                                        }
                                                        let symbol = if entry.directory { "▰" }
                                                            else if is_audio(&entry.path) { "♫" }
                                                            else { "▧" };
                                                        ui.label(RichText::new(symbol).size(30.0).weak());
                                                    }
                                                },
                                            );
                                            if ui.add(egui::Label::new(name.to_string()).truncate()
                                                .sense(egui::Sense::click()))
                                                .on_hover_text(name.as_ref()).clicked() {
                                                open = Some((entry.path.clone(), entry.directory,
                                                    selection_intent(ui.input(|i| i.modifiers))));
                                            }
                                        });
                                    if frame.response.interact(egui::Sense::click()).clicked() {
                                        open = Some((entry.path.clone(), entry.directory,
                                            selection_intent(ui.input(|i| i.modifiers))));
                                    }
                                }
                            });
                        }
                    });
                } else {
                egui::ScrollArea::vertical().show_rows(ui, 56.0, self.entries.len(), |ui, rows| {
                    for entry in &self.entries[rows] {
                        let name = if self.search_active {
                            entry
                                .path
                                .strip_prefix(&self.folder)
                                .unwrap_or(&entry.path)
                                .to_string_lossy()
                        } else {
                            entry.path.file_name().unwrap_or_default().to_string_lossy()
                        };
                        let selected = self.selection.contains(&entry.path);
                        let row = egui::Frame::new()
                            .fill(if selected { ui.visuals().selection.bg_fill } else {
                                Color32::TRANSPARENT
                            })
                            .corner_radius(6.0)
                            .inner_margin(egui::Margin::symmetric(6, 4))
                            .show(ui, |ui| {
                            ui.set_min_height(48.0);
                            ui.horizontal(|ui| {
                            let media = !entry.directory
                                && (is_image(&entry.path) || is_video(&entry.path));
                            ui.allocate_ui_with_layout(
                                egui::vec2(64.0, 48.0),
                                egui::Layout::centered_and_justified(egui::Direction::LeftToRight),
                                |ui| {
                                    if let Some(texture) = self.thumbnails.get(&entry.path) {
                                        let extent = texture.size_vec2();
                                        let scale = (64.0 / extent.x).min(48.0 / extent.y).min(1.0);
                                        if ui
                                            .add(
                                                egui::Image::new(texture)
                                                    .fit_to_exact_size(extent * scale)
                                                    .sense(egui::Sense::click()),
                                            )
                                            .clicked()
                                        {
                                            open = Some((entry.path.clone(), entry.directory, selection_intent(ui.input(|i| i.modifiers))));
                                        }
                                    } else {
                                        if media {
                                            needed_thumbnails.push(entry.path.clone());
                                        }
                                        // Distinct, low-cost placeholders; no decorative
                                        // loading animation is needed for every visible row.
                                        let (symbol, tint, kind) = if entry.directory {
                                            ("▰", Color32::from_rgb(187, 159, 112),
                                                tr(self.language, Text::Folder))
                                        } else if is_audio(&entry.path) {
                                            ("♫", Color32::from_rgb(127, 184, 193),
                                                tr(self.language, Text::Audio))
                                        } else {
                                            ("▧", Color32::from_rgb(114, 124, 135), "")
                                        };
                                        ui.label(RichText::new(symbol).size(25.0).color(tint))
                                            .on_hover_text(kind);
                                    }
                                },
                            );
                            let label = name.to_string();
                            if ui
                                .add(
                                    egui::Label::new(label)
                                        .halign(egui::Align::Min)
                                        .truncate()
                                        .sense(egui::Sense::click()),
                                )
                                .on_hover_text(name.as_ref())
                                .clicked()
                            {
                                open = Some((entry.path.clone(), entry.directory, selection_intent(ui.input(|i| i.modifiers))));
                            }
                            });
                        });
                        if row.response.interact(egui::Sense::click()).clicked() {
                            open = Some((entry.path.clone(), entry.directory, selection_intent(ui.input(|i| i.modifiers))));
                        }
                    }
                });
                }
            });
        for path in needed_thumbnails {
            self.request_thumbnail(path);
        }
        if let Some((path, directory, intent)) = open {
            if directory {
                self.open_folder(path);
            } else {
                self.select_media(path, intent);
            }
        }
        if self.show_details && self.selection.len() > 1 {
            let images = self.selection.iter().filter(|path| is_image(path)).count();
            let videos = self.selection.iter().filter(|path| is_video(path)).count();
            let audio = self.selection.iter().filter(|path| is_audio(path)).count();
            egui::Panel::right("selection-summary")
                .resizable(true)
                .default_size(230.0)
                .size_range(180.0..=400.0)
                .show(root, |ui| {
                    ui.heading(tr(self.language, Text::SelectionSummary));
                    ui.label(RichText::new(format!("{} {}",
                        self.selection.len(),
                        tr(self.language, Text::SelectedCount))).strong());
                    ui.separator();
                    if images > 0 {
                        ui.horizontal(|ui| {
                            ui.label(tr(self.language, Text::Images));
                            ui.label(images.to_string());
                        });
                    }
                    if videos > 0 {
                        ui.horizontal(|ui| {
                            ui.label(tr(self.language, Text::Videos));
                            ui.label(videos.to_string());
                        });
                    }
                    if audio > 0 {
                        ui.horizontal(|ui| {
                            ui.label(tr(self.language, Text::Audio));
                            ui.label(audio.to_string());
                        });
                    }
                    ui.add_space(12.0);
                    if images == self.selection.len() {
                        if ui.add_enabled(!self.rating_busy && !self.rename_busy && self.rename_draft.is_none(), egui::Button::new(tr(self.language, Text::BatchEdit))).clicked() {
                            prepare_batch = true;
                        }
                    } else {
                        ui.label(RichText::new(tr(self.language, Text::BatchUnavailable)).weak());
                    }
                });
        } else if self.show_details && self.metadata.is_some() {
            let metadata = self.metadata.as_ref().expect("checked metadata presence");
            egui::Panel::right("metadata")
                .resizable(true)
                .default_size(230.0)
                .size_range(180.0..=400.0)
                .show(root, |ui| {
                    ui.label(RichText::new(tr(self.language, Text::MediaDetails)).strong());
                    ui.label(RichText::new(tr(self.language, Text::EmbeddedDetails)).weak());
                    ui.separator();
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        if *metadata == Metadata::default() {
                            ui.label(tr(self.language, Text::NoDetails));
                        }
                        let camera = if metadata.camera_model.is_empty() {
                            &metadata.camera_make
                        } else {
                            &metadata.camera_model
                        };
                        for (label, value) in [
                            (tr(self.language, Text::Title), &metadata.title),
                            (tr(self.language, Text::Camera), camera),
                            (tr(self.language, Text::Lens), &metadata.lens),
                            (tr(self.language, Text::Description), &metadata.description),
                            (tr(self.language, Text::Keywords), &metadata.keywords),
                        ] {
                            if !value.is_empty() {
                                ui.label(RichText::new(label).weak());
                                ui.label(value);
                                ui.add_space(8.0);
                            }
                        }
                        if let Some(rating) = metadata.rating {
                            ui.label(RichText::new(tr(self.language, Text::Rating)).weak());
                            ui.label(if rating == -1 {
                                tr(self.language, Text::Rejected).to_owned()
                            } else {
                                format!("{rating} / 5")
                            });
                        }
                        if self.selected.as_ref().is_some_and(|path| is_image(path))
                            && ui.add_enabled(!self.rating_busy && !self.rename_busy && self.rename_draft.is_none(), egui::Button::new(tr(self.language, Text::EditRating))).clicked() {
                            prepare_rating = true;
                        }
                    });
                });
        } else if self.show_details && self.texture.is_some() {
            egui::Panel::right("metadata-error")
                .default_size(230.0)
                .show(root, |ui| {
                    ui.label(RichText::new(tr(self.language, Text::MediaDetails)).strong());
                    ui.label(tr(self.language, Text::ReadDetailsFailed));
                });
        }
        } // Keep native Items panels hidden while displaying the same media fullscreen.
        if prepare_rating {
            if let Some(path) = self.selected.clone() {
                if self.slideshow { self.stop_slideshow(); }
                self.rating_busy = true;
                self.status = Status::Text(Text::OpeningMedia);
                if self.rating_sender.send(rating::Job::Prepare(path)).is_err() {
                    self.rating_busy = false;
                    self.status = Status::Error("Metadata worker is unavailable.".into());
                }
            }
        }
        if prepare_batch {
            if self.slideshow { self.stop_slideshow(); }
            let paths: Vec<_> = self.visible_media.iter().filter(|path| self.selection.contains(path)).cloned().collect();
            if paths.len() == self.selection.len() && !paths.is_empty() {
                self.rating_busy = true;
                self.status = Status::Text(Text::OpeningMedia);
                if self.rating_sender.send(rating::Job::PrepareBatch(paths)).is_err() {
                    self.rating_busy = false;
                    self.status = Status::Error("Metadata worker is unavailable.".into());
                }
            }
        }
        if let Some(frame) = self
            .playback_shared
            .as_ref()
            .and_then(|shared| shared.take_frame())
        {
            #[cfg(feature = "visual-audit")]
            {
                self.audit_frame_seen = true;
            }
            if frame.height > 0 && self.selected.as_ref().is_some_and(|path| is_video(path)) {
                self.video_aspect = Some(frame.width as f32 / frame.height as f32);
            }
            let image =
                ColorImage::from_rgba_unmultiplied([frame.width, frame.height], &frame.rgba);
            if let Some(texture) = &mut self.texture {
                texture.set(image, egui::TextureOptions::LINEAR);
            } else {
                self.texture =
                    Some(ctx.load_texture("playback", image, egui::TextureOptions::LINEAR));
            }
        }
        egui::CentralPanel::default().show(root, |ui| {
            ui.horizontal(|ui| {
                if ui.add_enabled(!self.visible_media.is_empty() && !self.folder_open_pending,
                    egui::Button::new(tr(self.language, if self.slideshow { Text::StopSlideshow } else { Text::Slideshow }))).clicked() {
                    self.toggle_slideshow(&ctx);
                }
                ui.add_enabled(!self.slideshow, egui::DragValue::new(&mut self.slide_seconds).range(1..=30).suffix(" s"))
                    .on_hover_text(tr(self.language, Text::SlideDelay));
            });
            if self.fullscreen && ui.button(tr(self.language, Text::ExitFullscreen)).clicked() {
                self.toggle_fullscreen(&ctx);
            }
            if let Some(file_name) = self.selected.as_ref().map(|path| {
                path.file_name().unwrap_or_default().to_string_lossy().into_owned()
            }) {
                ui.horizontal(|ui| {
                    if ui.button("‹").on_hover_text(tr(self.language, Text::Previous)).clicked() {
                        self.step_media(-1, false);
                    }
                    if ui.button("›").on_hover_text(tr(self.language, Text::Next)).clicked() {
                        self.step_media(1, false);
                    }
                    if ui.add_enabled(!self.rename_busy && !self.rating_busy && self.rating_edit.is_none() && self.batch_edit.is_none(), egui::Button::new(tr(self.language, Text::Rename))).clicked() {
                        self.start_rename();
                    }
                    ui.label(RichText::new(file_name).strong().size(17.0));
                });
            } else if self.comparison.is_some() {
                ui.heading(tr(self.language, Text::Compare));
            } else if self.selection.len() > 1 {
                ui.heading(tr(self.language, Text::SelectionSummary));
            } else {
                ui.heading(tr(self.language, Text::PhotoPreview));
            }
            ui.label(RichText::new(self.status.display(self.language)).color(Color32::LIGHT_GRAY));
            if self
                .selected
                .as_ref()
                .is_some_and(|path| is_video(path) || is_audio(path))
            {
                ui.horizontal(|ui| {
                    let label = if self.playback_state == 3 {
                        tr(self.language, Text::Pause)
                    } else if self.playback_state == 4 {
                        tr(self.language, Text::Resume)
                    } else if self.playback_state == 6 {
                        tr(self.language, Text::Replay)
                    } else {
                        tr(self.language, Text::Play)
                    };
                    if ui
                        .add_enabled(
                            !matches!(self.playback_state, 1 | 2),
                            egui::Button::new(label),
                        )
                        .clicked()
                    {
                        self.toggle_playback(&ctx);
                    }
                    if self.playback_shared.is_some() && ui.button(tr(self.language, Text::Stop)).clicked() {
                        self.slideshow = false;
                        self.slide_deadline = None;
                        self.stop_playback();
                        self.status = Status::Text(Text::Stopped);
                    }
                    if self.playback_duration > 0 {
                        let mut seconds = self.playback_time as f64 / 1000.0;
                        let duration = self.playback_duration as f64 / 1000.0;
                        if ui
                            .add(egui::Slider::new(&mut seconds, 0.0..=duration).show_value(false))
                            .changed()
                        {
                            let _ = self.playback_sender.send(PlaybackRequest::Seek(
                                self.playback_generation,
                                (seconds * 1000.0) as i64,
                            ));
                        }
                        ui.label(format!(
                            "{}:{:02} / {}:{:02}",
                            (seconds as i64) / 60,
                            (seconds as i64) % 60,
                            (duration as i64) / 60,
                            (duration as i64) % 60
                        ));
                    }
                });
            }
            let still_image = self.selected.as_ref().is_some_and(|path| is_image(path));
            if still_image && self.texture.is_some() {
                ui.horizontal(|ui| {
                    let source = self.source_extent.unwrap_or(egui::Vec2::ONE);
                    let viewport = self.last_viewport.max(egui::Vec2::ONE);
                    if ui.button("−").on_hover_text(tr(self.language, Text::ZoomOut)).clicked() {
                        self.zoom.step(-1, source, viewport, viewport * 0.5);
                    }
                    if ui.button("+").on_hover_text(tr(self.language, Text::ZoomIn)).clicked() {
                        self.zoom.step(1, source, viewport, viewport * 0.5);
                    }
                    if ui.button("1:1").on_hover_text(tr(self.language, Text::ActualPixels)).clicked() {
                        self.zoom.actual_pixels(source, viewport);
                    }
                    if ui.button(tr(self.language, Text::Fit)).clicked() {
                        self.zoom.fit();
                    }
                    ui.toggle_value(&mut self.navigator_enabled, "▣")
                        .on_hover_text(tr(self.language, Text::Navigator));
                    ui.label(format!("{:.0}%", self.zoom.scale(source, viewport) * 100.0));
                    ui.label(
                        RichText::new(tr(self.language, Text::ZoomHelp))
                            .small()
                            .color(Color32::GRAY),
                    );
                });
            }
            if let Some(compare) = &self.comparison {
                let source = if self.comparison_active_right { compare.right_source } else { compare.left_source };
                let viewport = self.comparison_viewport.max(egui::Vec2::ONE);
                ui.horizontal(|ui| {
                    if ui.button("−").on_hover_text(tr(self.language, Text::ZoomOut)).clicked() {
                        self.comparison_zoom.step(-1, source, viewport, viewport * 0.5);
                    }
                    if ui.button("+").on_hover_text(tr(self.language, Text::ZoomIn)).clicked() {
                        self.comparison_zoom.step(1, source, viewport, viewport * 0.5);
                    }
                    if ui.button("1:1").on_hover_text(tr(self.language, Text::ActualPixels)).clicked() {
                        self.comparison_zoom.actual_pixels(source, viewport);
                    }
                    if ui.button(tr(self.language, Text::Fit)).clicked() {
                        self.comparison_zoom.fit();
                    }
                    ui.label(format!("{:.0}%", self.comparison_zoom.scale(source, viewport) * 100.0));
                    ui.label(RichText::new(tr(self.language, Text::ZoomHelp)).small().color(Color32::GRAY));
                });
            }
            ui.add_space(12.0);
            if let Some(compare) = &self.comparison {
                let available = ui.available_size().max(egui::Vec2::ZERO);
                let (viewport, _) = ui.allocate_exact_size(available, egui::Sense::hover());
                let gap = 12.0;
                let pane_width = ((viewport.width() - gap) * 0.5).max(0.0);
                let left_rect = egui::Rect::from_min_size(
                    viewport.min, egui::vec2(pane_width, viewport.height()));
                let right_rect = egui::Rect::from_min_size(
                    egui::pos2(viewport.min.x + pane_width + gap, viewport.min.y),
                    egui::vec2(pane_width, viewport.height()));
                self.comparison_viewport = left_rect.size();
                let mut needs_full = false;
                for (right, texture, source, bounds) in [
                    (false, &compare.left, compare.left_source, left_rect),
                    (true, &compare.right, compare.right_source, right_rect),
                ] {
                    if source.x <= 0.0 || source.y <= 0.0 || bounds.is_negative() {
                        continue;
                    }
                    let response = ui.interact(bounds, ui.scope_id().with(("compare", right)), egui::Sense::click_and_drag());
                    if response.clicked() || response.dragged() || response.hovered() {
                        self.comparison_active_right = right;
                    }
                    if response.hovered() {
                        let (scroll, control, pointer) = ui.input(|i| (i.smooth_scroll_delta.y, i.modifiers.ctrl, i.pointer.hover_pos()));
                        if control {
                            self.comparison_wheel_remainder = (self.comparison_wheel_remainder + scroll).clamp(-960.0, 960.0);
                            let anchor = pointer.unwrap_or(bounds.center()) - bounds.min;
                            let mut steps = 0;
                            while self.comparison_wheel_remainder.abs() >= 60.0 && steps < 6 {
                                let direction = if self.comparison_wheel_remainder > 0.0 { 1 } else { -1 };
                                self.comparison_zoom.step(direction, source, bounds.size(), anchor);
                                self.comparison_wheel_remainder -= direction as f32 * 60.0;
                                steps += 1;
                            }
                        } else { self.comparison_wheel_remainder = 0.0; }
                    }
                    if response.dragged() {
                        self.comparison_zoom.pan_by(response.drag_delta(), source, bounds.size());
                    }
                    let rect = self.comparison_zoom.rect(bounds, source);
                    let size = texture.size_vec2();
                    needs_full |= rect.width() > size.x * 1.15 || rect.height() > size.y * 1.15;
                    ui.painter_at(bounds).image(
                        texture.id(),
                        rect,
                        egui::Rect::from_min_max(
                            egui::Pos2::ZERO, egui::pos2(1.0, 1.0),
                        ),
                        Color32::WHITE,
                    );
                }
                if needs_full && !self.comparison_full_requested && !self.folder_open_pending {
                    if let Some((left, right)) = self.comparison_pair() {
                        self.comparison_full_requested = true;
                        let _ = self.sender.send(Request::FullCompare(self.image_generation, left, right));
                    }
                }
            } else if let Some(texture) = self.texture.clone() {
                let available = ui.available_size();
                // Keep a stopped video centered too; the preview and player share
                // the same aspect-driven media stage.
                if self.video_aspect.is_some() {
                    let aspect = self.video_aspect.unwrap_or(1.0);
                    let width = available.x.min(available.y * aspect).max(0.0);
                    let (viewport, _) = ui.allocate_exact_size(
                        available.max(egui::Vec2::ZERO),
                        egui::Sense::hover(),
                    );
                    let display = egui::vec2(width, width / aspect);
                    let rect = egui::Rect::from_center_size(viewport.center(), display);
                    ui.painter_at(viewport).image(
                        texture.id(),
                        rect,
                        egui::Rect::from_min_max(
                            egui::Pos2::ZERO,
                            egui::pos2(1.0, 1.0),
                        ),
                        Color32::WHITE,
                    );
                } else if still_image {
                    let (viewport, response) = ui.allocate_exact_size(
                        available.max(egui::Vec2::ZERO),
                        egui::Sense::click_and_drag(),
                    );
                    let source = self.source_extent.unwrap_or_else(|| texture.size_vec2());
                    self.last_viewport = viewport.size();
                    let initial_rect = self.zoom.rect(viewport, source);
                    if self.navigator_last_rect != Some(initial_rect)
                        || (response.hovered() && ctx.input(|i| i.pointer.delta() != egui::Vec2::ZERO)) {
                        self.navigator_until = Some(Instant::now() + Duration::from_secs(3));
                    }
                    let nav_scale = (160.0 / source.x).min(160.0 / source.y)
                        .min((viewport.width() - 16.0).max(0.0) / source.x)
                        .min((viewport.height() - 16.0).max(0.0) / source.y);
                    let nav_bounds = egui::Rect::from_min_size(viewport.min + egui::vec2(8.0, 8.0), source * nav_scale);
                    let show_navigator = self.navigator_enabled && nav_scale > 0.0
                        && self.zoom.is_magnified(source, viewport.size())
                        && self.navigator_until.is_some_and(|until| until > Instant::now());
                    let nav_response = show_navigator.then(|| ui.interact(
                        nav_bounds, ui.scope_id().with("zoom_navigator"), egui::Sense::click_and_drag()));
                    if response.hovered() {
                        let (scroll, control, pointer) = ui.input(|i| {
                            (i.smooth_scroll_delta.y, i.modifiers.ctrl, i.pointer.hover_pos())
                        });
                        // The original Items contract reserves Ctrl+wheel at
                        // Fit for thumbnail sizing; only zoom mode consumes it.
                        if control && self.zoom.is_magnified(source, viewport.size()) {
                            self.zoom_wheel_remainder =
                                (self.zoom_wheel_remainder + scroll).clamp(-960.0, 960.0);
                            let anchor = pointer.unwrap_or(viewport.center()) - viewport.min;
                            let mut steps = 0;
                            while self.zoom_wheel_remainder.abs() >= 60.0 && steps < 6 {
                                let direction = if self.zoom_wheel_remainder > 0.0 { 1 } else { -1 };
                                self.zoom.step(direction, source, viewport.size(), anchor);
                                self.zoom_wheel_remainder -= direction as f32 * 60.0;
                                steps += 1;
                            }
                        } else {
                            self.zoom_wheel_remainder = 0.0;
                        }
                    }
                    if response.dragged() && !nav_response.as_ref().is_some_and(|nav| nav.dragged()
                        || nav.contains_pointer()) {
                        self.zoom.pan_by(response.drag_delta(), source, viewport.size());
                    }
                    if let Some(nav) = &nav_response {
                        if nav.hovered() || nav.dragged() {
                            self.navigator_until = Some(Instant::now() + Duration::from_secs(3));
                        }
                        if (nav.clicked() || nav.dragged()) && nav.interact_pointer_pos().is_some() {
                            let point = nav.interact_pointer_pos().unwrap();
                            self.zoom.set_center((point - nav_bounds.min) / nav_bounds.size(), source, viewport.size());
                        }
                    }
                    let rect = self.zoom.rect(viewport, source);
                    ui.painter_at(viewport).image(
                        texture.id(),
                        rect,
                        egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
                        Color32::WHITE,
                    );
                    self.navigator_last_rect = Some(rect);
                    if show_navigator {
                        let painter = ui.painter_at(viewport);
                        painter.rect_filled(nav_bounds.expand(2.0), 2.0, Color32::from_black_alpha(220));
                        painter.image(texture.id(), nav_bounds,
                            egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)), Color32::WHITE);
                        let visible = rect.intersect(viewport);
                        let start = (visible.min - rect.min) / rect.size();
                        let end = (visible.max - rect.min) / rect.size();
                        let marker = egui::Rect::from_min_max(nav_bounds.min + start * nav_bounds.size(),
                            nav_bounds.min + end * nav_bounds.size());
                        painter.rect_stroke(marker, 0.0, egui::Stroke::new(2.0, Color32::WHITE), egui::StrokeKind::Inside);
                        if let Some(until) = self.navigator_until {
                            ctx.request_repaint_after(until.saturating_duration_since(Instant::now()));
                        }
                    }
                    // Source-space scale is invariant when a thumbnail/preview is
                    // replaced by a better texture. Decode full pixels only when
                    // the visible rectangle outgrows the current decoded raster.
                    let displayed = rect.size();
                    let decoded = texture.size_vec2();
                    if (displayed.x > decoded.x * 1.15 || displayed.y > decoded.y * 1.15)
                        && !self.full_image_requested && !self.folder_open_pending {
                        self.full_image_requested = true;
                        if let Some(path) = &self.selected {
                            let _ = self.sender.send(
                                Request::FullImage(self.image_generation, path.clone())
                            );
                        }
                    }
                } else {
                    ui.add(
                        egui::Image::new(&texture)
                            .max_size(available)
                            .maintain_aspect_ratio(true),
                    );
                }
            } else if self.selected.as_ref().is_some_and(|path| is_audio(path)) {
                // Audio has no raster frame; a compact centered stage gives the
                // player an intentional resting surface instead of empty space.
                let available_height = ui.available_height();
                ui.vertical_centered(|ui| {
                    ui.add_space(((available_height - 165.0) * 0.4).max(16.0));
                    ui.label(RichText::new("♫").size(80.0)
                        .color(Color32::from_rgb(127, 184, 193)));
                    ui.add_space(8.0);
                    ui.label(RichText::new(tr(self.language, Text::Audio))
                        .size(18.0).strong());
                });
            }
        });
        if let Some(mut draft) = self.rename_draft.take() {
            let mut action = None;
            egui::Window::new(tr(self.language, Text::Rename)).collapsible(false).resizable(true).show(&ctx, |ui| {
                ui.label(draft.source.to_string_lossy());
                ui.label(tr(self.language, Text::RenameName));
                if ui.text_edit_singleline(&mut draft.name).changed() {
                    draft.plan = None;
                    draft.error = None;
                }
                if ui.add_enabled(!self.rename_busy && !draft.name.is_empty(), egui::Button::new(tr(self.language, Text::RenameAnalyze))).clicked() {
                    action = Some(1);
                }
                if let Some(error) = &draft.error { ui.colored_label(Color32::LIGHT_RED, error); }
                if let Some(plan) = &draft.plan {
                    ui.label(tr(self.language, Text::RenameReview));
                    egui::ScrollArea::vertical().max_height(180.0).show(ui, |ui| {
                        for (from, to) in &plan.moves {
                            ui.label(format!("{} → {}", from.display(), to.display()));
                        }
                    });
                }
                ui.horizontal(|ui| {
                    if ui.add_enabled(!self.rename_busy && draft.plan.is_some(), egui::Button::new(tr(self.language, Text::RenameRun))).clicked() {
                        action = Some(2);
                    }
                    if ui.button(tr(self.language, Text::Cancel)).clicked() { action = Some(3); }
                });
            });
            match action {
                Some(1) => {
                    draft.plan = None;
                    draft.error = None;
                    self.rename_busy = true;
                    if self.rename_sender.send(rename::Job::Analyze(draft.source.clone(), draft.name.clone())).is_err() {
                        self.rename_busy = false;
                        draft.error = Some("Rename worker is unavailable.".into());
                    }
                    self.rename_draft = Some(draft);
                }
                Some(2) => {
                    if let Some(plan) = draft.plan.take() {
                        self.rename_busy = true;
                        if self.rename_sender.send(rename::Job::Run(plan)).is_err() {
                            self.rename_busy = false;
                            self.rename_result = Some("Rename worker is unavailable.".into());
                        }
                    }
                }
                Some(3) => {},
                _ => self.rename_draft = Some(draft),
            }
        }
        if let Some(message) = self.rename_result.take() {
            let mut close = false;
            egui::Window::new(tr(self.language, Text::RenameResult)).collapsible(false).show(&ctx, |ui| {
                ui.label(&message);
                if ui.button(tr(self.language, Text::Done)).clicked() { close = true; }
            });
            if !close { self.rename_result = Some(message); }
        }
        if let Some(message) = self.operation_error.take() {
            let mut close = false;
            egui::Window::new(tr(self.language, Text::OperationResult)).collapsible(false).show(&ctx, |ui| {
                ui.label(&message);
                if ui.button(tr(self.language, Text::Done)).clicked() { close = true; }
            });
            if !close { self.operation_error = Some(message); }
        }
        if let Some(mut draft) = self.batch_edit.take() {
            let mut action = None;
            egui::Window::new(tr(self.language, Text::BatchEdit))
                .collapsible(false).resizable(true).show(&ctx, |ui| {
                    ui.label(tr(self.language, Text::BatchReview));
                    ui.label(format!("{} {}", draft.items.len(), tr(self.language, Text::SelectedCount)));
                    egui::ScrollArea::vertical().max_height(160.0).show(ui, |ui| {
                        for (path, _) in &draft.items { ui.label(path.to_string_lossy()); }
                    });
                    ui.label(tr(self.language, Text::RatingEffect));
                    ui.checkbox(&mut draft.set_title, tr(self.language, Text::BatchSetTitle));
                    ui.add_enabled(draft.set_title, egui::TextEdit::singleline(&mut draft.title));
                    ui.checkbox(&mut draft.set_description, tr(self.language, Text::BatchSetDescription));
                    ui.add_enabled(draft.set_description, egui::TextEdit::multiline(&mut draft.description).desired_rows(2));
                    if draft.set_title || draft.set_description {
                        ui.label(egui::RichText::new(tr(self.language, Text::BatchEmptyClears)).weak());
                    }
                    ui.horizontal_wrapped(|ui| {
                        ui.selectable_value(&mut draft.rating, None, tr(self.language, Text::BatchNoRating));
                        for value in -1..=5 {
                            let label = if value < 0 { tr(self.language, Text::Rejected).to_owned() } else { value.to_string() };
                            ui.selectable_value(&mut draft.rating, Some(value), label);
                        }
                    });
                    ui.label(tr(self.language, Text::AddTags));
                    ui.add(egui::TextEdit::multiline(&mut draft.add_tags).desired_rows(2));
                    ui.label(tr(self.language, Text::RemoveTags));
                    ui.add(egui::TextEdit::multiline(&mut draft.remove_tags).desired_rows(2));
                    ui.horizontal(|ui| {
                        let has_changes = draft.set_title || draft.set_description || draft.rating.is_some()
                            || !draft.add_tags.trim().is_empty() || !draft.remove_tags.trim().is_empty();
                        if ui.add_enabled(has_changes, egui::Button::new(tr(self.language, Text::RatingSave))).clicked() { action = Some(true); }
                        if ui.button(tr(self.language, Text::Cancel)).clicked() { action = Some(false); }
                    });
                });
            match action {
                Some(true) => {
                    let edits = rating::Edits {
                        rating: draft.rating,
                        title: draft.set_title.then_some(draft.title),
                        description: draft.set_description.then_some(draft.description),
                        add_tags: draft.add_tags.lines().map(str::trim).filter(|tag| !tag.is_empty()).map(str::to_owned).collect(),
                        remove_tags: draft.remove_tags.lines().map(str::trim).filter(|tag| !tag.is_empty()).map(str::to_owned).collect(),
                        ..Default::default()
                    };
                    self.rating_busy = true;
                    self.batch_cancel.store(false, Ordering::Release);
                    self.batch_progress = Some((0, draft.items.len()));
                    self.status = Status::Text(Text::RatingSaving);
                    if self.rating_sender.send(rating::Job::SaveBatch(draft.items, edits, Arc::clone(&self.batch_cancel))).is_err() {
                        self.rating_busy = false;
                        self.batch_progress = None;
                        self.status = Status::Error("Metadata worker is unavailable.".into());
                    }
                }
                Some(false) => self.status = Status::Empty,
                None => self.batch_edit = Some(draft),
            }
        }
        if let Some((done, total)) = self.batch_progress {
            egui::Window::new(tr(self.language, Text::RatingSaving)).collapsible(false).resizable(false).show(&ctx, |ui| {
                ui.label(format!("{done} / {total}"));
                ui.add(egui::ProgressBar::new(if total == 0 { 0.0 } else { done as f32 / total as f32 }));
                if self.batch_cancel.load(Ordering::Acquire) {
                    ui.label(tr(self.language, Text::BatchCancelling));
                } else if ui.button(tr(self.language, Text::Cancel)).clicked() {
                    self.batch_cancel.store(true, Ordering::Release);
                }
            });
        }
        if let Some((succeeded, failures, reload_failed, cancelled)) = self.batch_result.take() {
            let mut close = false;
            egui::Window::new(tr(self.language, Text::BatchResult)).collapsible(false).show(&ctx, |ui| {
                ui.label(format!("{}: {succeeded}", tr(self.language, Text::BatchSucceeded)));
                ui.label(format!("{}: {}", tr(self.language, Text::BatchFailed), failures.len()));
                ui.label(format!("{}: {}", tr(self.language, Text::BatchCancelled), cancelled.len()));
                egui::ScrollArea::vertical().max_height(200.0).show(ui, |ui| {
                    for (path, error) in &failures { ui.label(format!("{}: {error}", path.display())); }
                    for path in &reload_failed { ui.label(format!("{}: {}", tr(self.language, Text::ReloadFailed), path.display())); }
                    for path in &cancelled { ui.label(format!("{}: {}", tr(self.language, Text::BatchCancelled), path.display())); }
                });
                if ui.button(tr(self.language, Text::Done)).clicked() { close = true; }
            });
            if !close { self.batch_result = Some((succeeded, failures, reload_failed, cancelled)); }
        }
        if let Some((path, original, mut edited, snapshot, mut add_tags, mut remove_tags)) = self.rating_edit.take() {
            let mut action = None;
            egui::Window::new(tr(self.language, Text::EditRating))
                .collapsible(false).resizable(false).show(&ctx, |ui| {
                    ui.label(tr(self.language, Text::RatingReview));
                    ui.label(path.to_string_lossy());
                    ui.label(tr(self.language, Text::RatingEffect));
                    ui.label(tr(self.language, Text::Title));
                    ui.text_edit_singleline(&mut edited.title);
                    ui.label(tr(self.language, Text::Description));
                    ui.add(egui::TextEdit::multiline(&mut edited.description).desired_rows(2));
                    ui.label(format!("{}: {}", tr(self.language, Text::Keywords), original.keywords));
                    ui.label(tr(self.language, Text::AddTags));
                    ui.add(egui::TextEdit::multiline(&mut add_tags).desired_rows(2));
                    ui.label(tr(self.language, Text::RemoveTags));
                    ui.add(egui::TextEdit::multiline(&mut remove_tags).desired_rows(2));
                    ui.label(tr(self.language, Text::Rating));
                    ui.horizontal_wrapped(|ui| {
                        for rating in -1..=5 {
                            let label = if rating < 0 { tr(self.language, Text::Rejected).to_owned() }
                                else { rating.to_string() };
                            if ui.selectable_label(edited.rating.unwrap_or(0) == rating, label).clicked() {
                                edited.rating = Some(rating);
                            }
                        }
                    });
                    ui.horizontal(|ui| {
                        if ui.button(tr(self.language, Text::RatingSave)).clicked() { action = Some(true); }
                        if ui.button(tr(self.language, Text::Cancel)).clicked() { action = Some(false); }
                    });
                });
            match action {
                Some(true) => {
                    let changes = rating::Edits {
                        title: (edited.title != original.title).then_some(edited.title),
                        description: (edited.description != original.description).then_some(edited.description),
                        rating: (edited.rating != original.rating).then_some(edited.rating).flatten(),
                        add_tags: add_tags.lines().map(str::trim).filter(|tag| !tag.is_empty()).map(str::to_owned).collect(),
                        remove_tags: remove_tags.lines().map(str::trim).filter(|tag| !tag.is_empty()).map(str::to_owned).collect(),
                    };
                    if changes.title.is_none() && changes.description.is_none() && changes.rating.is_none()
                        && changes.add_tags.is_empty() && changes.remove_tags.is_empty() {
                        self.status = Status::Empty;
                        return;
                    }
                    self.rating_busy = true;
                    self.status = Status::Text(Text::RatingSaving);
                    if self.rating_sender.send(rating::Job::Save(path, changes, snapshot)).is_err() {
                        self.rating_busy = false;
                        self.status = Status::Error("Metadata worker is unavailable.".into());
                    }
                }
                Some(false) => self.status = Status::Empty,
                None => self.rating_edit = Some((path, original, edited, snapshot, add_tags, remove_tags)),
            }
        }
        #[cfg(feature = "visual-audit")]
        if self.texture.is_some()
            && self.thumbnail_pending.is_empty()
            && (!self.audit_autoplay || (self.audit_frame_seen && self.playback_state == 3))
        {
            if let Some(path) = self.audit_output.take() {
                let callback_ctx = ctx.clone();
                ctx.request_screenshot(move |image| {
                    use std::io::Write;
                    if let Ok(mut file) = std::fs::File::create(path) {
                        let _ = write!(file, "P6\n{} {}\n255\n", image.size[0], image.size[1]);
                        for pixel in &image.pixels {
                            let _ = file.write_all(&[pixel.r(), pixel.g(), pixel.b()]);
                        }
                    }
                    callback_ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                });
            }
        }
    }
}
