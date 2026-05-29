# MGI-Mind

**[English](README.md)** | **[Русский](README.ru.md)** | **[中文](README.zh.md)**

Долговременная память для AI-ассистентов на самохостинге. Ассистент сохраняет важное
во время работы и потом находит это по смыслу, а не начинает каждый раз с нуля. Всё
работает на твоей машине: бинарь на Rust, локальная векторная база Qdrant и локальные
ONNX-модели. Без облака, без API-ключей, данные никуда не уходят.

```
"Какой был адрес сервера для деплоя?"

  mgimind search "адрес сервера для деплоя"
  -> Деплой сервер 10.0.0.5:8080   (source: infra.md)
```

Ассистент подключается по MCP (Model Context Protocol), поэтому Claude Code и другие
инструменты читают и пишут память напрямую. Работает и как обычный CLI.

## Статус

Текущая версия: **0.7.x**. Проект прошёл полный аудит безопасности и качества
(27 пунктов), все закрыты. Поштучный учёт - в [`AUDIT_STATUS.md`](AUDIT_STATUS.md),
история по релизам - в [`CHANGELOG.md`](CHANGELOG.md).

Главное - это поиск:

- **Гибридный поиск.** Каждая запись хранится двумя векторами: плотный (dense,
  multilingual-e5-base) ловит смысл, разреженный BM25 ловит точные слова. Запрос идёт
  по обоим, результаты сливаются через Reciprocal Rank Fusion - то есть сразу и
  смысловая полнота, и точность по ключевым словам.
- **Реранкер (cross-encoder).** Топ кандидатов после слияния пере-оценивает
  cross-encoder (bge-reranker-base), который читает запрос и текст вместе. Включён по
  умолчанию, силён на английском.
- **Тёплый демон.** Долгоживущий процесс держит модели загруженными и обслуживает
  MCP-клиента через Unix-сокет, чтобы каждый поиск не платил за загрузку моделей.

## Как это работает

- **Хранилище.** Все записи в одной коллекции Qdrant. Поле `library` разделяет
  пространства имён (например рабочие заметки и геймдизайн в одном месте, с фильтром
  при необходимости). ID точки - это UUIDv5 от `library + content`, поэтому повторное
  добавление того же текста просто перезаписывает запись, без дублей.
- **Эмбеддинги.** Текст кодируется локально через ONNX Runtime. По умолчанию
  multilingual-e5-base (768 измерений) - силён на английском и тянет смешанные языки.
  Эмбеддер настраиваемый (пулинг, префиксы, token_type_ids), смена модели не требует
  правок кода. Вход обрезается до 512 токенов.
- **Поиск.** Запрос кодируется один раз, Qdrant за один вызов делает плотный и
  разреженный поиск и сливает их через RRF. Если реранк включён, топ `rerank_top_k`
  пере-оценивается cross-encoder'ом. Фильтр по `library` применяется к обеим веткам.
- **Безопасность.** Загрузки проверяются по SHA-256 где есть пины. Qdrant слушает
  только loopback и может требовать API-ключ. Хранилище секретов работает только в
  терминале: мастер-пароль вводится без эха, затирается из памяти после использования
  и никогда не уходит через MCP. Запись файлов атомарна (temp + rename).

## Установка (Linux)

Нужны: тулчейн Rust (`rustup`) и Node или Bun для MCP-сервера.

```bash
git clone https://github.com/madgodinc/mgi-mind.git
cd mgi-mind
cargo build --release           # бинарь: target/release/mgimind

target/release/mgimind init
target/release/mgimind doctor --fix   # скачает ONNX Runtime, Qdrant и модели
target/release/mgimind serve          # запустит Qdrant
target/release/mgimind doctor         # проверка
```

