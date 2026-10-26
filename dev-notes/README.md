# dev-notes — как устроен remoteplayer-restservice

Заметки о реализации и одновременно учебник по Rust на примере этого кода.
Написаны для человека, который вырос на C и C++, писал на PHP, любил .NET и
сейчас работает с Java. Там, где удобно, Rust сравнивается с этими языками.

Порядок чтения:

| Файл | О чём |
|---|---|
| [01-architecture.md](01-architecture.md) | компоненты, потоки данных, почему решения такие, а не другие |
| [02-rust-for-oop-developers.md](02-rust-for-oop-developers.md) | отображение привычных понятий ООП на Rust |
| [03-ownership-in-this-code.md](03-ownership-in-this-code.md) | владение и заимствование на конкретных местах кода |
| [04-async-and-tokio.md](04-async-and-tokio.md) | async/await, Tokio, каналы, акторы, отмена |
| [05-errors.md](05-errors.md) | `Result`, `?`, `thiserror`, превращение ошибок в HTTP-ответы |
| [06-mpv-ipc.md](06-mpv-ipc.md) | протокол mpv, клиент, снимок состояния и восстановление |
| [07-settings-and-systemd.md](07-settings-and-systemd.md) | модель настроек, EnvironmentFile, перезапуск с откатом, polkit, D-Bus |
| [08-uploads-and-storage.md](08-uploads-and-storage.md) | потоковая загрузка, MD5, безопасность путей |
| [09-testing-without-pi.md](09-testing-without-pi.md) | заглушки и как всё проверить на обычном Linux |
| [10-hdmi-cec-and-unsafe.md](10-hdmi-cec-and-unsafe.md) | CEC, ioctl и единственный `unsafe` в проекте |
| [11-code-style.md](11-code-style.md) | правила оформления кода в этом проекте |

Как читать сам код: `src/main.rs` → `src/app_context.rs` → `src/routes/mod.rs`
(карта всех эндпоинтов) → нужный обработчик в `src/routes/*.rs` → модуль,
который он вызывает.
