#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod kit;
mod media;
mod notes;
mod models;
mod storage;
mod system;
mod theme;
mod timer_logic;
mod voice;
mod volume;

use eframe::egui;

/// Падение программы: окно с текстом ошибки и отчёт crash.log рядом с данными.
/// Без этого exe без консоли просто молча исчезал бы — и причину было не узнать.
fn install_crash_report() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let dir = storage::get_default_path();
        let path = dir.join("crash.log");
        let report = format!(
            "Timer Pro {} · {}\n{info}\n\n{}",
            env!("CARGO_PKG_VERSION"),
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
            std::backtrace::Backtrace::force_capture()
        );
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(&path, &report);
        #[cfg(windows)]
        {
            use windows::core::HSTRING;
            use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};
            let text = HSTRING::from(format!(
                "Программа столкнулась с ошибкой и закроется.\n\n{info}\n\nОтчёт сохранён:\n{}\nПришлите его разработчику.",
                path.display()
            ));
            unsafe { MessageBoxW(None, &text, &HSTRING::from("Timer Pro — ошибка"), MB_OK | MB_ICONERROR) };
        }
        default_hook(info);
    }));
}

fn main() -> eframe::Result<()> {
    install_crash_report();
    // Идентификатор приложения для уведомлений Windows — до создания окон.
    system::register_app_identity();

    // Служебная проверка уведомлений: `timer_pro.exe --test-toast`.
    if std::env::args().any(|a| a == "--test-toast") {
        match system::notify_blocking("Время вышло", "Проверка уведомления · Timer Pro") {
            Ok(()) => eprintln!("toast: ok"),
            Err(e) => eprintln!("toast: {e}"),
        }
        return Ok(());
    }

    // Служебная проверка распознавания: `timer_pro.exe --voice-test <папка с wav>`.
    let args: Vec<String> = std::env::args().collect();
    if let Some(i) = args.iter().position(|a| a == "--voice-test") {
        let dir = args.get(i + 1).map(std::path::PathBuf::from).unwrap_or_else(|| ".".into());
        if let Err(e) = voice::run_file_test(&dir) {
            eprintln!("voice: {e}");
        }
        return Ok(());
    }
    // Программы в микшере громкости (только чтение): `timer_pro.exe --volume-list`.
    #[cfg(windows)]
    if args.iter().any(|a| a == "--volume-list") {
        match volume::list() {
            Ok(items) => {
                for (exe, level, muted, peak, active) in items {
                    println!("{exe:<28} громкость {:>3}%{}  пик {peak:.2}{}", (level * 100.0).round(), if muted { " (без звука)" } else { "" }, if active { "  играет" } else { "" });
                }
            }
            Err(e) => eprintln!("volume: {e}"),
        }
        return Ok(());
    }
    // Проверка живого микрофона: `timer_pro.exe --voice-listen 20`.
    if let Some(i) = args.iter().position(|a| a == "--voice-listen") {
        let secs = args.get(i + 1).and_then(|s| s.parse().ok()).unwrap_or(20);
        voice::run_listen_test(secs, args.get(i + 2).cloned());
        return Ok(());
    }

    let icon = eframe::icon_data::from_png_bytes(include_bytes!("../resources/timer.png"))
        .unwrap_or_else(|_| egui::IconData::default());

    // Конфиг читаем до создания окна: в нём лежат сохранённая позиция
    // виджета и флаг "поверх всех окон".
    let config = storage::load_config();

    // Окно чуть больше виджета: прозрачные поля нужны под тень блоков.
    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([
            theme::WINDOW_W + 2.0 * theme::PAD_X,
            theme::PAD_TOP + theme::WIDGET_H + theme::PAD_BOTTOM,
        ])
        .with_transparent(true)
        .with_decorations(false)
        // Размер окна задаёт сама программа. Растягиваемое окно без рамки получало
        // невидимую зону ресайза по краям: его можно было случайно растянуть мышью,
        // а позиция при каждом запуске уползала на пару пикселей.
        .with_resizable(false)
        .with_icon(icon);

    if config.always_on_top {
        viewport = viewport.with_always_on_top();
    }
    // Позицию здесь не задаём: winit пересчитал бы её через масштаб монитора,
    // на котором окно только создаётся, и на мониторах с разным масштабом
    // окно уезжало за край экрана. Окно ставится на место в первом кадре — см. app.rs.

    let native_options = eframe::NativeOptions {
        viewport,
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    };

    eframe::run_native(
        "Timer Pro",
        native_options,
        Box::new(move |cc| Ok(Box::new(app::TimerApp::new(cc, config)))),
    )
}
