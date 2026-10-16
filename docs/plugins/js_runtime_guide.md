# JavaScript 插件运行时

使用 `trpack new my-plugin --template metadata --runtime javascript` 创建项目，在 `plugin.js` 中实现业务。包内代码采用 ESM，操作由具名导出提供；清单声明的操作必须在入口中导出。

创建、构建、打包和安装步骤见[插件开发指南](./plugin-dev.md)。

```yaml
id: example-metadata-js
name: Example Metadata JS
version: 2.0.0
min_core_version: 2.0.0
runtime: javascript
entry_point: plugin.js
author: Example
description: { zh: 示例, en: Example }
capabilities:
  - id: metadata.search
    kind: metadata_provider
    operations: [search]
    search_fields:
      - key: title
        label: { zh: 书名, en: Title }
        type: text
        required: true
    result_fields:
      - key: title
        label: { zh: 书名, en: Title }
permissions: []
```

```js
import { success, publishSearch } from './sdk.mjs';

export async function search(request) {
  const items = request.title ? [{ id: null, title: request.title }] : [];
  return success(publishSearch({ items, total: null, has_more: null }, request));
}
```

`SearchRequest` 使用 `title/author/narrator/page/page_size/filters`，可选 `chapter_candidates` 和 `context`；字段、可空值和限额见[元数据搜索接口](./capabilities.md#元数据搜索接口)。平台原始数据经 `publishSearch` 转为 `SearchPage` 后再通过 `success` 返回。其他操作也必须返回成功或错误信封；结果和资源接口的类型声明可在 `sdk.d.ts` 中查看。

模块使用签名插件包内的相对 `.js` / `.mjs` 地址，例如 `import { parse } from './parse.js'`。构建时将第三方依赖打包进插件目录，发布为可直接执行的 ESM 模块。运行时的模块加载范围为当前插件包，清单字段按[能力声明](./capabilities.md)填写。

可用的宿主入口包括 `Ting.host.invoke(method, params)`、`Ting.resources`、内置的 `fetch` 和 `Ting.log.debug/info/warn/error(message, fields)`。Host 方法按声明权限和当前用户授权执行；资源 ID 带插件实例、用户及调用作用域，不应缓存供另一个用户使用。网络请求仅允许授权域名，重定向仍复核权限；HTTP 响应和二进制媒体资源使用宿主限额。`Ting.config` 包含当前插件的有效配置，不要在日志中记录密钥或完整请求体。权限及参数见[HostGateway](./hostgateway.md)，能力声明见[capabilities](./capabilities.md)。

## 错误处理

Host 和资源调用失败时抛出 `Error`，携带协议 `code`、`details`、`retryable` 以及完整的 `pluginError`。按错误码处理分支，不要匹配 `message` 文本。未捕获的 Host 错误返回宿主时仍保留分类。

业务入口使用 SDK 的 `failure(code, message, context, options)` 返回失败，`code` 使用 `sdk.d.ts` 中的 `PluginErrorCode`。`context` 包含 `plugin_id/capability_id/operation/request_id`，由宿主重新绑定可信身份；`options` 可传入 `details` 和 `retryable`。消息限 512 UTF-8 字节，身份字段各限 128 字节，`details` 限 4096 字节。

部分成功的业务结果若需附带失败原因，可用 SDK 的 `errorInfo(error)` 保留 `code/message/details/retryable`。它是业务数据，不代替操作失败时的协议信封。插件 Web 页面通过[页面桥接](./ui-logging-migration.md)接收 REST 错误对象，错误类型与运行时协议分别处理。

运行 `trpack validate my-plugin`、`trpack build my-plugin --sign-key keys/private.json` 和 `trpack verify <package.tr>`；加载时还会检查具名导出，调用时检查请求和结果。

## 执行和内存预算

所有 JS 插件都使用宿主设置的预算，`permissions: []` 同样创建沙箱并应用限制。管理员的 `plugins.max_memory_per_plugin` 和 `plugins.max_execution_time` 配置同时用于 JS、WASM；默认分别为 512 MiB 和 300 秒，JS 模块加载最多 30 秒。插件清单不能取消或扩大这些限制。

JS 内存预算的四分之三用于 V8 堆，四分之一用于 ArrayBuffer、TypedArray 和宿主 chunk 副本的底层缓冲。接近堆上限时终止实例，并保留最多 16 MiB 的紧急退出空间；这不是整个进程 RSS 的硬上限。Deno 内部编码、序列化等操作不向插件开放，JS 中也不提供第二个 WebAssembly 运行时。WASM 插件应声明 `runtime: wasm`，由 Wasmtime 执行。

模块求值、同步死循环、异步调用、初始化及关闭钩子都受执行预算约束。生命周期钩子返回 Promise 时，宿主等待其完成。超时或内存超限会取消关联 Host 请求及资源，使实例不可再调用并销毁运行时；管理页显示失败原因，需要重新加载恢复。

执行结束撤销监控计时并清空 Host 调用上下文和结果引用。垃圾回收释放缓冲配额；实例销毁时回收 V8 堆、缓冲、回调和监控线程。不要将每次调用的资源或大型返回值长期保留在插件全局变量中。
