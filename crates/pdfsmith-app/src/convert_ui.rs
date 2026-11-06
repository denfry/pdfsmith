//! Окно «Конвертер»: операции списком слева, файлы и параметры справа.

use std::path::{Path, PathBuf};

use eframe::egui::{self, RichText, Stroke, Vec2};
use egui_phosphor::regular as ph;

use crate::office::{self, OfficeApp, OFFICE_EXTS};
use crate::render_thread::{ConvertOp, ImageFormat};
use crate::theme;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ConvertMode {
    PdfToImages,
    PdfToText,
    ImagesToPdf,
    TextToPdf,
    Merge,
    Split,
    OfficeToPdf,
    PdfToWord,
    PdfToExcel,
    PdfToPowerPoint,
}

impl ConvertMode {
    /// Операции группами — как в списке окна.
    pub const GROUPS: [(&'static str, &'static [ConvertMode]); 3] = [
        (
            "СОБРАТЬ И РАЗДЕЛИТЬ",
            &[ConvertMode::Merge, ConvertMode::Split],
        ),
        (
            "СОЗДАТЬ PDF",
            &[
                ConvertMode::OfficeToPdf,
                ConvertMode::ImagesToPdf,
                ConvertMode::TextToPdf,
            ],
        ),
        (
            "ЭКСПОРТ ИЗ PDF",
            &[
                ConvertMode::PdfToWord,
                ConvertMode::PdfToExcel,
                ConvertMode::PdfToPowerPoint,
                ConvertMode::PdfToImages,
                ConvertMode::PdfToText,
            ],
        ),
    ];

    pub fn label(self) -> &'static str {
        match self {
            ConvertMode::PdfToImages => "PDF → картинки",
            ConvertMode::PdfToText => "PDF → текст",
            ConvertMode::ImagesToPdf => "Картинки → PDF",
            ConvertMode::TextToPdf => "Текст → PDF",
            ConvertMode::Merge => "Объединить файлы",
            ConvertMode::Split => "Разделить PDF",
            ConvertMode::OfficeToPdf => "Word, Excel → PDF",
            ConvertMode::PdfToWord => "PDF → Word",
            ConvertMode::PdfToExcel => "PDF → Excel",
            ConvertMode::PdfToPowerPoint => "PDF → PowerPoint",
        }
    }

    pub fn icon(self) -> &'static str {
        match self {
            ConvertMode::PdfToImages => ph::IMAGE,
            ConvertMode::PdfToText => ph::TEXT_T,
            ConvertMode::ImagesToPdf => ph::IMAGES,
            ConvertMode::TextToPdf => ph::FILE_TEXT,
            ConvertMode::Merge => ph::STACK,
            ConvertMode::Split => ph::SCISSORS,
            ConvertMode::OfficeToPdf => ph::FILE_PDF,
            ConvertMode::PdfToWord => ph::MICROSOFT_WORD_LOGO,
            ConvertMode::PdfToExcel => ph::MICROSOFT_EXCEL_LOGO,
            ConvertMode::PdfToPowerPoint => ph::MICROSOFT_POWERPOINT_LOGO,
        }
    }

    fn description(self) -> &'static str {
        match self {
            ConvertMode::PdfToImages => "Каждая страница — отдельный файл PNG или JPEG.",
            ConvertMode::PdfToText => "Весь текст документа — в один файл .txt.",
            ConvertMode::ImagesToPdf => "Картинки по порядку — в один PDF, страница на картинку.",
            ConvertMode::TextToPdf => "Текстовый файл — в PDF (кириллица поддерживается).",
            ConvertMode::Merge => {
                "PDF, документы Word, таблицы Excel и презентации — в один PDF, в порядке списка."
            }
            ConvertMode::Split => "Каждая страница — в отдельный PDF.",
            ConvertMode::OfficeToPdf => {
                "Документы Word, таблицы Excel, презентации PowerPoint — в PDF с той же вёрсткой."
            }
            ConvertMode::PdfToWord => {
                "Редактируемый документ .docx: текст, абзацы, таблицы и картинки."
            }
            ConvertMode::PdfToExcel => "Таблицы и текст — по ячейкам книги .xlsx, числа остаются числами. Office не нужен.",
            ConvertMode::PdfToPowerPoint => {
                "Каждая страница — слайд .pptx (страница вставляется картинкой)."
            }
        }
    }

    /// Какие программы Office нужны операции.
    fn needs(self) -> &'static [OfficeApp] {
        match self {
            ConvertMode::OfficeToPdf => &[],
            ConvertMode::PdfToWord => &[OfficeApp::Word],
            ConvertMode::PdfToPowerPoint => &[OfficeApp::PowerPoint],
            _ => &[],
        }
    }

    pub fn multi_input(self) -> bool {
        matches!(
            self,
            ConvertMode::ImagesToPdf | ConvertMode::Merge | ConvertMode::OfficeToPdf
        )
    }

    fn input_filter(self) -> (&'static str, &'static [&'static str]) {
        match self {
            ConvertMode::ImagesToPdf => ("Изображения", IMAGE_EXTS),
            ConvertMode::TextToPdf => ("Текст", &["txt", "md", "log", "csv"]),
            ConvertMode::OfficeToPdf => ("Документы Office", OFFICE_EXTS),
            ConvertMode::Merge => ("PDF и документы Office", MERGE_EXTS),
            _ => ("PDF", &["pdf"]),
        }
    }
}

