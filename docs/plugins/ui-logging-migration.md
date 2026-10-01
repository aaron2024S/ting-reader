# 插件界面与结构化日志

本文说明插件如何声明客户端入口、在 Web 和 Flutter 中展示页面、调用宿主，以及记录和查看日志。项目创建、编译、签名和安装见[插件开发指南](./plugin-dev.md)，完整能力和权限见[能力声明](./capabilities.md)及 [HostGateway](./hostgateway.md)。

## 1. 页面与业务代码的位置

插件业务入口在服务端的 JavaScript、WASM 或 Native 运行时执行；`ui/` 中的 HTML、CSS 和浏览器 JavaScript 在客户端容器中执行。页面通过客户端桥接调用插件业务能力或宿主方法。

服务端 JavaScript 中的 `Ting.host`、`Ting.config`、`Ting.resources` 和 `Ting.log` 由运行时提供。客户端页面使用 `window.__TING_PLUGIN_BRIDGE__`，页面中的 `console.log` 只属于客户端调试输出，不会自动成为服务端插件日志。

## 2. 声明界面入口

| 字段 | 含义 |
| --- | --- |
| `id` | 当前插件内唯一的能力 ID |
| `kind: ui_extension` | 客户端界面能力 |
| `slots` | 入口位置：`app.sidebar_page`、`global.floating_action`、`global.panel`、`book.detail_action` |
| `contexts` | 声明适用上下文：`global`、`book`、`reader` |
| `title` | 入口名称，可使用 `zh` / `en` 多语言对象 |
| `icon` | 可选图标名称，例如 `message-circle` |
| `priority` | 可选排序优先级 |
| `render.mode` | `web_container` 展示插件页面；`action` 调用插件操作并展示执行结果 |
| `render.entry` | `web_container` 的包内 HTML 入口 |
| `render.bridge.capabilities` | 页面可调用的同插件其他能力 ID |
| `render.bridge.host_methods` | 页面可直接调用的宿主方法名称 |

UI 能力的固定操作为 `open`，业务入口必须实现该操作。`action` 点击时调用 `open`；`web_container` 打开时首先加载 HTML、初始化页面桥接，页面需要业务数据时主动发送请求。

下面是一份可以用于联调的完整清单。插件自身版本与最低主程序版本分别填写。

```yaml
id: example-panel
name: Example Panel
version: 1.0.0
min_core_version: 2.0.0
author: Example Author
description: { zh: 插件面板示例, en: Plugin panel example }
runtime: javascript
entry_point: plugin.js
capabilities:
  - id: panel.view
    kind: ui_extension
    slots: [app.sidebar_page, global.floating_action]
    contexts: [global, book, reader]
    title: { zh: 示例面板, en: Example Panel }
    icon: message-circle
    render:
      mode: web_container
      entry: ui/panel.html
      bridge:
        capabilities: [panel.tools]
        host_methods: [user_settings.get]
  - id: panel.tools
    kind: tool_provider
    invoke: invokeTool
    tools:
      - name: panel.echo
        description: { zh: 返回输入文本, en: Return the input text }
        input_schema:
          type: object
          properties:
            message: { type: string, maxLength: 2000 }
          required: [message]
          additionalProperties: false
        output_schema:
          type: object
          properties:
            message: { type: string }
          required: [message]
          additionalProperties: false
        side_effects: false
permissions:
  - type: user_settings_read
```

`bridge` 是页面调用白名单，`permissions` 是插件申请的宿主权限，两者都需要满足。上例的页面只能直接读取用户设置；业务工具如需读取书籍，还应声明 `books_read`。请求最终仍受到当前用户的资源访问权限限制。

## 3. 服务端业务入口

在 `plugin.js` 中实现清单声明的 `open` 和 `invokeTool` 操作：

`plugin.js`：

```js
import { success } from './sdk.mjs';

export async function open(request) {
  return success({ context: request?.context ?? null });
}

export async function invokeTool(request) {
  if (request.tool_name !== 'panel.echo') {
    throw new Error('Unknown tool');
  }
  const message = request.params.message;
  Ting.log.info('Echo completed', {
    op: 'panel.echo',
    message_length: message.length,
  });
  return success({ message });
}
```

工具调用参数使用 `tool_name` 和 `params`；工具的输入、输出接受清单 JSON Schema 校验。SDK `success` 生成调用结果信封，客户端桥接的 `result` 是经过宿主处理后的业务结果。

## 4. 页面桥接

### 初始化与消息格式

页面注册 `message` 监听器后，会收到 `ting-plugin:init`，包含：

| 字段 | 含义 |
| --- | --- |
| `pluginId` / `pluginName` | 当前插件身份及显示名称 |
| `capabilityId` | 当前 UI 能力 ID |
| `slot` / `contexts` | 当前入口及声明的上下文类型 |
| `context` | 客户端提供的当前页面上下文；具体字段取决于入口 |
| `theme` | 当前主题、明暗模式和 CSS 变量 |
| `bridgeToken` | 当前页面实例的桥接令牌 |

