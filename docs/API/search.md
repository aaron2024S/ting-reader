# 搜索与刮削

## GET /api/v1/search

搜索在线书籍。

搜索结果在后端内存中缓存 5 分钟。相同数据源、搜索参数、页码和每页数量的重复请求复用结果；不同参数分别缓存，失败请求不写入缓存。缓存最多保留 100 项，后端重启后清空。`GET /api/v1/search` 与 `POST /api/v1/scraper/search` 共用这份缓存，客户端和插件无须额外处理。

查询参数：

| 参数 | 类型 | 说明 |
| --- | --- | --- |
| `q` | string | 搜索关键词 |
| `source` | string | 刮削源列表返回的 `id`，可选 |
| `page` | number | 页码，默认 1 |
| `page_size` | number | 每页数量，默认 20 |

响应：`200 OK`

```json
{
  "items": [
    {
      "id": "string | null",
      "title": "string",
      "author": "string",
      "narrator": "string | null",
      "cover_url": "string | null",
      "intro": "string | null",
      "tags": ["string"],
      "genre": "string | null",
      "subtitle": "string | null",
      "published_year": null,
      "published_date": "string | null",
      "publisher": "string | null",
      "isbn": "string | null",
      "asin": "string | null",
      "language": "string | null",
      "explicit": null,
      "abridged": null,
      "duration": null,
      "source_url": null,
      "score": null,
      "chapter_title_template": null,
      "chapter_titles": []
    }
  ],
  "total": null,
  "has_more": null,
  "page": 1,
  "page_size": 20
}
```

`items` 返回书籍搜索结果；未知作者为 `""`，可空字段显式为 `null`，未知数组为 `[]`。`published_year` 为年份字符串或 `null`，`duration` 为非负秒数或 `null`，`score` 范围为 0–1。`total` 和 `has_more` 分别表示已知总数和下一页状态，未知时为 `null`。插件接收与返回的字段和限额见 [元数据搜索接口](../plugins/capabilities.md#元数据搜索接口)。

## GET /api/v1/scraper/sources

获取可用的刮削数据源列表。

响应：`200 OK`

```json
{
  "sources": [
    {
      "id": "ximalaya-scraper-wasm@2.0.0",
      "name": "ximalaya scraper",
      "description": "从喜马拉雅获取有声书元数据（WASM 实现）",
      "version": "2.0.0",
      "enabled": true,
      "auto_scrape": true,
      "aggregate_auto_scrape": false,
      "search_fields": [
        {
          "key": "title",
          "label": "书名",
          "label_i18n": {
            "zh": "书名",
            "en": "Title"
          },
          "required": true,
          "type": "text",
          "placeholder": "输入书名",
          "placeholder_i18n": {
            "zh": "输入书名",
            "en": "Enter title"
          },
          "default_from": "book.title"
        }
      ],
      "result_fields": ["title", "author", "cover_url", "intro", "tags"],
      "result_field_labels": {
        "title": {
          "zh": "书名",
          "en": "Title"
        },
        "author": {
          "zh": "作者",
          "en": "Author"
        },
        "cover_url": {
          "zh": "封面",
          "en": "Cover"
        },
        "intro": {
          "zh": "简介",
          "en": "Description"
        },
        "tags": {
          "zh": "标签",
          "en": "Tags"
        }
      }
    }
  ]
}
```

## POST /api/v1/scraper/search

使用刮削器搜索书籍。

请求体：

```json
{
  "query": "string",
  "search_params": {
    "title": "书名",
    "author": "作者"
  },
  "source": "ximalaya-scraper-wasm@2.0.0",
  "page": 1,
  "page_size": 20,
  "author": "string",
  "narrator": "string"
}
```

说明：

- `query` 可选，作为缺省的 `search_params.title`；已经提供的同名搜索参数优先。
- `title`、`author`、`narrator` 转为插件请求的同名字段，其他 `search_params` 进入 `filters`。
- `source` 可选，填写刮削源列表返回的 `id`。不传时选择可用的普通刮削源；使用聚合源时由该源的声明与配置决定如何聚合。
- `author`、`narrator` 会补入对应搜索参数。

响应：`200 OK`

```json
{
  "items": [],
  "total": null,
  "has_more": null,
  "page": 1,
  "page_size": 20
}
```
