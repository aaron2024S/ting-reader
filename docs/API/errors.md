# 错误处理

## 业务错误响应

```json
{
  "error": "ValidationError",
  "message": "Validation error: 请求参数不符合要求",
  "trace_id": "request-trace-id"
}
```

`error` 是错误类型，`message` 是可读说明，`trace_id` 用于关联服务端日志；存在额外信息时还会返回 `details`。框架层请求解析错误以实际响应和 HTTP 状态码为准。

## HTTP 状态码

| 状态码 | 说明 |
| --- | --- |
| 200 | 成功 |
| 201 | 创建成功 |
| 204 | 成功，无响应正文 |
| 400 | 请求无效、解析或数据验证失败 |
| 401 | 未认证，Token 缺失或过期 |
| 403 | 权限不足或访问范围受限 |
| 404 | 资源或插件不存在 |
| 408 | 执行超时 |
| 428 | 插件安装需要发布者确认，响应结构见 [插件安装](plugins.md#post-apiv1pluginsinstall) |
| 429 | 资源限额或请求频率受限 |
| 500 | 服务端处理错误，包括插件加载或执行失败 |

## 错误类型

| error | 说明 |
| --- | --- |
| `AuthenticationError` | 认证失败 |
| `PermissionDenied` | 权限不足 |
| `SecurityViolation` | 安全或资源访问范围校验失败 |
| `NotFound` | 资源不存在 |
| `ValidationError` | 数据验证失败 |
| `InvalidRequest` | 请求不符合要求 |
| `SerializationError` / `DeserializationError` | 数据编解码失败 |
| `PluginNotFound` | 插件实例不存在 |
| `PluginLoadError` | 插件加载失败 |
| `PluginExecutionError` | 插件执行失败 |
| `DependencyError` | 插件依赖不满足 |
| `ResourceLimitExceeded` | 资源限额超出 |
| `Timeout` | 执行超时 |
| `TaskError` | 任务处理失败 |
| `ExternalError` / `ExternalServiceError` | 外部工具或服务失败 |
| `ConfigError` / `DatabaseError` / `IoError` / `NetworkError` | 配置、数据库、文件或网络处理错误 |

客户端根据状态码和错误类型处理失败，不解析本地化的 `message` 来决定业务分支。插件能力调用成功的 HTTP 响应包含 `result`，其中仍可能是插件错误信封，应同时检查 `result.ok`。