页面收到的消息由容器转发到当前窗口，监听时检查 `event.source === window`。`context` 可用于展示和提出业务请求，服务端仍自行判断用户身份与资源权限。

请求使用：

```js
window.__TING_PLUGIN_BRIDGE__.postMessage({
  type: 'ting-plugin:request',
  bridge_token: bridgeToken,
  id: requestId,
  method: 'capability.invoke',
  params: {
    capabilityId: 'panel.tools',
    params: {
      tool_name: 'panel.echo',
      params: { message: 'Hello' },
    },
  },
});
```

直接调用宿主方法时，最外层 `method` 为 `host.invoke`：

```js
request('host.invoke', {
  method: 'user_settings.get',
  params: {
    key: 'language',
  },
});
```

响应的 `type` 为 `ting-plugin:response`，携带对应 `id`、`bridge_token` 和 `ok`；成功读取 `result`，失败读取 `error`。页面应设置超时、释放已完成请求，并在关闭时清理未完成请求。页面超时只结束本地等待，不代表服务端操作已经取消；写操作重试需要业务层避免重复执行。

### 可运行页面示例

`ui/panel.html`：

```html
<!doctype html>
<html lang="zh-CN">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>示例面板</title>
  <link rel="stylesheet" href="./panel.css">
</head>
<body>
  <main>
    <h1>示例面板</h1>
    <p id="status">等待宿主初始化…</p>
    <input id="message" maxlength="2000" value="Hello">
    <button id="send" disabled>发送</button>
    <pre id="result"></pre>
  </main>
  <script src="./panel.js"></script>
</body>
</html>
```

`ui/panel.css`：

```css
body {
  margin: 0;
  background: var(--bg, #f8fafc);
  color: var(--text, #0f172a);
  font: 14px/1.5 system-ui, sans-serif;
}
main { padding: 16px; }
input, button { font: inherit; }
pre { white-space: pre-wrap; overflow-wrap: anywhere; }
```

`ui/panel.js`：

```js
(() => {
  let bridgeToken = null;
  let sequence = 0;
  const pending = new Map();
  const status = document.getElementById('status');
  const result = document.getElementById('result');
  const send = document.getElementById('send');

  function rejectPending(message) {
    for (const item of pending.values()) {
      clearTimeout(item.timer);
      item.reject(new Error(message));
    }
    pending.clear();
  }

  function request(method, params) {
    if (!bridgeToken) return Promise.reject(new Error('Host not ready'));
    const id = String(++sequence);
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        pending.delete(id);
        reject(new Error('Request timed out'));
      }, 30000);
      pending.set(id, { resolve, reject, timer });
      try {
        window.__TING_PLUGIN_BRIDGE__.postMessage({
          type: 'ting-plugin:request',
          bridge_token: bridgeToken,
          id,
          method,
          params,
        });
      } catch (error) {
        clearTimeout(timer);
        pending.delete(id);
        reject(error);
      }
    });
  }

  window.addEventListener('message', (event) => {
    if (event.source !== window || !event.data) return;
    const data = event.data;
    if (data.type === 'ting-plugin:init') {
      rejectPending('Host context changed');
      bridgeToken = data.bridgeToken;
      send.disabled = false;
      status.textContent = data.pluginName;
      request('host.invoke', {
        method: 'user_settings.get',
        params: { key: 'language' },
      }).then((value) => {
        status.textContent = `语言：${value?.value ?? '未设置'}`;
      }).catch((error) => {
        status.textContent = error.message;
      });
      return;
    }
    if (data.type !== 'ting-plugin:response' ||
        data.bridge_token !== bridgeToken) return;
    const item = pending.get(data.id);
    if (!item) return;
    clearTimeout(item.timer);
    pending.delete(data.id);
    if (data.ok) item.resolve(data.result);
    else item.reject(new Error(data.error || 'Request failed'));
  });

  send.addEventListener('click', async () => {
    send.disabled = true;
    try {
      const value = await request('capability.invoke', {
        capabilityId: 'panel.tools',
        params: {
          tool_name: 'panel.echo',
          params: { message: document.getElementById('message').value },
        },
      });
      result.textContent = value.message;
    } catch (error) {
      result.textContent = error.message;
    } finally {
      send.disabled = !bridgeToken;
    }
  });

  window.addEventListener('pagehide', () => {
    bridgeToken = null;
    send.disabled = true;
    rejectPending('Page closed');
  });
})();
```

宿主会应用 `theme.cssVariables`、`data-ting-theme` 和 `color-scheme`，页面可直接使用这些 CSS 变量。需要重绘图表等组件时，再监听 `ting-plugin:theme` 处理主题变化。

### 资产与授权

