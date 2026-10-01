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

运行 `trpack validate my-plugin`、`trpack build my-plugin --sign-key keys/private.json` 和 `trpack verify <package.tr>`；加载时还会检查具名导出，调用时检查请求和结果。