`doctor --fix` кладёт в `~/mgimind/`: ONNX Runtime, бинарь Qdrant, модель эмбеддингов
(multilingual-e5-base, квантованный ONNX, ~270 МБ) и реранкер (bge-reranker-base,
~280 МБ). Если ONNX Runtime лежит не рядом с бинарём, укажи путь в `ORT_DYLIB_PATH`.

macOS и Windows собираются так же (`cargo build --release`).

### MCP-сервер

```bash
cd mcp-server
bun install        # или: npm install
```

Подключение к Claude Code:

```bash
claude mcp add mgi-mind -- node /абсолютный/путь/mgi-mind/mcp-server/index.js
```

MCP-сервер сначала идёт в тёплый демон (`mgimind daemon`), а если его нет - запускает
CLI. Работает в любом случае.

## Команды

```
mgimind add <lib> "текст" [--source тег]   добавить запись
mgimind search "запрос" [--library l] [--limit N] [--tier 1|2|3]
                                           гибридный поиск + реранк
mgimind history [--limit N]                последние записи
mgimind delete <lib> <id>                  удалить одну запись
mgimind context                            брифинг к началу сессии

mgimind create <lib> / drop <lib> / list   библиотеки
mgimind stats                              статистика

mgimind fact add S P O                     факт (субъект-предикат-объект)
mgimind fact query <term>                  поиск фактов
mgimind fact invalidate <id>               пометить факт недействительным

mgimind session start --agent <имя>        начать сессию
mgimind session end --agent <имя> --summary "итог"
mgimind session last [--agent <имя>]       прошлая сессия

mgimind vault store K V / get K / list / delete K   секреты (только терминал)

mgimind serve / stop                       запуск/остановка Qdrant
mgimind daemon                             тёплый демон
mgimind migrate [--purge]                  перенос старых коллекций в новую
mgimind backup <file> / restore <file>     бэкап/восстановление
mgimind export [--format json|md]          экспорт
mgimind import obsidian /path              импорт markdown
mgimind doctor [--fix]                     проверка/автоустановка
```

## Настройки

Конфиг: `~/mgimind/config.json`. Важное для поиска:

| Поле | По умолчанию | Что значит |
|---|---|---|
| `model_name` | `multilingual-e5-base` | папка модели в `models/` |
| `vector_size` | `768` | размерность эмбеддинга (должна совпадать с моделью) |
| `pooling` | `mean` | `mean` (e5, MiniLM) или `cls` (часть XLM-R) |
| `uses_token_type_ids` | `false` | true для BERT-семейства, false для XLM-R/e5 |
| `query_prefix` / `passage_prefix` | `query: ` / `passage: ` | для e5 нужны, для других пусто |
| `rerank_enabled` | `true` | реранк cross-encoder (силён на английском) |
| `rerank_model` | `bge-reranker-base` | папка реранкера в `models/` |
| `rerank_top_k` | `20` | сколько кандидатов берём и реранкаем |

### Про модели и языки

- Эмбеддер мультиязычный: английский запрос находит русский текст и наоборот.
- Реранкер по умолчанию (bge-reranker-base) заточен под английский и улучшает
  английскую выдачу. Русскую ранжировку он ухудшает - для русскоязычного контента
  ставь `rerank_enabled=false` (гибрид dense+sparse и так хорошо ранжирует русский)
  либо бери более сильный мультиязычный реранкер.
- Реранк добавляет инференс на запрос. На CPU это примерно 1-2 секунды на 20
  кандидатов. Снизь `rerank_top_k` или выключи реранк, если нужна меньшая задержка.

### Смена модели эмбеддингов

Смена модели обычно меняет размерность, значит записи надо переэмбеддить. Пропиши
`model_name`, `vector_size`, `pooling`, `uses_token_type_ids` и префиксы под новую
модель, запусти `mgimind doctor --fix` (скачает её), затем `mgimind migrate` (он
переэмбеддит из сохранённого текста). Сначала сделай бэкап.

## Лицензия

Apache-2.0 - [Mad God Inc](https://github.com/madgodinc), 2026