- 将 HTML、浏览器脚本、样式和图片放在插件包内，使用相对地址。上例使用普通 `<script src>`，可以同时用于 Web 和 Flutter；前端框架源码应先构建成浏览器可执行的普通脚本，例如 IIFE。
- Web 容器当前不支持 `<script type="module">`。宿主会应用 sandbox 和 CSP，页面网络请求受限；第三方 CDN、远程模块、CSS `@import` 和 CSS `url(...)` 不作为跨客户端页面的依赖方案。
- 页面通过桥接取得业务数据。客户端持有登录凭据和 `client_grant`，并负责转发请求，页面无需拼装后端认证请求。
- `bridgeToken` 只在当前页面实例内使用。`client_grant` 转发为 `ui_grant`，服务端核对当前用户、插件、来源 UI 能力和调用白名单。两类令牌、带授权的资产地址都不能写入日志或持久存储。
- 外链使用用户实际点击的 HTTP/HTTPS `<a>`，由容器处理确认与打开；程序自动导航、`window.open`、表单跳转不属于页面桥接调用方式。
- Web 桥接单条消息最大 256 KiB，每 10 秒最多 100 次请求。Flutter 同样校验消息大小、身份和白名单；页面应使用小型 JSON 请求、分页和按需加载。

## 5. 结构化日志

### JavaScript 业务日志

服务端 JavaScript 支持：

```js
const eventId = Ting.log.info('Metadata search completed', {
  op: 'metadata.search',
  duration_ms: 184,
  result_count: 12,
});

Ting.log.debug('Cache checked', { op: 'metadata.search', cache_hit: true });
Ting.log.warn('Upstream rate limited', { op: 'metadata.search', status: 429 });
Ting.log.error('Search failed', { op: 'metadata.search', error_code: 'UPSTREAM_ERROR' });
```

`debug/info/warn/error(message, fields)` 的 `fields` 为可选 JSON 对象，不能传数组。调用返回宿主生成的事件 ID，可用于定位单条日志；各日志事件有各自的 ID。

| 字段 | 来源 |
| --- | --- |
| `event_id` | 宿主生成 |
| `plugin_id`、`plugin_instance_id`、`plugin_version`、`plugin` | 宿主绑定的插件身份 |
| `runtime` | 运行时类型 |
| `source` | 宿主区分的日志来源 |
| `op` | 插件传入的 `fields.op`，用于标识业务操作 |
| `plugin_fields` | 插件传入的结构化业务字段 |

`source` 为 `code`、`lifecycle`、`runtime`、`gateway` 或 `security`。插件只填写业务字段；身份、权限和来源由宿主确定。日志级别还受服务端日志过滤配置控制，未启用 `debug` 时不会保存调试级事件。

记录耗时、数量、缓存命中、状态码和稳定错误码即可。密钥、Cookie、Authorization、授权令牌、带签名的 URL、完整请求体和用户文本不应作为日志字段。

WASM 和 Native 的加载、调用异常及生命周期日志由宿主记录。业务调用返回稳定错误码和错误信息，配合宿主日志定位问题。

### 查看与导出

管理员可在插件管理页查看日志，或调用：

| API | 用途 |
| --- | --- |
| `GET /api/v1/plugins/{id}/logs` | 查询指定插件日志 |
| `GET /api/v1/plugins/{id}/logs/export` | 导出指定插件日志 |
| `GET /api/v1/plugin-logs` | 查询全部插件日志 |
| `GET /api/v1/plugin-logs/export` | 导出全部插件日志 |

查询参数支持 `plugin_id`（全局查询）、`level`、`source`、`q`、`since`、`until`、`page`、`page_size`。时间使用 RFC 3339；默认每页 100 条，最大 500 条。`q` 可检索消息和结构化字段，便于通过 `op` 或 `event_id` 定位。

## 6. 构建与联调

1. 用 `trpack new example-panel --template ui --runtime javascript --version 1.0.0` 创建独立项目，按上例补充清单、业务入口和页面文件。
2. 执行 `trpack validate example-panel`，再用稳定私钥运行 `trpack build`，最后 `trpack inspect` 和 `trpack verify`。具体命令见[开发流程](./plugin-dev.md)。
3. 在测试服务端的插件管理页上传 `.tr` 包，完成发布者确认和配置，确认插件初始化成功。
4. 分别从 Web 和 Flutter 打开入口，验证初始化、用户语言、主题、页面资产和业务请求；测试未列入白名单的方法被拒绝。
5. 查看服务端插件日志，确认 `plugin_id`、`runtime`、`source`、`op` 和事件 ID 可定位本次调用，日志不含敏感信息。
6. 测试切换用户、关闭再打开页面、重新加载插件和升级后重新打开。页面不沿用已关闭实例的桥接令牌；请求等待和资源随页面生命周期释放。

`trpack verify` 验证包的结构、文件哈希和签名，并不能代替客户端桥接联调，也不能替宿主决定是否信任发行者。
