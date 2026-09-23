//! pdfsmith — просмотр, редактирование и конвертация PDF.
//!
//! Тайловый рендер в фоновом потоке PDFium, композиция на GPU (wgpu с
//! программным WARP-фоллбэком), непрерывная прокрутка всех страниц.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;

use eframe::egui;

mod app;
mod render_thread;
mod settings;
mod theme;

fn main() {
    env_logger::init();

    // В release нет консоли: показываем панику нативным окном.
    std::panic::set_hook(Box::new(|info| {
        let _ = rfd::MessageDialog::new()
            .set_level(rfd::MessageLevel::Error)
            .set_title("pdfsmith — критическая ошибка")
            .set_description(info.to_string())
            .show();
    }));

    let path = std::env::args_os().nth(1).map(PathBuf::from);

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 840.0])
            .with_min_inner_size([720.0, 480.0])
            .with_title("pdfsmith"),
        wgpu_options: eframe::egui_wgpu::WgpuConfiguration {
            wgpu_setup: build_wgpu_setup(),
            ..Default::default()
        },
        ..Default::default()
    };

    let result = eframe::run_native(
        "pdfsmith",
        options,
        Box::new(move |cc| {
            theme::install(&cc.egui_ctx);
            Ok(Box::new(app::ViewerApp::new(cc, path)))
        }),
    );

    if let Err(e) = result {
        let _ = rfd::MessageDialog::new()
            .set_level(rfd::MessageLevel::Error)
            .set_title("pdfsmith — не удалось запустить окно")
            .set_description(format!(
                "{e}\n\nВозможные причины: нет графического адаптера (драйверы видеокарты, \
                 удалённый рабочий стол или виртуальная машина)."
            ))
            .show();
    }
}

/// Создаёт wgpu-устройство: сначала аппаратное (DX12/Vulkan), при отказе —
/// программный WARP (встроен в Windows, не требует драйверов GPU).
fn build_wgpu_setup() -> eframe::egui_wgpu::WgpuSetup {
    use eframe::wgpu;
    use std::sync::Arc;

    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::DX12 | wgpu::Backends::VULKAN | wgpu::Backends::GL,
        ..Default::default()
    });

    // PDFSMITH_FORCE_WARP=1 принудительно выбирает программный рендер.
    let force_warp = std::env::var("PDFSMITH_FORCE_WARP").is_ok();
    let adapter = pollster::block_on(async {
        if !force_warp {
            if let Some(a) = instance
                .request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::HighPerformance,
                    compatible_surface: None,
                    force_fallback_adapter: false,
                })
                .await
            {
                return Some(a);
            }
        }
        instance
            .enumerate_adapters(wgpu::Backends::all())
            .into_iter()
            .min_by_key(|a| match a.get_info().device_type {
                wgpu::DeviceType::Cpu => 0u8,
                _ => 1u8,
            })
    })
    .expect("не найден ни один графический адаптер (DX12/Vulkan/WARP)");

    log::info!("wgpu адаптер: {:?}", adapter.get_info());

    let (device, queue) = pollster::block_on(adapter.request_device(
        &wgpu::DeviceDescriptor {
            label: Some("pdfsmith"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            memory_hints: wgpu::MemoryHints::default(),
        },
        None,
    ))
    .expect("не удалось создать графическое устройство");

    eframe::egui_wgpu::WgpuSetup::Existing {
        instance: Arc::new(instance),
        adapter: Arc::new(adapter),
        device: Arc::new(device),
        queue: Arc::new(queue),
    }
}
