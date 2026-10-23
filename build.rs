use std::path::{Path, PathBuf};

// Шрифты из макета (Inter и JetBrains Mono) встраиваются в exe, если лежат
// в resources/fonts. Без них приложение собирается со встроенными шрифтами egui.
const BRAND_FONTS: [&str; 4] = [
    "resources/fonts/Inter-Regular.ttf",
    "resources/fonts/Inter-Medium.ttf",
    "resources/fonts/JetBrainsMono-Regular.ttf",
    "resources/fonts/JetBrainsMono-Medium.ttf",
];

fn main() {
    println!("cargo::rustc-check-cfg=cfg(has_brand_fonts)");
    println!("cargo:rerun-if-changed=resources/fonts");
    for font in BRAND_FONTS {
        println!("cargo:rerun-if-changed={font}");
    }
    if BRAND_FONTS.iter().all(|f| Path::new(f).exists()) {
        println!("cargo:rustc-cfg=has_brand_fonts");
    }

    // Голосовая модель и libvosk.dll: в релизе кладём их рядом с exe (target/release/vosk),
    // чтобы папку с программой можно было перенести целиком. В отладке программа
    // берёт их прямо из resources/vosk.
    println!("cargo:rerun-if-changed=resources/vosk");
    if std::env::var("PROFILE").as_deref() == Ok("release") && Path::new("resources/vosk").is_dir() {
        if let Some(target_dir) = std::env::var("OUT_DIR").ok().and_then(|o| PathBuf::from(o).ancestors().nth(3).map(Path::to_path_buf)) {
            copy_dir_if_changed(Path::new("resources/vosk"), &target_dir.join("vosk"));
        }
    }

    if std::env::var("CARGO_CFG_TARGET_OS").unwrap() == "windows" {
        // 1. Создаем экземпляр ресурсов (этой строки у вас не хватало)
        let mut res = winres::WindowsResource::new();

        // 2. Указываем путь к иконке
        res.set_icon("resources/timer.ico");

        // 3. Компилируем
        res.compile().unwrap();
    }
}

/// Копирует папку, пропуская файлы того же размера (модель весит ~140 МБ).
fn copy_dir_if_changed(from: &Path, to: &Path) {
    let Ok(entries) = std::fs::read_dir(from) else { return };
    let _ = std::fs::create_dir_all(to);
    for entry in entries.flatten() {
        let src = entry.path();
        let dst = to.join(entry.file_name());
        if src.is_dir() {
            copy_dir_if_changed(&src, &dst);
        } else {
            let same = matches!((src.metadata(), dst.metadata()), (Ok(a), Ok(b)) if a.len() == b.len());
            if !same {
                let _ = std::fs::copy(&src, &dst);
            }
        }
    }
}
