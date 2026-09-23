//! Сборочный скрипт: под Windows вшивает иконку приложения в exe.
//!
//! Вшивание «мягкое»: если в системе нет `rc.exe`/`llvm-rc` (Windows SDK),
//! сборка не падает — установщик соберётся, просто без встроенной в exe иконки
//! (иконка всё равно ставится файлом и используется для ассоциации .pdf).

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rerun-if-changed=assets/pdfsmith.ico");
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/pdfsmith.ico");
        if let Err(e) = res.compile() {
            println!(
                "cargo:warning=не удалось вшить иконку в exe ({e}); \
                 собираю без встроенной иконки"
            );
        }
    }
}
