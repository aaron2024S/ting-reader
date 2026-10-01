# 系统管理

除“公共接口”外，本页接口均需要登录；系统管理接口通常需要管理员权限。

## 公共接口

### GET /api/health

健康检查，无需认证。

响应：`200 OK`

```json
{
  "status": "healthy",
  "components": {
    "database": {
      "status": "healthy",
      "message": "Database is operational",
      "details": {
        "status": "connected"
      }
    },
    "plugin_system": {
      "status": "healthy",
      "message": "Plugin system is operational",
      "details": {
        "total_plugins": 0,
        "active_plugins": 0,
        "failed_plugins": 0
      }
    }
  },
  "timestamp": "RFC3339",
  "version": "2.0.0"
}
```

### GET /api/stats

公共馆藏统计，无需认证。

响应：`200 OK`

```json
{
  "total_books": 0,
  "total_chapters": 0,
  "total_duration": 0,
  "last_scan_time": "RFC3339 | null"
}
```

## 管理员统计报表

### GET /api/system/statistics

获取后台数据统计报表。普通用户不可访问。

响应：`200 OK`

```json
{
  "overview": {
    "total_books": 0,
    "total_chapters": 0,
    "total_duration": 0,
    "total_libraries": 0,
    "local_libraries": 0,
    "webdav_libraries": 0,
    "rss_libraries": 0,
    "total_users": 0,
    "admin_users": 0,
    "active_users": 0,
    "total_progress_records": 0,
    "total_listen_seconds": 0.0
  },
  "library_breakdown": [
    {
      "id": "string",
      "name": "string",
      "library_type": "local | webdav | rss",
      "total_books": 0,
      "total_chapters": 0,
      "total_duration": 0,
      "last_scanned_at": "RFC3339 | null"
    }
  ],
  "user_activity": [
    {
      "id": "string",
      "username": "string",
      "role": "admin | user",
      "listened_books": 0,
      "progress_records": 0,
      "listen_seconds": 0.0,
      "last_active_at": "RFC3339 | null"
    }
  ],
  "recent_activity": [
    {
      "date": "YYYY-MM-DD",
      "active_users": 0,
      "progress_updates": 0,
      "listen_seconds": 0.0
    }
  ],
  "top_books": [
    {
      "id": "string",
      "title": "string | null",
      "author": "string | null",
      "library_id": "string",
      "library_name": "string | null",
      "listeners": 0,
      "progress_updates": 0,
      "listen_seconds": 0.0
    }
  ],
  "generated_at": "RFC3339"
}
```

说明：

- 统计使用 `listening_events`，用户清空最近收听不会影响后台历史统计；每日活动默认保留 90 天。
- `overview.total_progress_records` 和 `user_activity.progress_records` 表示当前 `progress` 表中的实际进度行数，不是心跳同步次数。
- `recent_activity` 默认返回最近 14 天有记录的活动点；`progress_updates` 为“用户 × 书籍 × 章节 × 日期”的活跃聚合记录数，不是 2 秒心跳次数。热门作品使用最近 90 天的活跃聚合记录。
- `top_books` 当前最多返回 12 条热门作品，只包含累计实际收听时长大于 0 的作品；不足 12 条时按实际数量返回。

## 系统指标

### GET /api/system/metrics

获取系统指标。支持 JSON 和 Prometheus 文本格式。

请求头：

| Header | 说明 |
| --- | --- |
| `Accept` | `application/json` 或 `text/plain` |

响应：`200 OK`

```json
{
  "system": {
    "total_requests": 0,
    "avg_response_time_ms": 0.0,
    "total_errors": 0,
    "error_rate": 0.0,
    "uptime_seconds": 0
  },
  "plugins": [
    {
      "plugin_id": "string",
      "plugin_name": "string",
      "total_calls": 0,
      "successful_calls": 0,
      "failed_calls": 0,
      "success_rate": 0.0,
      "min_execution_time_ms": null,
      "max_execution_time_ms": null,
      "avg_execution_time_ms": null,
      "p95_execution_time_ms": null,
      "memory_usage_bytes": null,
      "peak_memory_bytes": null,
      "error_distribution": {}
    }
  ],
  "task_queue": {
    "queued_tasks": 0,
    "running_tasks": 0,
    "completed_tasks": 0,
    "failed_tasks": 0,
    "cancelled_tasks": 0,
    "total_tasks": 0,
    "avg_processing_time_ms": 0.0,
    "failure_rate": 0.0
  },
  "database": {
    "active_connections": 0,
    "idle_connections": 0,
    "total_queries": 0,
    "avg_query_time_ms": 0.0
  },
  "timestamp": "RFC3339"
}
```

## 系统配置

### GET /api/system/config

获取系统配置。

响应：`200 OK`

