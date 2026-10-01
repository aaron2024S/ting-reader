# Host 能力与资源接口

宿主通过 `PluginHostGateway` 执行插件请求，调用身份由服务端绑定：插件实例、generation、当前用户、声明权限和资源作用域均不能从业务 JSON 中伪造。后台 JS 使用 `Ting.host.invoke(method, params)`；Rust WASM/Native 使用 SDK 的 `Host::invoke` 或 `host_call`。Web/Flutter `web_container` 仅能转发其 `render.bridge.host_methods` 白名单内的调用，后端还会检查登录身份和签名的 `ui_grant`。

## 授权目录

| 方法 | 清单权限 | 额外约束 |
| --- | --- | --- |
| `http.request` | `network_access.domain` | 每跳检查授权域名与实际出站地址，正文返回限定长度的资源 ID |
| `books.list/get` | `books_read` 或 `books_write` | 只读当前用户可访问的书籍 |
| `libraries.list/get` | `libraries_read` | 只读当前用户可访问的库 |
| `books.update` | `books_write` | 写操作按用户/管理员策略校验 |
| `chapters.list/get` | `chapters_read` 或 `chapters_write` | 所属书籍授权 |
| `chapters.update` | `chapters_write` | 所属书籍授权 |
| `libraries.update` | `libraries_write` | 管理员授权 |
| `progress.recent` | `progress_read` | 当前用户 |
| `media.get_url/get_signed_url` | `media_read_url` | 所属书籍授权；生成短期地址 |
| `plugin_routes.sign/revoke` | `plugin_route_sign` / `plugin_route_revoke` | 仅当前插件声明的路由；撤销持久化并立即阻断旧签名 |
| `metadata.write` | `metadata_write` | 管理员和库范围校验 |
| `library.file.list/stat/open` | `file_read.path` | 存储库及相对路径限制；`open` 返回资源 ID |
| `assets.commit` | `file_write.path` | 仅宿主创建的暂存资源可提交 |
| `files.open/stat/list/remove` | `file_read.path` / `file_write.path` | 仅 `plugin-data/<plugin_id>` 或 `plugin-temp/<plugin_id>`；拒绝绝对路径、穿越和符号链接逃逸 |
| `config.get` | `config_read` | 当前插件实例配置快照；敏感字段按配置契约处理 |
| `storage.get/set/delete/list` | `storage_read` / `storage_write` | 插件实例与用户双重分区；与可清缓存分离 |
| `capabilities.invoke` | `capability_invoke` | 必须精确声明目标插件、能力和操作；不转移资源句柄，不允许递归 |
| `tasks.create/get/cancel/report_progress` | `task_create` / `task_read` / `task_manage` / `task_progress` | 任务绑定创建插件和用户；保留核心任务类型 |
| `events.publish` | `event_publish` | 仅发布自身命名空间；事件处理器通过 `event_handler` 声明 |
| `html.parse/select/text/attr/extract/close` | `html_parse` | 2 MiB 输入、句柄和查询上限；纯解析，不执行脚本、不请求外部资源 |
| `cache.get/has` | `cache_read` 或 `cache_write` | 插件隔离的命名空间 |
| `cache.set/delete` | `cache_write` | 插件隔离的命名空间 |
| `playlists.list/get` | `playlists_read` 或 `playlists_write` | 当前用户 |
| `playlists.create/update/delete/add_item/remove_item` | `playlists_write` | 当前用户 |
| `favorites.list` | `favorites_read` 或 `favorites_write` | 当前用户 |
| `favorites.add/remove` | `favorites_write` | 当前用户 |
| `user_settings.get` | `user_settings_read` 或 `user_settings_write` | 当前用户 |
| `user_settings.set` | `user_settings_write` | 当前用户 |

`http.request` 可在无用户的受限系统调用中使用；其余业务方法要求用户 principal。单有清单权限不足以读取其他用户的内容。未知方法、未声明权限、无用户身份及越权资源会失败；错误不要将文件路径或敏感 URL 透传给插件。

## 宿主资源

资源接口提供 `resources.stat/read_at/write_at/create_output/finish/close/create_session/check_session/close_session` 等操作。Rust 使用 `Host::read_at/write_at/chunk_create/chunk_copy` 处理二进制数据，结构化资源请求类型通过 `ting_plugin_sdk::contract::resources` 导入；JavaScript 使用 `Ting.resources` 或从 `./sdk.mjs` 导入 `resources`。资源 ID、chunk lease 和 session ID 只属于颁发它们的插件实例、generation、用户和作用域；读取时限制范围和 256 KiB 单块大小，写回必须写入 staging 输出，成功后由宿主校验并提交。调用者应释放块、关闭临时资源，取消后不继续使用句柄。

网络获取可调用 SDK `http_request_response`，它保留源站状态码，限制最多 8 MiB 响应字节并关闭响应资源；大媒体文件应使用宿主按 Range 提供的资源读操作，避免整文件读入内存。JavaScript 可以通过包内 `sdk.mjs` 的 `resources` / `readResource` 操作受控资源。

## 客户端 UI 桥接

`GET /api/v1/plugin-capabilities` 为可见的 `ui_extension` 返回 `client_grant`。客户端转发 `POST /api/v1/plugin-host/invoke` 或 capability 调用时必须提供 `ui_capability_id`、匹配的 `ui_grant`、目标插件 ID 与方法；插件页面不能直接取得用户登录凭据。grant 有效期和上下文由后端重新校验；插件只能访问当前 UI 声明的 `bridge.host_methods` 和 `bridge.capabilities`。资产地址中的 grant 同样是临时秘密，不得记录或外传。

能力与固定操作见[能力声明](./capabilities.md)，业务实现见[插件开发指南](./plugin-dev.md#4-实现业务)，页面调用示例见[页面桥接](./ui-logging-migration.md#4-页面桥接)。
