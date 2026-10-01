# WASM 插件运行时

创建 WASM 项目并编译：

```sh
trpack new my-wasm --template metadata --runtime wasm --id my-wasm
cd my-wasm
rustup target add wasm32-wasip1
cargo build --release --target wasm32-wasip1
```

将 `target/wasm32-wasip1/release/ting_plugin.wasm` 复制到 `plugin.yml` 的 `entry_point`，再执行 `trpack validate`、`trpack build --sign-key`、`trpack verify`。具体命令见[构建与打包流程](./plugin-dev.md#5-检查与构建)。

在 `src/lib.rs` 实现业务，完整例子见 [Rust 实现](./plugin-dev.md#rust-实现)。

实现 `ting_plugin_sdk::Plugin` 并调用 `ting_plugin_sdk::export_plugin!(MyPlugin)`。`Plugin::OPERATIONS` 必须覆盖清单声明的每一个操作；宿主加载时验证 ABI revision、操作导出和调用缓冲上限。开发者的 `invoke(operation, input, host)` 实现业务逻辑，宏负责导出 `ting_abi_revision`、`supports`、`alloc/dealloc`、`invoke` 和生命周期。Host 访问通过 SDK 的 `Host`、`host_call`、`http_request_response`、`read_exact_range` 以及资源方法完成；响应体与媒体字节由宿主资源句柄管理。

元数据 `search` 输入是 `SearchRequest`，输出由 `ting_scraper_sdk::publish_search_for_request(&input, raw)` 转换成 `SearchPage`。字段、可空值和限额见[元数据搜索接口](./capabilities.md#元数据搜索接口)。扩展格式插件可以声明文件扩展名，并提供 `probe`、`extract_metadata` 和可选的 staging 写回；只声明已实现的操作。其他能力和固定操作见[能力声明](./capabilities.md)。

WASM 操作的结果信封由 SDK `dispatch` 生成。插件所请求的网络、数据和文件权限在清单中声明，宿主每次访问资源时再依据实例、用户和授权范围校验。操作名称来自能力定义的固定操作集合，调用参数和结果使用 SDK 提供的结构化类型。

宿主通过 `plugins.max_memory_per_plugin`（字节）和 `plugins.max_execution_time`（秒）限制 WASM 实例内存与单次执行时间。实例创建时执行的插件代码、初始化、业务调用和关闭均有执行期限；执行中的 WASM 会定期让出控制权，使纯计算或死循环也能超时退出。超时或运行时错误会终止该实例的调用、清理宿主上下文并取消关联资源；重新加载插件后可创建新实例。其他插件和主程序继续运行。