```json
{
  "server": {
    "host": "0.0.0.0",
    "port": 3000,
    "max_connections": 100,
    "request_timeout": 30
  },
  "database": {
    "path": "string",
    "connection_pool_size": 5,
    "busy_timeout": 5000
  },
  "plugins": {
    "plugin_dir": "string",
    "enable_hot_reload": false,
    "max_memory_per_plugin": 536870912,
    "max_execution_time": 300
  },
  "task_queue": {
    "max_concurrent_tasks": 2,
    "default_retry_count": 3,
    "task_timeout": 3600
  },
  "logging": {
    "level": "info",
    "format": "text | json",
    "output": "stdout | file",
    "log_file": "string | null",
    "max_file_size": 10485760,
    "max_backups": 5
  },
  "security": {
    "enable_auth": true,
    "api_key": "***",
    "allowed_origins": ["*"],
    "rate_limit_requests": 100,
    "rate_limit_window": 60,
    "enable_hsts": false,
    "hsts_max_age": 31536000
  },
  "storage": {
    "data_dir": "string",
    "temp_dir": "string",
    "local_storage_root": "string",
    "local_library_roots": ["string"],
    "max_disk_usage": 50
  }
}
```

说明：

- `plugins.max_memory_per_plugin` 单位为字节，`plugins.max_execution_time` 单位为秒；两项均须大于 0。
- `local_storage_root` 是旧版默认本地库根目录，仍用于兼容相对路径库和 Docker 默认 `/app/storage`。
- `local_library_roots` 可额外配置多个允许作为本地媒体库的根目录；配置后需要重启服务生效。

### PUT /api/system/config

更新系统配置。请求体中所有字段均可选。

请求体示例：

```json
{
  "server": {
    "host": "0.0.0.0",
    "port": 3000
  },
  "security": {
    "enable_auth": true,
    "api_key": "new-key"
  }
}
```

响应：`200 OK`

```json
{
  "message": "Configuration updated successfully. 2 parameter(s) require system restart to take effect.",
  "updated_fields": ["server.host", "server.port"],
  "requires_restart": ["server.host", "server.port"]
}
```

## 系统更新

### GET /api/system/check-update

检查服务端更新。

响应：`200 OK`

返回更新服务的原始 JSON，例如：

```json
{
  "version": "v1.4.4",
  "download_url": "https://...",
  "size": "string",
  "date": "RFC3339"
}
```

## 系统日志

### GET /api/system/logs

获取系统日志与任务日志。仅当前 Ting Reader 实例的管理员可访问；普通用户和外部插件作者没有日志读取权限。

查询参数：

| 参数 | 类型 | 说明 |
| --- | --- | --- |
| `level` | string | 日志级别过滤，如 `INFO`、`WARN`、`ERROR` |
| `module` | string | 模块过滤，如 `audit`、`audit::login`、`audit::playback`、`audit::scan`、`audit::notification`、`all` |
| `plugin_id` | string | 插件稳定 ID 或 `id@version`；匹配时会忽略版本差异 |
| `source` | string | 插件日志来源，如 `code`、`lifecycle`、`runtime`、`gateway`、`security` |
| `q` | string | 在消息、原始消息、模块和结构化字段中做不区分大小写的关键字搜索 |
| `since` | RFC3339 string | 起始时间，包含边界 |
| `until` | RFC3339 string | 结束时间，包含边界；不能早于 `since` |
| `page` | number | 页码，默认 `1` |
| `page_size` | number | 每页数量，默认 `50` |

响应：`200 OK`

```json
{
  "logs": [
    {
      "timestamp": "RFC3339",
      "level": "INFO",
      "module": "audit::login",
      "message": "用户 'admin' 登录成功",
      "fields": {
        "user_id": "string",
        "username": "admin",
        "real_ip": "127.0.0.1",
        "user_agent": "Mozilla/5.0 ...",
        "device": "Windows / Chrome"
      },
      "task_id": "string | null",
      "task_status": "queued | running | completed | failed | cancelled | null",
      "task_type": "string | null"
    }
  ],
  "total": 0,
  "page": 1,
  "page_size": 50
}
```

说明：

- 登录日志会记录 `real_ip`、`user_agent`、`device`。
- 扫描完成日志会记录媒体库 ID、名称、类型、路径、新增/更新/删除数量。
- `fields` 为结构化日志字段，不含 `message`。
- 插件日志在同一个 `system.json` 及轮转文件中保存，不为插件创建可自行读取的独立日志文件。
- 插件日志由宿主绑定 `event_id`、`plugin_id`、`plugin_instance_id`、`plugin_version`、`runtime` 和 `source`；插件传入的业务字段保存在 `fields.plugin_fields`，其中 `op` 会提升到顶层便于检索。

### DELETE /api/system/logs

清空系统日志文件。仅管理员可调用。

响应：`200 OK`

```json
{
  "message": "System logs cleared successfully"
}
```

### GET /api/system/logs/export

导出系统日志文本。仅管理员可调用；导出文件可能包含实例运行信息，提供给插件作者排查前应由管理员主动脱敏。

查询参数：

| 参数 | 类型 | 说明 |
| --- | --- | --- |
| `level` | string | 可选，仅导出指定级别 |

响应：`200 OK`，`text/plain` 文件下载。

## 通知与事件

Webhook 通知管理接口见 [notifications.md](notifications.md)。
