# 插件能力声明

`capabilities` 为必填的封闭声明数组。每项有插件内唯一的 `id` 和 `kind`。加载时检查字段、操作、工具 schema、重复路由及依赖；调用前核对 `(plugin_id, capability_id, operation)`，请求与响应均接受校验。业务代码不根据运行时或插件 ID 选择能力。

| kind | 声明字段 | 固定操作 |
| --- | --- | --- |
| `metadata_provider` | `operations`、`search_fields`、`result_fields`、可选 `filters_schema`、`auto_scrape` | `search`，可选 `detail`、`list_chapters`、`chapter_detail`、`resolve_audio`、`fetch_cover` |
| `format_handler` | `extensions`、`operations` | `probe`、`extract_metadata`；可选 `get_metadata_read_size`、`write_metadata` |
| `tool_provider` | `invoke: invokeTool`、`tools`，每项有名称、双向 schema、说明、`side_effects` | `invokeTool` |
| `http_route` | `route.method/path/auth` | `handle` |
| `ui_extension` | `slots`、`contexts`、`title`、`render` | `open` |
| `plugin_store` | `operations` | `list_plugins`，可选 `get_plugin` |
| `content_processor` | `extensions`、`operations` | `probe/open/close/cancel`；可选 `extract_metadata/read_text/render_page/seek` |
| `task_handler` | `tasks`，每项有 `task_type/input_schema/output_schema/idempotent` | `run` |
| `event_handler` | `events`，每项有 `name/schema` | `handle` |

## 元数据搜索接口

`metadata_provider` 的 `search` 接收以下请求，例如：

```json
{
  "title": "示例书籍",
  "author": null,
  "narrator": null,
  "page": 1,
  "page_size": 20,
  "filters": {},
  "chapter_candidates": [],
  "context": null
}
```

| 请求字段 | 类型 | 说明 |
| --- | --- | --- |
| `title`、`author`、`narrator` | `string \| null` | 三个字段均须存在；未知值为 `null`，每项最多 512 字节 |
| `page` | 正整数 | 页码，从 1 开始 |
| `page_size` | 整数 | 每页 1–100 条 |
| `filters` | 对象 | 平台搜索条件；省略时为 `{}`，序列化后最多 8 KiB |
| `chapter_candidates` | 数组 | 章节标题处理时提供的 `{id, title}`；省略时为 `[]`，最多 500 条，每个非空 `id` 和 `title` 最多 512 字节 |
| `context` | `object \| null` | 宿主提供的搜索上下文；省略时为 `null`，最多 128 KiB。`mode` 为 `search`、`aggregate` 或 `title_cleanup`，可含 `candidates`（最多 100 项）、`merged_metadata`、`scanner_context`、`scraper_query`（最多 512 字节） |

成功结果的 `data` 为 `SearchPage`：

| 结果字段 | 类型 | 说明 |
| --- | --- | --- |
| `items` | 数组 | 搜索结果，每项为下表的书籍元数据 |
| `page`、`page_size` | 整数 | 页码从 1 开始，`page_size` 为 1–100，`items` 不超过该值；分页大小与源站实际分页一致 |
| `total` | 非负整数或 `null` | 已知总数；未知时为 `null` |
| `has_more` | `boolean \| null` | 是否还有下一页；未知时为 `null` |

每项书籍元数据包含：

| 字段 | 类型 | 说明 |
| --- | --- | --- |
| `title` | `string` | 非空标题，最多 512 字节 |
| `id`、`source_url` | `string \| null` | 源站标识和详情地址 |
| `author`、`narrator`、`cover_url`、`intro` | `string \| null` | 作者、演播者、封面地址和简介 |
| `subtitle`、`publisher`、`language`、`genre` | `string \| null` | 副标题、出版方、语言和分类 |
| `published_year` | 整数或 `null` | 出版年份，范围 0–65535 |
| `published_date` | `string \| null` | `YYYY-MM-DD` 格式的出版日期 |
| `isbn`、`asin` | `string \| null` | 书籍标识 |
| `explicit`、`abridged` | `boolean \| null` | 内容分级和是否删节 |
| `duration` | 非负整数或 `null` | 时长，单位为秒 |
| `score` | 数值或 `null` | 匹配评分，范围 0–1 |
| `tags` | 字符串数组 | 未知时为 `[]`，最多 64 项，每项非空且最多 256 字节 |
| `chapter_title_template` | `string \| null` | 章节标题模板 |
| `chapter_titles` | 字符串数组 | 未知时为 `[]`，最多 500 项，每项非空且最多 512 字节 |

JavaScript 使用 `publishSearch`，Rust 使用 `publish_search_for_request` 整理解析结果。对外结果包含上表全部字段，可空字段显式为 `null`，未知数组为 `[]`；平台内部的额外字段保留在插件内部。实现示例见[开发指南](./plugin-dev.md#4-实现业务)。

## 扩展格式

```yaml
capabilities:
  - id: format.handler
    kind: format_handler
    extensions: [example]
    operations: [probe, get_metadata_read_size, extract_metadata, write_metadata]
```

此声明只注册支持的扩展名和操作；格式解析和元数据规则由插件实现。宿主以扩展名筛选、`probe` 验证，使用宿主资源句柄调用；Rust 插件通过 `ting_plugin_sdk::contract::format` 和 `format_calls` 使用元数据及 staging 写回类型。普通音频和 FFmpeg 管理属于核心。

## UI 与权限

```yaml
capabilities:
  - id: assistant.panel
    kind: ui_extension
    slots: [app.sidebar_page, global.floating_action]
    contexts: [global]
    title: { zh: 助手, en: Assistant }
    render:
      mode: web_container
      entry: ui/panel.html
      bridge:
        capabilities: [assistant.tools]
        host_methods: [user_settings.get]
permissions:
  - type: user_settings_read
```

合法的 `slots` 为 `app.sidebar_page`、`global.floating_action`、`global.panel` 和 `book.detail_action`；`contexts` 为 `global/book/reader`；`render.mode` 为 `web_container/action`。`web_container` 的页面在宿主的 sandbox 中运行，`bridge` 白名单与当前授权一起校验。网络权限使用 `network_access.domain`，文件权限使用 `file_read.path` / `file_write.path`，事件订阅使用 `event_subscribe.event`；书籍和库的读取权限分别使用 `books_read` 与 `libraries_read`。

运行时具体写法见 [JavaScript](./js_runtime_guide.md)、[WASM](./wasm_runtime_guide.md) 和 [Native](./native_runtime_guide.md) 指南。
