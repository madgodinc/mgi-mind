# MGI-Mind

**[English](README.md)** | **[Русский](README.ru.md)** | **[中文](README.zh.md)**

AI-нативный второй мозг. Самохостинг, оптимизация токенов, память для AI-ассистентов.

Замени Obsidian/Notion на систему где AI сам всё запоминает за тебя.

```
"Какой адрес сервера?"

-> mgimind search "адрес сервера"
-> Деплой сервер 10.0.0.5:8080 (score: 0.72)
```

> **v0.2.0** — слои данных и безопасности перестроены по итогам полного аудита кода:
> атомарные записи, контент-адресные ID (идемпотентный upsert), скрытый/затираемый
> мастер-пароль vault, проверка SHA-256 загрузок, Qdrant только на 127.0.0.1,
> пер-агентные сессии, нативный HTTP/архивы, тесты + CI. Полный учёт всех пунктов —
> [`AUDIT_STATUS.md`](AUDIT_STATUS.md), изменения — [`CHANGELOG.md`](CHANGELOG.md).

Полная документация с примерами кода - в [английском README](README.md).

---

## Быстрый старт

```bash
git clone https://github.com/madgodinc/mgi-mind.git
cd mgi-mind
cargo build --release

mgimind init
mgimind doctor --fix   # скачает Qdrant, ONNX Runtime, модель
mgimind serve          # запустит векторную БД

mgimind create work
mgimind add work "Деплой сервер 10.0.0.5, порт 8080"
mgimind search "адрес сервера"
```

## MCP интеграция

```json
{
  "mcpServers": {
    "mgi-mind": {
      "command": "bun",
      "args": ["run", "/путь/к/mgi-mind/mcp-server/index.js"]
    },
    "crw": {
      "command": "crw-mcp"
    }
  }
}
```

## Возможности

- **Семантический поиск** - ищет по смыслу, не по словам
- **Граф знаний** - структурированные факты (субъект -> предикат -> объект)
- **Логи сессий** - AI помнит что было в прошлый раз
- **Зашифрованное хранилище** - пароли, SSH, API-ключи (AES-256-GCM + Argon2)
- **Веб-ридер** - AI читает любые страницы через CRW (Rust)
- **Импорт из Obsidian** - затягивает .md файлы из существующего хранилища
- **Экспорт** - JSON и Markdown
- **Дедупликация** - не сохраняет одинаковые записи дважды
- **Самонастройка** - AI предлагает прописать правила в свой конфиг
- **Tiered retrieval** - 10-20x экономия токенов
- **Кросс-платформа** - Windows, macOS (Intel + Apple Silicon), Linux

## Все команды

```
mgimind init                - инициализация
mgimind doctor --fix        - автоустановка зависимостей
mgimind serve / stop        - запуск/остановка Qdrant

mgimind create <lib>        - создать библиотеку
mgimind drop <lib>          - удалить библиотеку
mgimind list                - список библиотек
mgimind add <lib> "текст"   - добавить запись
mgimind search "запрос"     - семантический поиск (--tier 1/2/3)
mgimind delete <lib> <id>   - удалить конкретную запись

mgimind fact add S P O      - добавить факт в граф знаний
mgimind fact query S        - найти факты
mgimind fact invalidate <id> - удалить факт

mgimind session start       - начать лог сессии
mgimind session last        - прочитать прошлую сессию
mgimind session end         - завершить с итогом

mgimind vault store K V     - сохранить секрет (AES-256-GCM)
mgimind vault get K         - получить (мастер-пароль)
mgimind vault list          - список ключей (без значений)
mgimind vault delete K      - удалить секрет

mgimind context             - брифинг для AI
mgimind history             - последние записи
mgimind stats               - статистика
mgimind web <url>           - прочитать страницу (через CRW)
mgimind web <url> --save X  - прочитать и сохранить в библиотеку
mgimind import obsidian /path - импорт из Obsidian
mgimind export --format json  - экспорт данных

mgimind backup <file>       - бэкап
mgimind restore <file>      - восстановление
```

## Стек

| Компонент | Технология |
|-----------|-----------|
| Ядро | Rust |
| Векторная БД | Qdrant |
| Эмбеддинги | ONNX Runtime (all-MiniLM-L6-v2) |
| MCP-сервер | Bun |
| Веб-ридер | CRW (Rust) |
| Шифрование | AES-256-GCM + Argon2 |
| Лицензия | Apache 2.0 |

## Лицензия

Apache 2.0 - [Mad God Inc](https://github.com/madgodinc), 2026
