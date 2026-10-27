# 09. Как всё проверить без Raspberry Pi

## Что подменяется и где лежит

Заглушки — это не моки в тестах, а полноценные варианты бэкендов, которые
выбираются в конфигурации. Каждая лежит рядом с настоящей реализацией:

| Что на Pi | Заглушка | Где | Как включить |
|---|---|---|---|
| systemd-юнит mpv | mpv как дочерний процесс: перезапуск с новыми аргументами, авто-перезапуск при выходе (как `Restart=always`), журнал в памяти | `src/service_manager/child_process.rs` | `[service-manager] kind = "child-process"` |
| `/dev/cec0` | фиксированный отчёт «телевизор LG, включён» | `src/cec/stub.rs` | `[cec] kind = "stub"` |
| перезагрузка через logind | запись в лог вместо перезагрузки | `src/power.rs` (`PowerControl::Stub`) | `[system] power = "stub"` |
| DRM-вывод mpv | окно mpv на рабочем столе | `config/dev.toml`, `[player-defaults]` | уже в dev.toml |

Остальное заглушек не требует и работает на любом Linux как есть:
`/sys/class/drm` (у ноутбука будут `eDP-1`, `HDMI-A-1`...), список аудиоустройств
mpv (на десктопе там будут `pulse`/`pipewire`), `/proc`, термодатчик.

`config/dev.toml` собирает всё это вместе и складывает состояние в
`./dev-data` (он в `.gitignore`).

## Запуск

```sh
cargo run -- --config config/dev.toml
```

Появится окно mpv в режиме простоя. Дальше — запросы из README или
[docs/API.md](../docs/API.md). Что стоит проверить руками:

1. `PATCH /settings {"hwdec": "no"}` → `"applied": "live"`, окно mpv не
   перезапускается.
2. `PATCH /settings {"script-opts": "osc-visibility=always"}` во время
   воспроизведения → mpv перезапускается, файл продолжается с той же позиции.
3. Закрыть окно mpv (или `kill -9`) → через 3 секунды «супервизор» поднимет
   mpv заново. Если рядом запущен mpvwatcher (его `state-socket` — это
   `restore.watcher-socket` из dev.toml, `dev-data/watcher/state.sock`), сервис восстановит
   воспроизведение по его снимку.
4. `PATCH /settings {"vo": "nonexistent", "force-window": true}` → окно mpv не
   открывается; сервис откатывает настройки и возвращает 502 с логом mpv.
5. `PATCH /settings {"demuxer-max-bytes": "abc"}` → mpv отклоняет значение на лету,
   ответ 422, ничего не сохранено.

## Режим «только для чтения» без Pi

Права обычного пользователя проверяются через `chmod`, а настоящее
read-only монтирование — от root:

```sh
mkdir -p ro/media/inbox ro/state
sudo mount --bind ro ro && sudo mount -o remount,bind,ro ro
# в копии dev.toml: upload-root = "ro/media", settings-file/args-env-file в ro/state
```

Что должно получиться: `GET /storage` → `"mode": "read-only"`, загрузка и
создание папки → 403 «read-only», список и воспроизведение работают,
`PATCH /settings` → 403 «nothing was changed», а mpv свои значения сохраняет.
`upload-root` в `/dev/shm` (tmpfs) при `allow-volatile = false` даёт 403
«RAM-backed filesystem». Без `upload-root` — 501.

## Юнит-тесты

`cargo test` не требует ни mpv, ни железа (больше 50 тестов). Внешний мир в
них подменён прямо в коде тестов:

- **mpv** — крошечный сервер на `UnixListener` внутри теста
  (`mpv/client.rs`, `run_fake_mpv`), который отвечает на запросы и шлёт событие.
- **sysfs DRM** — дерево каталогов во временной папке, с синтетическим EDID
  (`display.rs`, `sample_edid`: корректный блок с контрольной суммой).
- **файловая система** — `tempfile::tempdir()`.
- **CEC** — проверяются раскладка структур (на этапе компиляции, см.
  [10](10-hdmi-cec-and-unsafe.md)) и сборка сообщений. Сам ioctl без устройства
  не вызвать.

## Настоящий CEC без Pi

В ядре Linux есть тестовый драйвер `vivid`, который умеет эмулировать
CEC-адаптеры, соединённые друг с другом, — подходит, чтобы прогнать
`cec/linux.rs` с настоящими ioctl. Ядро должно быть собрано с поддержкой CEC в
vivid (`CONFIG_VIDEO_VIVID_CEC`); проверь `modinfo vivid`. Ещё вариант —
USB-адаптер Pulse-Eight (`/dev/cecN` через драйвер `pulse8-cec`). Затем в
конфиге:

```toml
[cec]
kind = "linux"
device = "/dev/cec0"   # или номер, который появился
```

Утилита `cec-ctl` из пакета `v4l-utils` помогает смотреть, что происходит
на шине.

## Версия mpv

Восстановление плейлиста использует синтаксис `loadfile` из mpv ≥ 0.38. На
более старом mpv всё остальное работает, а восстановление вернёт
предупреждение `invalid parameter` и восстановит только громкость, паузу и т.п.
