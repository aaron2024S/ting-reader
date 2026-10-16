# 错误处理

## 业务错误响应

```json
{
  "error": "ValidationError",
  "message": "Validation error: 请求参数不符合要求",
  "details": {
    "reason": "invalid_value",
    "operation": "create_book::validation",
    "resource": null,
    "context": ["书籍创建"],
    "retryable": false
  },
  "trace_id": "request-trace-id"
}
```

`error`、`message`、`details` 和 `trace_id` 组成统一的错误响应。`message` 用于展示；客户端根据 `error`、HTTP 状态码和 `details.reason` 处理业务分支。`details.operation` 标识失败操作，`details.context` 保留错误上下文，`details.resource` 标识涉及的资源，`details.retryable` 表示是否适合重试。底层 `source` 错误链不直接序列化，网络错误保存原因前会去除请求 URL。

响应正文的 `trace_id` 与响应头 `X-Trace-Id` 一致，反馈问题时提供此值即可关联到服务端请求日志。认证和限流中间件也使用统一错误响应。限流响应仍包含 `Retry-After` 响应头及 `details.retry_after`。

WebSocket 的错误消息保留 `type: "error"`，同时使用相同的 `error/message/details/trace_id` 字段。消息解析或进度保存失败时可按错误类型诊断，不需要解析消息文本。

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
| 409 | 插件报告资源冲突 |
| 428 | 插件安装需要发布者确认，响应结构见 [插件安装](plugins.md#post-apiv1pluginsinstall) |
| 429 | 资源限额或请求频率受限 |
| 500 | 服务端处理错误，包括插件加载或执行失败 |
| 502 | 插件报告无效输出、执行或上游处理错误 |
| 503 | 插件或其依赖暂时不可用 |
| 504 | 插件报告调用超时 |

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

插件错误的 `details.reason` 保留协议错误码，`details.resource` 和 `details.capability_id` 标识插件及能力，`details.retryable` 保留插件声明。宿主校验错误信封后再映射 HTTP 状态。客户端根据状态码和错误类型处理失败，不解析本地化的 `message` 来决定业务分支。插件能力调用成功的 HTTP 响应中，`result` 是已解包的业务数据；协议失败会转为对应的 HTTP 错误响应，不会作为成功的 `result` 返回。