/// Что принимает «Объединить»: PDF и всё, что Office умеет сохранить в PDF.
const MERGE_EXTS: &[&str] = &[
    "pdf", "doc", "docx", "docm", "dotx", "rtf", "odt", "xls", "xlsx", "xlsm", "xlsb", "ods",
    "csv", "ppt", "pptx", "pptm", "pps", "ppsx", "odp",
];

pub const IMAGE_EXTS: &[&str] = &["png", "jpg", "jpeg", "bmp", "gif", "tif", "tiff", "webp"];

pub fn has_ext(p: &Path, exts: &[&str]) -> bool {
    p.extension()
        .map(|e| exts.iter().any(|x| e.eq_ignore_ascii_case(x)))
        .unwrap_or(false)
}

pub struct ConvertDialog {
    pub open: bool,
    pub mode: ConvertMode,
    pub inputs: Vec<PathBuf>,
    pub output: Option<PathBuf>,
    jpeg: bool,
    dpi: f32,
}

impl Default for ConvertDialog {
    fn default() -> Self {
        ConvertDialog {
            open: false,
            mode: ConvertMode::Merge,
            inputs: Vec::new(),
            output: None,
            jpeg: false,
            dpi: 150.0,
        }
    }
}

impl ConvertDialog {
    /// Результат — папка (несколько файлов на выходе) или один файл.
    fn output_is_dir(&self) -> bool {
        match self.mode {
            ConvertMode::PdfToImages | ConvertMode::Split => true,
            ConvertMode::OfficeToPdf => self.inputs.len() > 1,
            _ => false,
        }
    }

