# 05. Ошибки

## `Result` и `?`

```rust
pub fn load(path: &Path) -> Result<AppConfig, ConfigError> {
    let text: String = std::fs::read_to_string(path).map_err(|source: std::io::Error| ConfigError::Read { ... })?;
    let config: AppConfig = toml::from_str(&text).map_err(...)?;
    config.validate()?;
    Ok(config)
}
```

`expr?` означает: если `Err(e)`, вернуть из функции `Err(From::from(e))`, иначе
развернуть значение. Это checked exceptions Java, только без стека вызовов и
с проверкой компилятором, что каждая ошибка либо обработана, либо проброшена.
Забыть обработать `Result` нельзя: компилятор предупредит. Поэтому в коде
встречается `let _ignored: Result<...> = ...;` — явная пометка «результат не
важен», а не тихий пропуск.

## Типы ошибок через `thiserror`

У каждого модуля свой enum ошибок:

```rust
#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("folder `{0}` does not exist")]
    FolderNotFound(String),
    #[error("not enough free space: {required} bytes required, {available} available")]
    InsufficientSpace { required: u64, available: u64 },
    ...
}
```

`thiserror` генерирует `Display` (текст из `#[error(...)]`) и `std::error::Error`.
`#[from]` на поле генерирует `From<Inner> for Outer`, и тогда `?`
преобразует ошибку автоматически. Это та же иерархия исключений, только
плоская: вместо наследования — вложение (`LifecycleError::Settings(SettingsError)`).

## Граница: доменная ошибка → HTTP

Модули не знают про HTTP. Решение «какой это статус» принимается в одном месте —
`src/error.rs`:

```rust
impl From<StorageError> for ApiError {
    fn from(error: StorageError) -> ApiError {
        let status: StatusCode = match error {
            StorageError::FolderNotFound(_) | StorageError::FileNotFound(_) => StatusCode::NOT_FOUND,
            StorageError::AlreadyExists(_) | StorageError::FolderNotEmpty(_) => StatusCode::CONFLICT,
            ...
        };
        ApiError::new(status, error.to_string())
    }
}
```

`match` перечисляет все варианты без `_ =>`. Появится новый вариант
`StorageError` — компилятор заставит решить, какой у него статус.

Обработчик пишет просто `context.library.list_files(&folder).await?`, а `?`
вызывает этот `From`. `ApiError` реализует `IntoResponse`, и axum
превращает его в JSON по RFC 9457.

Ошибки разбора тела запроса тоже приходят в этом формате — через свои
экстракторы `JsonBody`/`QueryParams` (`src/routes/extract.rs`). Стандартный
`axum::Json` ответил бы обычным текстом.

## `main` и `Box<dyn Error>`

В `main.rs` ошибки разных типов просто доносятся до пользователя и не
обрабатываются по-разному, поэтому там `Result<(), Box<dyn std::error::Error>>` —
«любая ошибка», аналог `throws Exception`. Это допустимо на самом верху
программы, но не в модулях.

## Паники

`panic!`, `unwrap()`, `expect()` останавливают поток (в нашем случае задачу) —
это для «невозможных» ситуаций, то есть багов. В коде сервиса их нет, кроме
одного `unreachable!` в `ServiceManager::from_config`: этот инвариант
гарантирует `AppContext`. В тестах `expect("...")` — норма: падение теста и
есть нужный результат.

Отравленный мьютекс (`PoisonError`) — это не паника, а сигнал «другой поток
упал, держа блокировку». В `SettingsStore` он сознательно игнорируется
(`unwrap_or_else(PoisonError::into_inner)`): внутри лежит целое значение
`PlayerSettings`, а не наполовину изменённая структура.
