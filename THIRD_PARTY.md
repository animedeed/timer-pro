# Сторонние компоненты

Timer Pro распространяется по лицензии MIT (см. `LICENSE`). В программу и установщик входят:

| Компонент | Что это | Лицензия |
|---|---|---|
| [Vosk API](https://github.com/alphacep/vosk-api) 0.3.45 (`libvosk.dll` и её зависимости) | Офлайн-распознавание речи | Apache 2.0 |
| [vosk-model-small-ru-0.22](https://alphacephei.com/vosk/models) | Русская модель для Vosk | Apache 2.0 |
| [Inter](https://rsms.me/inter/) | Шрифт интерфейса (вшит в exe) | SIL Open Font License 1.1 |
| [JetBrains Mono](https://www.jetbrains.com/lp/mono/) | Шрифт цифр (вшит в exe) | SIL Open Font License 1.1 |
| [Phosphor Icons](https://phosphoricons.com/) через `egui-phosphor` | Иконки | MIT |
| [egui / eframe](https://github.com/emilk/egui) | Интерфейс | MIT / Apache 2.0 |
| [rodio](https://github.com/RustAudio/rodio), [cpal](https://github.com/RustAudio/cpal) | Звук и микрофон | MIT / Apache 2.0 |
| [windows-rs](https://github.com/microsoft/windows-rs) | API Windows (медиа, уведомления) | MIT / Apache 2.0 |
| [global-hotkey](https://github.com/tauri-apps/global-hotkey), [tray-icon](https://github.com/tauri-apps/tray-icon) | Горячие клавиши и трей | MIT / Apache 2.0 |

Полный список Rust-зависимостей — в `Cargo.lock`. Тексты лицензий шрифтов лежат в `resources/fonts`.
