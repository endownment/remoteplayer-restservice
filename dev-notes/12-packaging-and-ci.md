# 12. Deb-пакеты, CI и журнал systemd

## Что собирается

`.github/workflows/packages.yml` собирает три пакета: `amd64` (обычный ПК),
`arm64` (64-битная Raspberry Pi OS) и `armhf` (32-битная). Таргеты Rust:

| Debian | Rust target | Где работает |
|---|---|---|
| `amd64` | `x86_64-unknown-linux-gnu` | любой x86-64 |
| `arm64` | `aarch64-unknown-linux-gnu` | Pi 3/4/5 с 64-битной ОС |
| `armhf` | `armv7-unknown-linux-gnueabihf` | Pi 2/3/4 с 32-битной ОС; **не** Pi 1/Zero (ARMv6) |

Кросс-компиляция в Rust проще, чем в C++: компилятор один и тот же, нужна
только стандартная библиотека под таргет (`rustup target add`) и линкер.
Линкер берём GNU (`gcc-aarch64-linux-gnu`), он прописан в `.cargo/config.toml`.
Сторонних C-библиотек проект не линкует (D-Bus — чистый Rust `zbus`), поэтому
sysroot с `-dev`-пакетами не нужен — только `libc6-dev-*-cross`.

## Почему сборка внутри debian:bookworm

Бинарник требует glibc *не новее*, чем на машине сборки (символы вроде
`GLIBC_2.39`). Ubuntu на раннере GitHub новее, чем Raspberry Pi OS, поэтому
пакеты собираются в контейнере Debian 12 (glibc 2.36): такой бинарник
работает и на bookworm, и на trixie. Шаг «Inspect the package» проверяет это
`readelf`-ом и роняет сборку, если версия выше `MAX_GLIBC`. Число должно
совпадать с `libc6 (>= 2.36)` в `Cargo.toml`.

## cargo-deb

Описание пакета — секция `[package.metadata.deb]` в `Cargo.toml`: зависимости,
список файлов (`assets`) и каталог со скриптами сопровождающего
(`packaging/debian/`). Всё, что лежит в `/etc`, cargo-deb автоматически
помечает как conffile: dpkg не затрёт правленый `restservice.toml` при
обновлении, а спросит.

Скрипты:

- `postinst configure` — создаёт системного пользователя `remoteplayer`,
  каталоги в `/srv/remoteplayer` (с `|| true`: на read-only системе это не
  ошибка), делает `daemon-reload` и `try-restart` REST-сервиса. mpv при
  обновлении **не** перезапускается — показ на телевизоре не прерывается.
  Ничего не включается автоматически: сначала нужно вписать токен.
- `prerm remove` — `disable --now` обоих юнитов.
- `postrm` — `daemon-reload`; пользователь и медиатека остаются даже при
  `purge` (удалять чужие видео пакет не вправе).

Скрипты проверяют `/run/systemd/system`: в chroot или контейнере без systemd
`systemctl` звать нельзя.

## Журнал systemd

Под systemd переменная `JOURNAL_STREAM` установлена, и логирование идёт через
`tracing-journald` — нативный протокол журнала, а не строки в stderr. Что это
даёт:

- уровень события становится приоритетом: `journalctl -p warning` показывает
  только предупреждения и ошибки;
- поля события (`socket = %path`) становятся полями журнала с префиксом `F_`:
  `journalctl F_SOCKET=/run/remoteplayer-mpv/mpv.sock`.

Если подключиться к журналу не удалось, сервис пишет в stderr — systemd
всё равно положит это в журнал, только без полей.

Потеря и появление сокета mpv логируются **по одной строке на переход**:
«не доступен, жду», «подключился», «потерял, жду возврата». Сервис не спамит
журнал строкой на каждую попытку переподключения — флаг `waiting_reported`
в `src/mpv/client.rs` (в наблюдателе — проверка `mpv_error` в `tracker.rs`).

## Код выхода 78

`EX_CONFIG` из `sysexits.h`. Ошибка конфигурации — не тот случай, когда
перезапуск поможет, поэтому `main` возвращает 78, если ошибка —
`ConfigError` (`downcast_ref` по `Box<dyn Error>` — аналог `instanceof` в
Java или `dynamic_cast` в C++), а юнит содержит `RestartPreventExitStatus=78`.
Сервис остаётся в состоянии failed с понятной строкой в журнале вместо
бесконечного цикла перезапусков.

## Релиз

Тег `vX.Y.Z` → job `check` сверяет его с `version` в `Cargo.toml` → три
пакета публикуются в GitHub Release. Обычный push только прикладывает пакеты
к запуску (Actions → запуск → Artifacts).
