# 03. Владение и заимствование — на примерах из кода

## Три правила

1. У каждого значения ровно один **владелец** (переменная, поле, элемент
   коллекции).
2. Когда владелец выходит из области видимости, значение уничтожается
   (вызывается `Drop`, как деструктор в C++). Сборщика мусора нет.
3. Можно временно **одолжить** значение: сколько угодно читателей (`&T`)
   **или** ровно одного писателя (`&mut T`) — одновременно не бывает.
   Компилятор (borrow checker) проверяет это при компиляции.

Если сравнивать с C++: `T` — это объект на стеке или `std::unique_ptr<T>`,
`&T` — `const T&`, `&mut T` — `T&`, `Arc<T>` — `std::shared_ptr<T>`. Только
висячих ссылок и гонок данных компилятор не допустит.

## Перемещение вместо копирования

```rust
// src/mpv/client.rs
let request: PendingRequest = PendingRequest { request_id, line: encode_command(request_id, &command), reply: reply_sender };
self.shared.requests.send(request).await?;
// дальше `request` использовать нельзя — он перемещён в канал
```

Присваивание и передача по значению **перемещают**: это `std::move` по
умолчанию. Копируются неявно только простые типы с маркером `Copy` (числа,
`bool`, `char`, кортежи из них). Для остального нужен явный `.clone()`, и это
хорошо видно при чтении: в коде, где clone, тратится память.

## `&self`, `&mut self`, `self`

```rust
impl ConnectionTask {
    async fn run(mut self) { ... }                                   // поглощает: задача владеет всем до конца
    async fn serve(&mut self, stream: UnixStream, generation: u64)   // меняет поля задачи
    fn dispatch_line(&self, line: &str, pending: &mut HashMap<...>)  // читает self, меняет только pending
}
```

Сигнатура сразу говорит, что метод делает с объектом. В Java это пришлось бы
выяснять, читая тело метода.

## Заимствование без копирования: срезы

```rust
// src/storage/md5_cache.rs
let count: usize = file.read(&mut buffer)?;
hasher.update(&buffer[..count]);
```

`&buffer[..count]` — срез, то есть указатель плюс длина, как `std::span`.
Данные не копируются, а компилятор гарантирует, что `buffer` жив, пока жив
срез.

## Разделяемое владение: `Arc`

`AppContext` нужен всем запросам одновременно, а владелец может быть только
один. Поэтому его оборачивают в `Arc` (atomic reference counted):

```rust
pub type SharedContext = Arc<AppContext>;
```

`Arc::clone(&context)` увеличивает счётчик, и данные не копируются. Когда
последний `Arc` исчезает, `AppContext` уничтожается. `Arc` даёт только чтение.
Для изменения внутри нужна синхронизация (`Mutex`, `RwLock`, атомики), см.
`SettingsStore::current` (`RwLock`) и `planned_restart` (`AtomicBool`).

`Arc<str>` в mpvwatcher — та же идея для строки: все клиенты сокета получают
указатель на один и тот же текст.

## RAII и отмена: `PartialFile`

```rust
// src/storage/upload.rs
struct PartialFile { path: PathBuf, committed: bool }

impl Drop for PartialFile {
    fn drop(&mut self) {
        if !self.committed { let _ = std::fs::remove_file(&self.path); }
    }
}
```

Если клиент оборвал загрузку, axum просто **уничтожает** future запроса. Код
после текущего `.await` не выполнится, `finally` нет. Но всё, чем future
владела, уничтожается, и `Drop` удаляет недокачанный файл. В Java пришлось бы
надеяться на try/finally, который при отмене потока срабатывает не всегда.

`commit(mut self)` принимает `self` по значению: после коммита guard
поглощён и использовать его повторно нельзя — это проверяет компилятор.

## Мьютекс-гард

```rust
// src/lifecycle.rs
let _guard: MutexGuard<'_, ()> = lock_player(context)?;
```

Блокировка снимается, когда уничтожается гард, то есть в конце функции.
Тонкость: имя `_guard` держит значение до конца области, а голое `_`
уничтожило бы его сразу, и блокировка отпустилась бы немедленно.

## Время жизни (`'a`, `'_`, `'static`)

Время жизни — это имя для «сколько живёт заимствование». Чаще всего компилятор
выводит его сам. Где оно видно в коде:

- `MutexGuard<'_, ()>` — гард заимствует мьютекс; `'_` значит «выведи сам».
- `Field<'_>` в multipart — часть формы заимствует сам `Multipart`, поэтому
  следующую часть нельзя взять, пока не дочитана текущая. Это проверено
  компилятором.
- `&'static str` — строка, живущая всё время работы программы (литерал).
  Поэтому `SettingChange::key` имеет тип `&'static str`: ключи — литералы в
  коде, и копировать их не нужно.

## `take()`, `as_mut()`, `Option<&mut T>`

```rust
// src/service_manager/child_process.rs
if let Some(stdout) = child.stdout.take() { tokio::spawn(collect_output(stdout, ...)); }
```

`take()` забирает значение из `Option`, оставляя `None`. Так «вынимают»
владение из поля, не разрушая структуру. Потоком вывода дальше владеет
отдельная задача.

```rust
// mpvwatcher, src/main.rs
match self.connection.as_mut() { Some(connection) => capture(connection).await, ... }
```

`as_mut()` превращает `&mut Option<T>` в `Option<&mut T>`: это заимствование
содержимого без перемещения.
