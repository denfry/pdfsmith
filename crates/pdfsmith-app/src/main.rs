//! pdfsmith — просмотр, редактирование и конвертация PDF.
//!
//! Тайловый рендер в фоновом потоке PDFium, композиция на GPU (wgpu: DX12/Vulkan,
//! без видеокарты — программный OpenGL Mesa llvmpipe), непрерывная прокрутка всех страниц.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;

use eframe::egui;

mod app;
mod default_app;
mod print;
mod print_ui;
mod render_thread;
mod settings;
mod settings_ui;
mod theme;
mod updates;

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

    let Some(wgpu_setup) = build_wgpu_setup() else {
        let _ = rfd::MessageDialog::new()
            .set_level(rfd::MessageLevel::Error)
            .set_title("pdfsmith — нет графики")
            .set_description(
                "Не удалось запустить отрисовку окна: не найден ни видеоадаптер (DirectX 12 / Vulkan), \
                 ни программный рендер.\n\nПереустановите PDFsmith (в папке программы должны быть \
                 opengl32.dll и libgallium_wgl.dll) или установите драйвер видеокарты.",
            )
            .show();
        return;
    };

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 840.0])
            .with_min_inner_size([720.0, 480.0])
            .with_title("pdfsmith"),
        wgpu_options: eframe::egui_wgpu::WgpuConfiguration {
            wgpu_setup,
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

/// Выбор графического адаптера, по убыванию предпочтения:
/// 1. аппаратный DX12/Vulkan;
/// 2. любой адаптер DX12 (в т.ч. программный WARP, если Windows его отдаёт);
/// 3. программный OpenGL Mesa llvmpipe — `opengl32.dll` + `libgallium_wgl.dll`
///    кладутся рядом с exe, работает без видеокарты и драйверов.
///
/// `PDFSMITH_FORCE_WARP=1` сразу выбирает программный рендер (п. 3).
/// `None` — не нашлось ничего, вызывающий показывает понятную ошибку.
fn build_wgpu_setup() -> Option<eframe::egui_wgpu::WgpuSetup> {
    use eframe::wgpu;
    use std::sync::Arc;

    let force_software = std::env::var_os("PDFSMITH_FORCE_WARP").is_some();

    let hardware = || {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::DX12 | wgpu::Backends::VULKAN,
            ..Default::default()
        });
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
        }))
        .or_else(|| instance.enumerate_adapters(wgpu::Backends::DX12).into_iter().next())?;
        Some((instance, adapter))
    };

    let software = || {
        // Mesa: выбрать CPU-растеризатор, а не свой драйвер «GL поверх D3D12».
        // Переменная читается Mesa при создании первого GL-контекста.
        std::env::set_var("GALLIUM_DRIVER", "llvmpipe");
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::GL,
            ..Default::default()
        });
        let adapter = instance.enumerate_adapters(wgpu::Backends::GL).into_iter().next()?;
        Some((instance, adapter))
    };

    let (instance, adapter) = if force_software { software() } else { hardware().or_else(software) }?;
    log::info!("wgpu адаптер: {:?}", adapter.get_info());

    let (device, queue) = pollster::block_on(adapter.request_device(
        &wgpu::DeviceDescriptor {
            label: Some("pdfsmith"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::downlevel_defaults().using_resolution(adapter.limits()),
            memory_hints: wgpu::MemoryHints::default(),
        },
        None,
    ))
    .map_err(|e| log::error!("не удалось создать графическое устройство: {e}"))
    .ok()?;

    Some(eframe::egui_wgpu::WgpuSetup::Existing {
        instance: Arc::new(instance),
        adapter: Arc::new(adapter),
        device: Arc::new(device),
        queue: Arc::new(queue),
    })
}