    /// Программы Office, которых не хватает для текущей операции.
    fn missing_office(&self) -> Vec<&'static str> {
        let mut need: Vec<OfficeApp> = self.mode.needs().to_vec();
        if matches!(self.mode, ConvertMode::OfficeToPdf | ConvertMode::Merge) {
            need.extend(self.inputs.iter().filter_map(|p| office::app_for(p)));
        }
        need.sort_by_key(|a| *a as u8);
        need.dedup();
        need.into_iter()
            .filter(|a| !office::available(*a))
            .map(|a| a.name())
            .collect()
    }

    /// Открыть окно на операции `mode` с готовым списком файлов.
    pub fn open_with(&mut self, mode: ConvertMode, inputs: Vec<PathBuf>) {
        self.open = true;
        if self.mode != mode {
            self.output = None;
        }
        self.mode = mode;
        let (_, exts) = mode.input_filter();
        self.inputs = inputs.into_iter().filter(|p| has_ext(p, exts)).collect();
    }

    /// Файлы, брошенные в окно, пока открыт конвертер. `false` — ни один не подошёл.
    pub fn add_dropped(&mut self, paths: Vec<PathBuf>) -> bool {
        let (_, exts) = self.mode.input_filter();
        let ok: Vec<PathBuf> = paths.into_iter().filter(|p| has_ext(p, exts)).collect();
        if ok.is_empty() {
            return false;
        }
        if self.mode.multi_input() {
            for p in ok {
                if !self.inputs.contains(&p) {
                    self.inputs.push(p);
                }
            }
        } else {
            self.inputs = vec![ok[0].clone()];
        }
        true
    }

    /// Системный диалог выбора входных файлов.
    pub fn pick_inputs(&mut self) {
        let (name, exts) = self.mode.input_filter();
        let d = rfd::FileDialog::new().add_filter(name, exts);
        if self.mode.multi_input() {
            if let Some(p) = d.pick_files() {
                self.add_dropped(p);
            }
        } else if let Some(p) = d.pick_file() {
            self.inputs = vec![p];
        }
    }

    fn choose_output(&mut self) -> bool {
        let mut d = rfd::FileDialog::new();
        if let Some(first) = self.inputs.first() {
            if let Some(dir) = first.parent() {
                d = d.set_directory(dir);
            }
            let stem = first
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or("out".into());
            d = match self.mode {
                ConvertMode::PdfToText => d
                    .add_filter("Текст", &["txt"])
                    .set_file_name(format!("{stem}.txt")),
                ConvertMode::Merge => d
                    .add_filter("PDF", &["pdf"])
                    .set_file_name("объединённый.pdf".to_string()),
                ConvertMode::PdfToWord => d
                    .add_filter("Документ Word", &["docx"])
                    .set_file_name(format!("{stem}.docx")),
                ConvertMode::PdfToExcel => d
                    .add_filter("Книга Excel", &["xlsx"])
                    .set_file_name(format!("{stem}.xlsx")),
                ConvertMode::PdfToPowerPoint => d
                    .add_filter("Презентация PowerPoint", &["pptx"])
                    .set_file_name(format!("{stem}.pptx")),
                _ => d
                    .add_filter("PDF", &["pdf"])
                    .set_file_name(format!("{stem}.pdf")),
            };
        }
        let out = if self.output_is_dir() {
            d.pick_folder()
        } else {
            d.save_file()
        };
        if out.is_some() {
            self.output = out;
        }
        self.output.is_some()
    }

    fn op(&self) -> Option<ConvertOp> {
        let out = self.output.clone()?;
        let src = self.inputs.first()?.clone();
        Some(match self.mode {
            ConvertMode::PdfToImages => ConvertOp::PdfToImages {
                src,
                out_dir: out,
                format: if self.jpeg {
                    ImageFormat::Jpeg
                } else {
                    ImageFormat::Png
                },
                dpi: self.dpi,
            },
            ConvertMode::PdfToText => ConvertOp::PdfToText { src, out },
            ConvertMode::ImagesToPdf => ConvertOp::ImagesToPdf {
                srcs: self.inputs.clone(),
                out,
            },
            ConvertMode::TextToPdf => ConvertOp::TextToPdf { src, out },
            ConvertMode::Merge => ConvertOp::Merge {
                srcs: self.inputs.clone(),
                out,
            },
            ConvertMode::Split => ConvertOp::Split { src, out_dir: out },
            ConvertMode::OfficeToPdf => ConvertOp::OfficeToPdf {
                srcs: self.inputs.clone(),
                out,
            },
            ConvertMode::PdfToWord => ConvertOp::PdfToWord { src, out },
            ConvertMode::PdfToExcel => ConvertOp::PdfToExcel { src, out },
            ConvertMode::PdfToPowerPoint => ConvertOp::PdfToPowerPoint { src, out },
        })
    }

    /// Рисует окно; возвращает операцию, если нажали «Запустить».
    pub fn ui(
        &mut self,
        ctx: &egui::Context,
        progress: Option<&(String, f32)>,
    ) -> Option<ConvertOp> {
        if !self.open {
            return None;
        }
        let mut open = true;
        let mut run = false;
        egui::Window::new("Конвертер")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ctx, |ui| {
                ui.horizontal_top(|ui| {
                    self.mode_list(ui);
                    ui.add_space(4.0);
                    let (rect, _) =
                        ui.allocate_exact_size(Vec2::new(1.0, 440.0), egui::Sense::hover());
                    ui.painter().vline(
                        rect.center().x,
                        rect.y_range(),
                        Stroke::new(1.0, theme::line()),
                    );
                    ui.add_space(10.0);
                    ui.vertical(|ui| {
                        ui.set_width(400.0);
                        run = self.details(ui, progress);
                    });
                });
            });
        self.open = open;
        if !run {
            return None;
        }
        if self.output.is_none() && !self.choose_output() {
            return None;
        }
        self.op()
    }

    fn mode_list(&mut self, ui: &mut egui::Ui) {
        ui.vertical(|ui| {
            ui.set_width(186.0);
            ui.spacing_mut().item_spacing.y = 2.0;
            for (gi, (title, modes)) in ConvertMode::GROUPS.iter().enumerate() {
                if gi > 0 {
                    ui.add_space(8.0);
                }
                ui.horizontal(|ui| {
                    ui.add_space(8.0);
                    theme::section_label(ui, title);
                });
                for &m in modes.iter() {
                    let active = self.mode == m;
                    let (rect, resp) =
                        ui.allocate_exact_size(Vec2::new(186.0, 32.0), egui::Sense::click());
                    let p = ui.painter();
                    if active {
                        p.rect_filled(rect, 7.0, theme::accent_dim());
                    } else if resp.hovered() {
                        p.rect_filled(rect, 7.0, theme::hover_wash());
                    }
                    let color = if active {
                        theme::accent()
                    } else {
                        theme::text()
                    };
                    p.text(
                        rect.left_center() + Vec2::new(16.0, 0.0),
                        egui::Align2::CENTER_CENTER,
                        m.icon(),
                        egui::FontId::proportional(16.0),
                        color,
                    );
                    p.text(
                        rect.left_center() + Vec2::new(32.0, 0.0),
                        egui::Align2::LEFT_CENTER,
                        m.label(),
                        egui::FontId::proportional(13.5),
                        if active {
                            theme::text()
                        } else {
                            theme::muted()
                        },
                    );
                    if resp.clicked() && !active {
                        self.mode = m;
                        let (_, exts) = m.input_filter();
                        self.inputs.retain(|p| has_ext(p, exts));
                        if !m.multi_input() {
                            self.inputs.truncate(1);
                        }
                        self.output = None;
                    }
                }
            }
        });
    }

    /// Правая часть окна; `true` — нажали «Запустить».
    fn details(&mut self, ui: &mut egui::Ui, progress: Option<&(String, f32)>) -> bool {
        ui.label(RichText::new(self.mode.label()).size(16.0).strong());
        ui.label(RichText::new(self.mode.description()).color(theme::muted()));
        let missing = self.missing_office();
        if !missing.is_empty() {
            ui.add_space(4.0);
            ui.label(
                RichText::new(format!("{}  Не найден Microsoft {} — эта операция работает через установленный Office.", ph::WARNING, missing.join(", ")))
                    .color(theme::danger())
                    .small(),
            );
        } else if !self.mode.needs().is_empty() || self.mode == ConvertMode::OfficeToPdf {
            ui.label(
                RichText::new(
                    "Через установленный Microsoft Office. Первый запуск может занять до минуты.",
                )
                .small()
                .color(theme::muted()),
            );
        }
        ui.add_space(8.0);

        // Входные файлы.
        ui.horizontal(|ui| {
            theme::section_label(
                ui,
                if self.mode.multi_input() {
                    "ФАЙЛЫ"
                } else {
                    "ФАЙЛ"
                },
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if self.mode.multi_input()
                    && !self.inputs.is_empty()
                    && ui.small_button("Очистить").clicked()
                {
                    self.inputs.clear();
                }
                let label = if self.mode.multi_input() {
                    "Добавить…"
                } else {
                    "Выбрать…"
                };
                if ui
                    .small_button(format!("{}  {label}", ph::FOLDER_OPEN))
                    .clicked()
                {
                    self.pick_inputs();
                }
            });
        });
        egui::Frame::none()
            .fill(theme::bg())
            .stroke(Stroke::new(1.0, theme::line()))
            .rounding(6.0)
            .inner_margin(8.0)
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.set_min_height(96.0);
                if self.inputs.is_empty() {
                    ui.vertical_centered(|ui| {
                        ui.add_space(14.0);
                        ui.label(
                            RichText::new(ph::FILE_ARROW_UP)
                                .size(24.0)
                                .color(theme::muted()),
                        );
                        ui.label(
                            RichText::new("Перетащите файлы сюда или нажмите «Выбрать…»")
                                .color(theme::muted())
                                .small(),
                        );
                    });
                    return;
                }
                egui::ScrollArea::vertical()
                    .max_height(132.0)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        let mut action: Option<(usize, i32)> = None;
                        let n = self.inputs.len();
                        for (i, p) in self.inputs.iter().enumerate() {
                            ui.horizontal(|ui| {
                                if self.mode.multi_input() {
                                    ui.label(
                                        RichText::new(format!("{}.", i + 1))
                                            .color(theme::muted())
                                            .small()
                                            .monospace(),
                                    );
                                }
                                let name = p
                                    .file_name()
                                    .map(|s| s.to_string_lossy().to_string())
                                    .unwrap_or_default();
                                ui.add(egui::Label::new(RichText::new(name)).truncate())
                                    .on_hover_text(p.display().to_string());
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if ui
                                            .small_button(ph::X)
                                            .on_hover_text("Убрать из списка")
                                            .clicked()
                                        {
                                            action = Some((i, 0));
                                        }
                                        if self.mode.multi_input() {
                                            if i + 1 < n
                                                && ui
                                                    .small_button(ph::ARROW_DOWN)
                                                    .on_hover_text("Ниже")
                                                    .clicked()
                                            {
                                                action = Some((i, 1));
                                            }
                                            if i > 0
                                                && ui
                                                    .small_button(ph::ARROW_UP)
                                                    .on_hover_text("Выше")
                                                    .clicked()
                                            {
                                                action = Some((i, -1));
                                            }
                                        }
                                    },
                                );
                            });
                        }
                        match action {
                            Some((i, 0)) => {
                                self.inputs.remove(i);
                            }
                            Some((i, 1)) => self.inputs.swap(i, i + 1),
                            Some((i, _)) => self.inputs.swap(i, i - 1),
                            None => {}
                        }
                    });
            });
        ui.add_space(8.0);

        // Параметры.
        if self.mode == ConvertMode::PdfToImages {
            theme::section_label(ui, "ПАРАМЕТРЫ");
            ui.horizontal(|ui| {
                ui.label("Формат");
                theme::pill_frame().show(ui, |ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    ui.selectable_value(&mut self.jpeg, false, "PNG");
                    ui.selectable_value(&mut self.jpeg, true, "JPEG");
                });
                ui.add_space(10.0);
                ui.label("Качество");
                theme::pill_frame().show(ui, |ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    for (d, tip) in [
                        (72.0, "экран"),
                        (150.0, "обычное"),
                        (300.0, "печать"),
                        (600.0, "максимум"),
                    ] {
                        ui.selectable_value(&mut self.dpi, d, format!("{d:.0}"))
                            .on_hover_text(format!("{d:.0} DPI — {tip}"));
                    }
                });
            });
            ui.add_space(8.0);
        }

        // Куда сохранить.
        theme::section_label(
            ui,
            if self.output_is_dir() {
                "ПАПКА ДЛЯ РЕЗУЛЬТАТА"
            } else {
                "СОХРАНИТЬ В"
            },
        );
        ui.horizontal(|ui| {
            match &self.output {
                Some(p) => {
                    ui.add(
                        egui::Label::new(RichText::new(p.display().to_string()).small()).truncate(),
                    )
                    .on_hover_text(p.display().to_string());
                }
                None => {
                    ui.label(
                        RichText::new("спросим при запуске")
                            .small()
                            .color(theme::muted()),
                    );
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add_enabled(
                        !self.inputs.is_empty(),
                        egui::Button::new("Изменить…").small(),
                    )
                    .clicked()
                {
                    self.choose_output();
                }
            });
        });
        ui.add_space(14.0);

        let mut run = false;
        ui.horizontal(|ui| {
            if let Some((msg, frac)) = progress {
                ui.add(
                    egui::ProgressBar::new(*frac)
                        .desired_width(180.0)
                        .desired_height(8.0),
                );
                ui.label(RichText::new(msg).small().color(theme::muted()));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let ready = !self.inputs.is_empty() && progress.is_none() && missing.is_empty();
                let btn = theme::primary_button(format!("{}  Запустить", ph::PLAY), ready)
                    .min_size(Vec2::new(132.0, 30.0));
                if ui.add_enabled(ready, btn).clicked() {
                    run = true;
                }
            });
        });
        run
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dropped_files_are_filtered_by_mode() {
        let mut d = ConvertDialog::default();
        d.open_with(ConvertMode::Merge, vec![PathBuf::from("a.pdf")]);
        assert!(d.add_dropped(vec![
            PathBuf::from("b.PDF"),
            PathBuf::from("c.png"),
            PathBuf::from("a.pdf"),
            PathBuf::from("d.docx")
        ]));
        assert_eq!(
            d.inputs,
            vec![
                PathBuf::from("a.pdf"),
                PathBuf::from("b.PDF"),
                PathBuf::from("d.docx")
            ],
            "объединение берёт PDF и Office"
        );
        assert!(!d.add_dropped(vec![PathBuf::from("x.txt")]));

        d.open_with(ConvertMode::OfficeToPdf, vec![PathBuf::from("a.docx")]);
        assert!(!d.output_is_dir(), "один документ — один PDF");
        d.add_dropped(vec![PathBuf::from("b.xlsx")]);
        assert!(d.output_is_dir(), "несколько документов — в папку");

        d.open_with(ConvertMode::Split, vec![]);
        assert!(d.add_dropped(vec![PathBuf::from("one.pdf"), PathBuf::from("two.pdf")]));
        assert_eq!(
            d.inputs,
            vec![PathBuf::from("one.pdf")],
            "одиночный режим берёт один файл"
        );
    }
}
