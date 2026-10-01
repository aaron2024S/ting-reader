# Native 插件运行时

Native 插件按服务器操作系统和 CPU 架构分别发布。创建项目并编译：

```sh
trpack new special-format --template format --runtime native --id special-format
cd special-format
cargo build --release
```

Windows 将 `target/release/ting_plugin.dll` 放到清单声明的入口；Linux/macOS 分别使用 `.so` / `.dylib` 并更新入口名。然后运行 `trpack validate`、`trpack build --sign-key keys/private.json` 和 `trpack verify`。Native 插件在服务端进程内运行，应只安装可信发行者的已签名二进制。

在 `src/lib.rs` 实现业务，完整调用示例见 [Rust 实现](./plugin-dev.md#rust-实现)，打包与安装步骤见[开发指南](./plugin-dev.md#6-签名打包和检查)。

## 声明式扩展格式

```yaml
id: special-format
name: Special Format
version: 2.0.0
min_core_version: 2.0.0
author: Example
description: { zh: 特殊格式, en: Special format }
runtime: native
entry_point: ting_plugin.dll
capabilities:
  - id: format.handler
    kind: format_handler
    extensions: [example]
    operations: [probe, extract_metadata]
permissions: []
```

先用 `probe` 确认格式，再通过宿主资源 ID 读取有限前缀并返回 `FormatMetadata`。可选 `write_metadata` 写入宿主创建的 staging 输出，成功并经校验后由宿主提交，插件不得直接改原文件。主项目仅包含通用路由、资源约束和 DTO，格式解析与元数据规则由扩展插件维护。

Rust 插件实现 SDK `Plugin`，在 `OPERATIONS` 列出所有声明的操作，以 `ting_plugin_sdk::export_plugin!(SpecialPlugin)` 生成 `ting_plugin_abi_v2`。调用输入与输出使用 SDK 导出的 `FormatCall`、`FormatOutput` 和结果信封；格式调用类型位于 `ting_plugin_sdk::contract::format_calls`。Host 的文件、资源和 HTTP 能力从 `&dyn Host` 访问。ABI revision、目标架构、导出声明以及宿主分配控制缓冲区在加载时检查；同进程私有堆无法按插件精确计量，托管缓冲区受限额和作用域管理。

独立 Native 插件同样可以声明其他八类能力，见[能力声明](./capabilities.md)。插件通过固定的能力操作、结构化结果信封和 Host 资源接口与宿主通信；清单中的操作名称必须来自能力定义的固定操作集合。
