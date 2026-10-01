# Ting Reader API 文档

版本：v1  
基础 URL：`http://<host>:<port>`

## 模块索引

| 模块 | 文件 | 说明 |
| --- | --- | --- |
| 认证 | [auth.md](auth.md) | 注册、登录、JWT Token |
| 用户 | [users.md](users.md) | 当前用户、用户管理、个性化设置 |
| 播放进度 | [progress.md](progress.md) | 最近收听、清空历史、更新播放进度 |
| 阅读与书签 | [reading.md](reading.md) | 已读/未读、分页历史、书签与备注 |
| 收藏 | [favorites.md](favorites.md) | 收藏管理 |
| 书单 | [playlists.md](playlists.md) | 我的书单、作品排序与管理 |
| 媒体库 | [libraries.md](libraries.md) | 媒体库 CRUD、扫描、WebDAV 测试 |
| 系列 | [series.md](series.md) | 系列 CRUD |
| 书籍 | [books.md](books.md) | 书籍 CRUD、章节管理、刮削、合并 |
| 搜索与刮削 | [search.md](search.md) | 本地搜索、在线刮削、刮削源 |
| 插件 | [plugins.md](plugins.md) | 安装、配置、能力发现与调用、Host、UI 资产、路由和日志 |
| 任务 | [tasks.md](tasks.md) | 异步任务管理 |
| 媒体流 | [media.md](media.md) | 音频流、HLS、封面代理、缓存 |
| 系统 | [system.md](system.md) | 健康检查、统计报表、指标、配置、日志 |
| 通知与事件 | [notifications.md](notifications.md) | Webhook 事件、自定义请求头、Body 模板与测试发送 |
| 工具 | [tools.md](tools.md) | 正则生成等工具接口 |
| WebSocket | [websocket.md](websocket.md) | 实时播放进度同步 |
| 错误处理 | [errors.md](errors.md) | 错误格式与状态码 |

## 通用约定

### URL 前缀

大部分新接口同时支持 `/api/...` 和 `/api/v1/...` 两种前缀。文档中优先写 `/api` 路径；仅 v1 路径存在的接口会明确写 `/api/v1`。

### 鉴权方式

除公共接口外，请求需要携带 JWT：

```http
Authorization: Bearer <token>
```

### 认证流程

1. 调用 `POST /api/auth/login` 获取 Token。
2. 后续 HTTP 请求在 Header 中携带 `Authorization: Bearer <token>`。
3. WebSocket 通过 `?token=<token>` 参数认证。

### 权限模型

- 公共入口：`/api/health`、`/api/stats`、认证接口、HLS 播放列表与分片。公开媒体、插件路由和 UI 资产仍按各自接口检查签名或访问凭据；WebSocket 握手检查用户 Token。
- 用户接口：普通登录用户可访问，例如播放进度、收藏、书单、个性化设置。
- 管理员接口：需要 `role = admin`，例如媒体库写操作、用户管理、系统日志、数据统计、通知与事件、插件安装/卸载/配置写入、插件日志和缓存管理。插件列表及可见实例的详情按当前用户过滤。

### 事件与统计

清除历史默认只隐藏可见记录；设置 `clear_progress: true` 可同步清除进度。系统统计报表依赖独立收听事件表，因此不会因用户清空历史或进度而回退。
