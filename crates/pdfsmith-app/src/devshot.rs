//! Снимок окна для проверки интерфейса без ручного ввода.
//!
//! `PDFSMITH_SHOT=путь.png` — через `PDFSMITH_SHOT_DELAY` секунд (по умолчанию 5)
//! сохранить кадр в PNG и закрыть окно. Состояние перед снимком:
//! `PDFSMITH_SHOT_TOOL=pen|marker|select`, `PDFSMITH_SHOT_SEARCH=слово`,
//! `PDFSMITH_SHOT_WINDOW=convert|help|settings`. Без переменных модуль ничего не делает.

use std::path::PathBuf;
use std::time::Instant;

use eframe::egui;

pub struct DevShot {
    path: PathBuf,
    started: Instant,
    delay: f32,
    requested: bool,
    pub tool: Option<String>,
    pub search: Option<String>,
    pub window: Option<String>,
    /// `PDFSMITH_SHOT_TEXT`: с инструментом «Правка» заменить первую строку этим текстом.
    pub text: Option<String>,
}

impl DevShot {
    pub fn from_env() -> Option<DevShot> {
        let path = PathBuf::from(std::env::var_os("PDFSMITH_SHOT")?);
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        Some(DevShot {
            path,
            started: Instant::now(),
            delay: var("PDFSMITH_SHOT_DELAY")
                .and_then(|v| v.parse().ok())
                .unwrap_or(5.0),
            requested: false,
            tool: var("PDFSMITH_SHOT_TOOL"),
            search: var("PDFSMITH_SHOT_SEARCH"),
            window: var("PDFSMITH_SHOT_WINDOW"),
            text: var("PDFSMITH_SHOT_TEXT"),
        })
    }

    /// Вызывается каждый кадр: запрашивает снимок, сохраняет и закрывает окно.
    pub fn tick(&mut self, ctx: &egui::Context) {
        let shot = ctx.input(|i| {
            i.raw.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(img) = shot {
            let [w, h] = img.size;
            let rgba: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_array()).collect();
            if let Err(e) = image::save_buffer(
                &self.path,
                &rgba,
                w as u32,
                h as u32,
                image::ExtendedColorType::Rgba8,
            ) {
                log::error!("снимок {}: {e}", self.path.display());
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        if !self.requested && self.started.elapsed().as_secs_f32() >= self.delay {
            self.requested = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(200));
    }
}
